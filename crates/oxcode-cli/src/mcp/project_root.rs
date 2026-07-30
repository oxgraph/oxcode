//! Resolve the project root for omitted MCP `path` arguments.
//!
//! MCP hosts often start the server process with `cwd=$HOME` even when the
//! agent is in a project folder. Prefer a live MCP roots cache (and then
//! host-injected env snapshots) over process cwd, and refuse `$HOME` unless
//! `path` was explicit.

use std::path::{Path, PathBuf};

use rmcp::{ErrorData as McpError, schemars};
use serde::Deserialize;

/// Shared optional `path` field for every tool that accepts a project root.
///
/// Flattened into each tool's params struct so the wire shape stays `path?`
/// while the defaulting docs live in one place.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct OptionalProjectRoot {
    /// Project root; defaults to the workspace (`OXCODE_ROOT` / MCP roots /
    /// `CLAUDE_PROJECT_DIR` / `WORKSPACE_FOLDER_PATHS`), never silently to
    /// `$HOME`.
    pub path: Option<String>,
}

/// Resolves an optional tool `path` against env defaults and a ready MCP root.
///
/// Order for omitted `path`: `OXCODE_ROOT` (explicit pin) → `mcp_root` (live
/// client roots) → host env snapshots (`CLAUDE_PROJECT_DIR` /
/// `WORKSPACE_FOLDER_PATHS`) → process cwd. Host env sits below MCP roots
/// because those vars are fixed at process start while roots refresh on
/// `roots/list_changed`.
///
/// When `allow_host_env` is false (roots refresh timed out in flight), host
/// snapshots are skipped so a sticky pre-switch env cannot win over an
/// in-progress roots update.
///
/// `mcp_root` is the first client workspace root already fetched outside the
/// tool handler (see [`super::roots::RootsCache`]); this never calls
/// `roots/list`.
pub(crate) fn resolve_project_root(
    path: Option<String>,
    mcp_root: Option<PathBuf>,
    allow_host_env: bool,
) -> Result<PathBuf, McpError> {
    let explicit = path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let raw = if let Some(explicit_path) = explicit.clone() {
        explicit_path
    } else if let Some(pin) = oxcode_root_override() {
        pin
    } else if let Some(from_roots) = mcp_root {
        from_roots
    } else if allow_host_env {
        if let Some(from_host) = host_env_project_root() {
            from_host
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
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

/// Explicit user/config pin. Wins over live MCP roots and host env snapshots.
pub(crate) fn oxcode_root_override() -> Option<PathBuf> {
    non_empty_env("OXCODE_ROOT")
}

/// Host-injected workspace snapshots (Claude Code / Cursor).
///
/// These are set at process start and typically do not update when the client
/// switches folders via `roots/list_changed`, so callers should prefer a ready
/// MCP root when one is available.
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

/// Converts a `file://` MCP root URI into a filesystem path.
pub(crate) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if let Some(path) = rest.strip_prefix("localhost") {
        path
    } else if rest.starts_with('/') {
        rest
    } else {
        return None;
    };
    let decoded = percent_decode(path);
    if decoded.is_empty() {
        return None;
    }
    // `file:///C:/Users/...` percent-decodes to `/C:/Users/...`. On Windows the
    // usable path is `C:/Users/...` — same as `Url::to_file_path`.
    let normalized = match decoded.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => decoded[1..].to_owned(),
        _ => decoded,
    };
    Some(PathBuf::from(normalized))
}

/// Decodes `%XX` sequences in a URI path; returns the input unchanged when none.
fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_owned();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            out.push((high << 4) | low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
fn restore_env(key: &str, previous: Option<std::ffi::OsString>) {
    // SAFETY: caller restores test-local env mutations.
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
    fn resolve_project_root_prefers_mcp_roots_over_host_env() {
        let host = tempfile::TempDir::new().expect("host");
        let live = tempfile::TempDir::new().expect("live");
        let previous_claude = std::env::var_os("CLAUDE_PROJECT_DIR");
        let previous_oxcode = std::env::var_os("OXCODE_ROOT");
        let previous_workspace = std::env::var_os("WORKSPACE_FOLDER_PATHS");
        // SAFETY: test-local env mutation; restored before return.
        unsafe {
            std::env::set_var("CLAUDE_PROJECT_DIR", host.path());
            std::env::remove_var("OXCODE_ROOT");
            std::env::remove_var("WORKSPACE_FOLDER_PATHS");
        }
        let with_roots = resolve_project_root(None, Some(live.path().to_path_buf()), true)
            .expect("mcp roots win");
        assert_eq!(
            with_roots,
            canonicalize_root(live.path().to_path_buf()),
            "live MCP roots must beat sticky CLAUDE_PROJECT_DIR"
        );
        let without_roots = resolve_project_root(None, None, true).expect("host env fallback");
        assert_eq!(
            without_roots,
            canonicalize_root(host.path().to_path_buf()),
            "host env is used only when MCP roots are absent"
        );
        let suppressed = resolve_project_root(None, None, false).expect("no host env");
        assert_ne!(
            suppressed,
            canonicalize_root(host.path().to_path_buf()),
            "refresh-in-flight must not fall back to sticky host env"
        );
        restore_env("CLAUDE_PROJECT_DIR", previous_claude);
        restore_env("OXCODE_ROOT", previous_oxcode);
        restore_env("WORKSPACE_FOLDER_PATHS", previous_workspace);
    }

    #[test]
    fn resolve_project_root_oxcode_root_pins_over_mcp_roots() {
        let pin = tempfile::TempDir::new().expect("pin");
        let live = tempfile::TempDir::new().expect("live");
        let previous = std::env::var_os("OXCODE_ROOT");
        // SAFETY: test-local env mutation; restored before return.
        unsafe {
            std::env::set_var("OXCODE_ROOT", pin.path());
        }
        let resolved =
            resolve_project_root(None, Some(live.path().to_path_buf()), true).expect("pin");
        assert_eq!(
            resolved,
            canonicalize_root(pin.path().to_path_buf()),
            "OXCODE_ROOT is an explicit pin above live MCP roots"
        );
        restore_env("OXCODE_ROOT", previous);
    }

    #[test]
    fn file_uri_to_path_decodes_file_roots() {
        assert_eq!(
            file_uri_to_path("file:///Users/snowmead/opt/jinttai"),
            Some(PathBuf::from("/Users/snowmead/opt/jinttai"))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/project%20name"),
            Some(PathBuf::from("/tmp/project name"))
        );
        assert_eq!(
            file_uri_to_path("file:///C:/Users/snowmead/opt/jinttai"),
            Some(PathBuf::from("C:/Users/snowmead/opt/jinttai"))
        );
        assert_eq!(file_uri_to_path("https://example.com"), None);
    }

    #[test]
    fn is_home_directory_matches_home_env() {
        let home = tempfile::TempDir::new().expect("home");
        let previous_home = std::env::var_os("HOME");
        // SAFETY: test-local HOME mutation; restored before return.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        assert!(is_home_directory(home.path()));
        assert!(!is_home_directory(&home.path().join("opt/jinttai")));
        // SAFETY: restore prior HOME.
        unsafe {
            match previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
