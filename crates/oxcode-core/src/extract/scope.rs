//! Per-language module-scope strategies.
//!
//! A file's *base scope* is the `::`-segmented prefix that anchors every
//! qualified name it produces (e.g. `[crate, ..modules]` for Rust). Each
//! language answers this differently — crate + modules for Rust, package path
//! for Go, file path for JS/TS — so the rule is captured behind
//! [`ScopeStrategy`] rather than hard-coded into one extractor.

use std::path::{Path, PathBuf};

use crate::extract::cargo;

/// Computes the module-scope prefix that anchors qualified names in a file.
pub(crate) trait ScopeStrategy: Send + Sync {
    /// Returns the base scope segments for the file at `absolute_path`
    /// (`relative_path` is the repository-relative spelling used as a fallback).
    fn base_scope(&self, absolute_path: &Path, relative_path: &str) -> Vec<String>;
}

/// Selects a [`ScopeStrategy`] for a generic (query-driven) language profile.
#[derive(Clone, Copy)]
pub(crate) enum ScopeKind {
    /// The file path as scope segments, extension stripped.
    FileStem,
    /// Python dotted package path via `__init__.py`, plus the module name.
    PythonPackage,
}

/// Returns the scope strategy for a profile's [`ScopeKind`].
pub(crate) fn strategy_for(kind: ScopeKind) -> &'static dyn ScopeStrategy {
    match kind {
        ScopeKind::FileStem => &FileStemScope,
        ScopeKind::PythonPackage => &PythonScope,
    }
}

/// Rust crate-qualified module scope (`[crate_name, ..module_segments]`).
pub(crate) struct RustScope;

impl ScopeStrategy for RustScope {
    fn base_scope(&self, absolute_path: &Path, relative_path: &str) -> Vec<String> {
        cargo::crate_module_scope(absolute_path, relative_path)
    }
}

/// Go package scope: the package's import path (`module_path/dir`) split on `/`.
///
/// All files in a directory share one package namespace, so — unlike Rust — the
/// file name contributes no segment. With no `go.mod`, the directory path
/// relative to the project root is used.
pub(crate) struct GoScope;

impl ScopeStrategy for GoScope {
    fn base_scope(&self, absolute_path: &Path, relative_path: &str) -> Vec<String> {
        if let Some((module_path, module_dir)) = nearest_go_module(absolute_path) {
            let mut segments = split_path(&module_path);
            if let Some(directory) = absolute_path.parent()
                && let Ok(relative) = directory.strip_prefix(&module_dir)
            {
                segments.extend(path_components(relative));
            }
            return segments;
        }
        // Fallback: the file's directory segments relative to the project root.
        Path::new(relative_path)
            .parent()
            .map(path_components)
            .unwrap_or_default()
    }
}

/// JS/TS module scope: the file path as `::` segments, extension stripped.
///
/// ES module identity is the file path, so `src/foo/bar.ts` anchors at
/// `[src, foo, bar]`. An `index.{js,ts}` file collapses to its directory (like
/// Rust's `mod.rs`) so a directory import resolves to it.
pub(crate) struct JsTsScope;

impl ScopeStrategy for JsTsScope {
    fn base_scope(&self, _absolute_path: &Path, relative_path: &str) -> Vec<String> {
        let path = Path::new(relative_path);
        let mut segments = path.parent().map(path_components).unwrap_or_default();
        if let Some(stem) = module_stem(path) {
            segments.push(stem);
        }
        segments
    }
}

/// Returns a file's module stem (file name without extension), or `None` for an
/// `index` file, which contributes no segment.
fn module_stem(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().to_string();
    (stem != "index").then_some(stem)
}

/// Generic fallback scope: the file path as segments, extension stripped.
pub(crate) struct FileStemScope;

impl ScopeStrategy for FileStemScope {
    fn base_scope(&self, _absolute_path: &Path, relative_path: &str) -> Vec<String> {
        let path = Path::new(relative_path);
        let mut segments = path.parent().map(path_components).unwrap_or_default();
        if let Some(stem) = path.file_stem() {
            segments.push(stem.to_string_lossy().to_string());
        }
        segments
    }
}

/// Python package scope: the dotted package path discovered by walking up while
/// an `__init__.py` exists, plus the module's own name (an `__init__.py` file
/// contributes only its package).
pub(crate) struct PythonScope;

impl ScopeStrategy for PythonScope {
    fn base_scope(&self, absolute_path: &Path, relative_path: &str) -> Vec<String> {
        let mut packages = Vec::new();
        let mut current = absolute_path.parent();
        while let Some(directory) = current {
            if !directory.join("__init__.py").is_file() {
                break;
            }
            if let Some(name) = directory.file_name() {
                packages.push(name.to_string_lossy().to_string());
            }
            current = directory.parent();
        }
        packages.reverse();
        if let Some(stem) = Path::new(relative_path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            && stem != "__init__"
        {
            packages.push(stem);
        }
        packages
    }
}

/// Walks up from a file to the nearest `go.mod`, returning the declared module
/// path and the directory that holds the manifest.
fn nearest_go_module(file: &Path) -> Option<(String, PathBuf)> {
    let mut current = file.parent();
    while let Some(directory) = current {
        let manifest = directory.join("go.mod");
        if manifest.is_file()
            && let Some(module_path) = module_path(&manifest)
        {
            return Some((module_path, directory.to_path_buf()));
        }
        current = directory.parent();
    }
    None
}

/// Reads the `module <path>` declaration from a `go.mod` file.
fn module_path(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("module ")
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
    })
}

/// Splits a slash-separated import path into non-empty segments.
fn split_path(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// Returns a relative path's components as strings, dropping `.`/`..`.
fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}
