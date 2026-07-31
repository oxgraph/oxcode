//! Resolve the project root for omitted MCP `path` arguments.
//!
//! MCP hosts often start the server process with `cwd=$HOME` even when the
//! agent is in a project folder. Prefer host-injected env snapshots over
//! process cwd, and refuse `$HOME` unless `path` was explicit.

use std::path::{Path, PathBuf};

use rmcp::{ErrorData as McpError, schemars};
use serde::Deserialize;

/// Shared optional `path` field for every tool that accepts a project root.
///
/// Flattened into each tool's params struct so the wire shape stays `path?`
/// while the defaulting docs live in one place.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct OptionalProjectRoot {
    /// Project root; defaults to the workspace (`OXCODE_ROOT` /
    /// `CLAUDE_PROJECT_DIR` / `WORKSPACE_FOLDER_PATHS`), never silently to
    /// `$HOME`.
    pub path: Option<String>,
}

/// Resolves an optional tool `path` against env defaults.
///
/// Order for omitted `path`: explicit `path` → `OXCODE_ROOT` (explicit pin) →
/// host env snapshots (`CLAUDE_PROJECT_DIR` / `WORKSPACE_FOLDER_PATHS`) →
/// process cwd.
pub(crate) fn resolve_project_root(path: Option<String>) -> Result<PathBuf, McpError> {
    let explicit = path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let raw = if let Some(explicit_path) = explicit.clone() {
        explicit_path
    } else if let Some(pin) = oxcode_root_override() {
        pin
    } else if let Some(from_host) = host_env_project_root() {
        from_host
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let root = canonicalize_root(raw);
    if explicit.is_none() && is_home_directory(&root) {
        return Err(McpError::invalid_params(
            format!(
                "refusing to use home directory {} as the project root — MCP hosts often \
                 start this server with cwd=$HOME even when your workspace is elsewhere. \
                 Pass `path` (the project folder), or set OXCODE_ROOT / CLAUDE_PROJECT_DIR \
                 / WORKSPACE_FOLDER_PATHS.",
                root.display()
            ),
            None,
        ));
    }
    Ok(root)
}

/// Explicit user/config pin. Wins over host env snapshots.
pub(crate) fn oxcode_root_override() -> Option<PathBuf> {
    non_empty_env("OXCODE_ROOT")
}

/// Host-injected workspace snapshots (Claude Code / Cursor).
pub(crate) fn host_env_project_root() -> Option<PathBuf> {
    if let Some(path) = non_empty_env("CLAUDE_PROJECT_DIR") {
        return Some(path);
    }
    std::env::var("WORKSPACE_FOLDER_PATHS")
        .ok()
        .and_then(|value| first_workspace_folder(&value))
}

fn non_empty_env(key: &str) -> Option<PathBuf> {
    std::env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    })
}

/// First folder from a `WORKSPACE_FOLDER_PATHS` value (single path or CSV).
pub(crate) fn first_workspace_folder(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let as_path = PathBuf::from(trimmed);
    if as_path.is_dir() {
        return Some(as_path);
    }
    // Multi-root workspaces: comma-separated. Do not split on `:` — that breaks
    // Windows drive letters (`C:\...`).
    trimmed
        .split(',')
        .map(str::trim)
        .find(|part| !part.is_empty())
        .map(PathBuf::from)
}

/// Whether `path` is the current user's home directory (best-effort).
pub(crate) fn is_home_directory(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return false;
    };
    let home = PathBuf::from(home);
    canonicalize_root(path.to_path_buf()) == canonicalize_root(home)
}

/// Canonicalizes best-effort so reader cache / writer registry / lock file key on
/// the same absolute path (FS events report canonical paths). Falls back to the
/// raw path when it does not exist yet.
pub(crate) fn canonicalize_root(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// Serializes tests that mutate project-root / HOME env vars.
#[cfg(test)]
pub(crate) static PROJECT_ROOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn restore_env(key: &str, previous: Option<std::ffi::OsString>) {
    // SAFETY: caller holds [`PROJECT_ROOT_ENV_LOCK`].
    unsafe {
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_workspace_folder_accepts_csv_and_single_path() {
        let project = tempfile::TempDir::new().expect("temp");
        let path = project.path().to_string_lossy().into_owned();
        assert_eq!(
            first_workspace_folder(&path),
            Some(PathBuf::from(&path)),
            "existing single path wins without splitting"
        );
        assert_eq!(
            first_workspace_folder(&format!("{path},/does/not/exist")),
            Some(PathBuf::from(&path))
        );
        assert_eq!(
            first_workspace_folder("/missing/a,/missing/b"),
            Some(PathBuf::from("/missing/a"))
        );
    }

    #[test]
    fn resolve_project_root_uses_host_env_when_no_explicit_path_or_oxcode_root() {
        let _lock = PROJECT_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let host = tempfile::TempDir::new().expect("host");
        let previous_claude = std::env::var_os("CLAUDE_PROJECT_DIR");
        let previous_oxcode = std::env::var_os("OXCODE_ROOT");
        let previous_workspace = std::env::var_os("WORKSPACE_FOLDER_PATHS");
        // SAFETY: held exclusively via PROJECT_ROOT_ENV_LOCK.
        unsafe {
            std::env::set_var("CLAUDE_PROJECT_DIR", host.path());
            std::env::remove_var("OXCODE_ROOT");
            std::env::remove_var("WORKSPACE_FOLDER_PATHS");
        }
        let resolved = resolve_project_root(None).expect("host env fallback");
        assert_eq!(
            resolved,
            canonicalize_root(host.path().to_path_buf()),
            "host env is used when no explicit path or OXCODE_ROOT"
        );
        restore_env("CLAUDE_PROJECT_DIR", previous_claude);
        restore_env("OXCODE_ROOT", previous_oxcode);
        restore_env("WORKSPACE_FOLDER_PATHS", previous_workspace);
    }

    #[test]
    fn resolve_project_root_oxcode_root_pins_over_host_env() {
        let _lock = PROJECT_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pin = tempfile::TempDir::new().expect("pin");
        let host = tempfile::TempDir::new().expect("host");
        let previous_oxcode = std::env::var_os("OXCODE_ROOT");
        let previous_claude = std::env::var_os("CLAUDE_PROJECT_DIR");
        // SAFETY: held exclusively via PROJECT_ROOT_ENV_LOCK.
        unsafe {
            std::env::set_var("OXCODE_ROOT", pin.path());
            std::env::set_var("CLAUDE_PROJECT_DIR", host.path());
        }
        let resolved = resolve_project_root(None).expect("pin");
        assert_eq!(
            resolved,
            canonicalize_root(pin.path().to_path_buf()),
            "OXCODE_ROOT is an explicit pin above host env"
        );
        restore_env("OXCODE_ROOT", previous_oxcode);
        restore_env("CLAUDE_PROJECT_DIR", previous_claude);
    }

    #[test]
    fn is_home_directory_matches_home_env() {
        let _lock = PROJECT_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::TempDir::new().expect("home");
        let previous_home = std::env::var_os("HOME");
        // SAFETY: held exclusively via PROJECT_ROOT_ENV_LOCK.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        assert!(is_home_directory(home.path()));
        assert!(!is_home_directory(&home.path().join("opt/jinttai")));
        restore_env("HOME", previous_home);
    }
}
