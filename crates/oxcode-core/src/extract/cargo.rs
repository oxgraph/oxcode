//! Cargo crate discovery and Rust module-scope mapping.
//!
//! Qualified names are anchored at the crate: a file's scope is
//! `[crate_name, ..module_segments]`, so symbols in different workspace crates
//! never collide and `crate::`-relative references resolve against real names.

use std::path::{Path, PathBuf};

/// Returns the crate-qualified module scope for a Rust source file.
///
/// `absolute_path` is the on-disk path; `project_relative` is the
/// repository-root-relative path used as a fallback when no `Cargo.toml` is
/// found. Examples (package `foo-bar`):
/// `crates/foo-bar/src/a/b.rs` -> `["foo_bar", "a", "b"]`;
/// `crates/foo-bar/src/lib.rs` -> `["foo_bar"]`. With no manifest, the crate
/// name falls back to the literal `crate`.
pub(crate) fn crate_module_scope(absolute_path: &Path, project_relative: &str) -> Vec<String> {
    let (crate_name, crate_root) = nearest_crate(absolute_path);
    let relative = crate_relative_path(absolute_path, crate_root.as_deref())
        .unwrap_or_else(|| project_relative.to_string());
    let mut scope = vec![crate_name];
    scope.extend(module_segments_from_relative(&relative));
    scope
}

/// Maps a crate- (or project-) relative path to Rust module segments.
///
/// A leading `src/` is stripped; `lib.rs`/`main.rs`/`mod.rs` contribute no
/// segment; any other `foo.rs` contributes `foo`.
fn module_segments_from_relative(relative: &str) -> Vec<String> {
    let mut parts = Path::new(relative)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|part| part == "src") {
        parts.remove(0);
    }
    let Some(file) = parts.pop() else {
        return Vec::new();
    };
    match file.as_str() {
        "lib.rs" | "main.rs" | "mod.rs" => {}
        other => parts.push(other.trim_end_matches(".rs").to_string()),
    }
    parts
}

/// Walks up from a file to the nearest `Cargo.toml`, returning the normalized
/// crate name and the directory that holds the manifest.
fn nearest_crate(file: &Path) -> (String, Option<PathBuf>) {
    let mut current = file.parent();
    while let Some(directory) = current {
        let manifest = directory.join("Cargo.toml");
        if manifest.is_file() {
            let name = package_name(&manifest)
                .map_or_else(|| "crate".to_string(), |name| normalize_crate_name(&name));
            return (name, Some(directory.to_path_buf()));
        }
        current = directory.parent();
    }
    ("crate".to_string(), None)
}

/// Reads `[package].name` from a manifest, tolerating virtual workspaces.
fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let value = text.parse::<toml::Value>().ok()?;
    value
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// Normalizes a Cargo package name to its Rust crate identifier (`-` -> `_`).
fn normalize_crate_name(name: &str) -> String {
    name.trim().replace('-', "_")
}

/// Returns the file path relative to the crate's `src/` directory (or crate
/// root), using forward separators.
fn crate_relative_path(file: &Path, crate_root: Option<&Path>) -> Option<String> {
    let root = crate_root?;
    let relative = file
        .strip_prefix(root.join("src"))
        .or_else(|_| file.strip_prefix(root))
        .ok()?;
    Some(
        relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn module_segments_skip_crate_roots() {
        assert_eq!(
            module_segments_from_relative("src/lib.rs"),
            Vec::<String>::new()
        );
        assert_eq!(
            module_segments_from_relative("src/main.rs"),
            Vec::<String>::new()
        );
        assert_eq!(
            module_segments_from_relative("src/graph/mod.rs"),
            vec!["graph".to_string()]
        );
        assert_eq!(
            module_segments_from_relative("src/graph/query.rs"),
            vec!["graph".to_string(), "query".to_string()]
        );
    }

    #[test]
    fn crate_scope_uses_normalized_package_name_and_src_relative_module() {
        let temp = TempDir::new().expect("temp");
        let crate_dir = temp.path().join("crates").join("foo-bar");
        fs::create_dir_all(crate_dir.join("src").join("a")).expect("dirs");
        fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"foo-bar\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        let file = crate_dir.join("src").join("a").join("b.rs");
        fs::write(&file, "fn x() {}").expect("file");

        assert_eq!(
            crate_module_scope(&file, "crates/foo-bar/src/a/b.rs"),
            vec!["foo_bar".to_string(), "a".to_string(), "b".to_string()]
        );

        let lib = crate_dir.join("src").join("lib.rs");
        fs::write(&lib, "").expect("lib");
        assert_eq!(
            crate_module_scope(&lib, "crates/foo-bar/src/lib.rs"),
            vec!["foo_bar".to_string()]
        );
    }

    #[test]
    fn crate_scope_falls_back_to_literal_crate_without_manifest() {
        let temp = TempDir::new().expect("temp");
        let file = temp.path().join("src").join("lib.rs");
        fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
        fs::write(&file, "").expect("file");
        assert_eq!(
            crate_module_scope(&file, "src/lib.rs"),
            vec!["crate".to_string()]
        );
    }
}
