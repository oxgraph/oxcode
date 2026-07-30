//! Resolve the project root for omitted MCP `path` arguments.
//!
//! MCP hosts often start the server process with `cwd=$HOME` even when the
//! agent is in a project folder. Prefer host-injected env vars and parsed
//! `file://` MCP roots over process cwd, and refuse `$HOME` unless `path` was
//! explicit.

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
    /// `CLAUDE_PROJECT_DIR` / `WORKSPACE_FOLDER_PATHS` / MCP roots), never
    /// silently to `$HOME`.
    pub path: Option<String>,
}

/// Resolves an optional tool `path` against env defaults and a ready MCP root.
///
/// `mcp_root` is the first client workspace root already fetched outside the
/// tool handler (see [`super::roots::RootsCache`]); this never calls
/// `roots/list`.
pub(crate) fn resolve_project_root(
    path: Option<String>,
    mcp_root: Option<PathBuf>,
) -> Result<PathBuf, McpError> {
    let explicit = path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let raw = if let Some(explicit_path) = explicit.clone() {
        explicit_path
    } else if let Some(from_env) = env_project_root() {
        from_env
    } else if let Some(from_roots) = mcp_root {
        from_roots
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

/// Project root from host-injected environment variables, in priority order.
///
/// MCP hosts frequently leave the server process cwd at `$HOME` while advertising
/// the real workspace via these variables (Claude Code → `CLAUDE_PROJECT_DIR`,
/// Cursor → `WORKSPACE_FOLDER_PATHS`). `OXCODE_ROOT` is the explicit override.
pub(crate) fn env_project_root() -> Option<PathBuf> {
    for key in ["OXCODE_ROOT", "CLAUDE_PROJECT_DIR"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    std::env::var("WORKSPACE_FOLDER_PATHS")
        .ok()
        .and_then(|value| first_workspace_folder(&value))
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
