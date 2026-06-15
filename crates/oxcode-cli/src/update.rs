//! Self-update for the `oxcode` binary.
//!
//! The MCP server is whatever `oxcode` is on `PATH`, so a stale binary silently
//! lacks the newest tools (this is how a user ended up without `oxcode_index`).
//! To keep the plugin current, `oxcode mcp` checks for a newer GitHub release on
//! startup and, when one exists, installs it and re-execs into it before serving.
//! `oxcode update` performs the same update explicitly.
//!
//! stdout is the MCP JSON-RPC transport, so nothing here may write to it: the
//! installer's stdout is disabled, the check is time-bounded, and all human
//! output goes to stderr.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use axoupdater::{AxoUpdater, ReleaseSource, ReleaseSourceType, Version};

/// GitHub owner of the oxcode releases.
const RELEASE_OWNER: &str = "oxgraph";
/// GitHub repository holding the oxcode releases.
const RELEASE_REPO: &str = "oxcode";
/// dist "app name": the crates.io package, which also names the release installer
/// asset (`oxcode-cli-installer.sh`) and the install-dir env var (`OXCODE_CLI_*`).
const RELEASE_APP: &str = "oxcode-cli";
/// Setting this disables the startup auto-update (CI, offline, reproducible runs).
const DISABLE_ENV: &str = "OXCODE_NO_AUTO_UPDATE";
/// Set on the re-exec'd process so it serves immediately instead of re-checking,
/// which would otherwise risk an update -> re-exec loop.
const REEXEC_GUARD_ENV: &str = "OXCODE_UPDATED_REEXEC";
/// Env var the dist installer reads to skip rewriting shell rc files.
const NO_MODIFY_PATH_ENV: &str = "OXCODE_CLI_NO_MODIFY_PATH";
/// Upper bound on the startup version check, so a slow network can't stall the
/// MCP handshake.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Builds an updater that reconciles this binary against the latest GitHub
/// release, installing in place over the running executable.
fn updater() -> Result<AxoUpdater> {
    let mut updater = AxoUpdater::new_for(RELEASE_APP);
    updater.set_release_source(ReleaseSource {
        release_type: ReleaseSourceType::GitHub,
        owner: RELEASE_OWNER.to_owned(),
        name: RELEASE_REPO.to_owned(),
        app_name: RELEASE_APP.to_owned(),
    });
    let version: Version = env!("CARGO_PKG_VERSION")
        .parse()
        .with_context(|| format!("parsing crate version {:?}", env!("CARGO_PKG_VERSION")))?;
    updater.set_current_version(version)?;
    // stdout carries MCP JSON-RPC: never let the installer write to it.
    updater.disable_installer_stdout();
    // Install over the running binary's own location, regardless of how it was
    // first installed (cargo install, binstall, or the dist installer).
    if let Some(prefix) = install_prefix() {
        updater.set_install_dir(prefix);
    }
    if let Some(token) = github_token() {
        updater.set_github_token(&token);
    }
    Ok(updater)
}

/// Install prefix to force, as UTF-8, or `None` if undeterminable.
///
/// The dist installer places binaries in `<prefix>/bin`, so when the running
/// executable already lives in a `bin/` directory we hand it the parent — the
/// binary then lands back in the same place instead of a nested `bin/bin`.
fn install_prefix() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let bin_dir = exe.parent()?;
    let prefix = if bin_dir.file_name().and_then(|name| name.to_str()) == Some("bin") {
        bin_dir.parent()?
    } else {
        bin_dir
    };
    Some(prefix.to_str()?.to_owned())
}

/// A GitHub token from the environment, to dodge unauthenticated rate limits.
fn github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
}

/// Startup hook for `oxcode mcp`: if a newer release exists, install it and
/// re-exec into it before serving. Best-effort — any failure logs to stderr and
/// falls through to serve the current binary; never writes to stdout.
pub(crate) fn auto_update_and_reexec() {
    if std::env::var_os(DISABLE_ENV).is_some() || std::env::var_os(REEXEC_GUARD_ENV).is_some() {
        return;
    }
    match check_for_update() {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            eprintln!("oxcode: update check skipped ({error})");
            return;
        }
    }
    match install_in_child() {
        Ok(true) => {
            if let Err(error) = reexec() {
                eprintln!("oxcode: staying on current version (re-exec failed: {error})");
            }
        }
        Ok(false) => eprintln!("oxcode: update did not complete; staying on current version"),
        Err(error) => eprintln!("oxcode: auto-update skipped ({error})"),
    }
}

/// Whether a newer release exists, bounded by [`CHECK_TIMEOUT`]. The blocking
/// network probe runs on a worker thread so a hung connection can't stall
/// startup; on timeout the orphaned thread is harmless and we report no update.
fn check_for_update() -> Result<bool> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(check_now());
    });
    match rx.recv_timeout(CHECK_TIMEOUT) {
        Ok(result) => result,
        Err(_) => Err(anyhow!("timed out after {}s", CHECK_TIMEOUT.as_secs())),
    }
}

/// The blocking "is a newer release available?" probe (no stdout output).
fn check_now() -> Result<bool> {
    let mut updater = updater()?;
    updater
        .is_update_needed_sync()
        .map_err(|error| anyhow!("{error}"))
}

/// Runs the install as a child `oxcode update`, keeping this process's stdout
/// (the MCP pipe) untouched and suppressing installer PATH edits. Returns
/// whether the child exited successfully.
fn install_in_child() -> Result<bool> {
    let exe = std::env::current_exe().context("locating current executable")?;
    let status = Command::new(exe)
        .arg("update")
        .env(NO_MODIFY_PATH_ENV, "1")
        .stdout(Stdio::null())
        .status()
        .context("running update child")?;
    Ok(status.success())
}

/// Replaces this process with a fresh `oxcode mcp` (same args) on the updated
/// binary, with the guard set so it serves without re-checking.
fn reexec() -> Result<()> {
    let exe = std::env::current_exe().context("locating current executable")?;
    let mut command = Command::new(exe);
    command.args(std::env::args_os().skip(1));
    command.env(REEXEC_GUARD_ENV, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` only returns if it failed to replace the process image.
        Err(anyhow!("exec failed: {}", command.exec()))
    }
    #[cfg(not(unix))]
    {
        let status = command.status().context("spawning updated binary")?;
        std::process::exit(status.code().unwrap_or(0));
    }
}

/// Updates the binary to the latest GitHub release in place, reporting to stderr
/// (stdout is reserved for the MCP transport). Backs both `oxcode update` and
/// the auto-update child process.
///
/// The dist installer reads [`NO_MODIFY_PATH_ENV`] from the environment to skip
/// rewriting shell rc/`env` files (which would otherwise clobber e.g.
/// `~/.cargo/env`). Setting process env needs `unsafe` on edition 2024, so when
/// the var is absent we re-spawn ourselves once with it set and do the real
/// install in that child. The auto-update path already spawns us with it set.
pub(crate) fn update_command() -> Result<()> {
    if std::env::var_os(NO_MODIFY_PATH_ENV).is_none() {
        let exe = std::env::current_exe().context("locating current executable")?;
        let status = Command::new(exe)
            .arg("update")
            .env(NO_MODIFY_PATH_ENV, "1")
            .status()
            .context("running update worker")?;
        std::process::exit(status.code().unwrap_or(1));
    }
    install()
}

/// Performs the in-place install. Must run with [`NO_MODIFY_PATH_ENV`] set; see
/// [`update_command`].
fn install() -> Result<()> {
    let mut updater = updater()?;
    match updater.run_sync().map_err(|error| anyhow!("{error}"))? {
        Some(result) => {
            let from = result
                .old_version
                .map_or_else(|| "?".to_owned(), |version| version.to_string());
            eprintln!(
                "oxcode: updated {from} -> {} ({})",
                result.new_version, result.new_version_tag
            );
        }
        None => eprintln!("oxcode: already on the latest version"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_builds_with_valid_config() {
        // Parses the compiled-in version and assembles the release source; no
        // network. Guards against an API drift or an unparseable version.
        assert!(updater().is_ok());
    }

    #[test]
    fn install_prefix_is_resolvable() {
        assert!(install_prefix().is_some());
    }
}
