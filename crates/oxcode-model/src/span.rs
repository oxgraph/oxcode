//! Source spans and locations.

use serde::{Deserialize, Serialize};

use crate::SourcePath;

/// A byte and line span in a source file.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SourceSpan {
    /// Inclusive start byte.
    pub start_byte: usize,
    /// Exclusive end byte.
    pub end_byte: usize,
    /// One-based start line.
    pub start_line: usize,
    /// Zero-based start column.
    pub start_column: usize,
    /// One-based end line.
    pub end_line: usize,
    /// Zero-based end column.
    pub end_column: usize,
}

impl SourceSpan {
    /// Returns the byte width of the span.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Returns whether `line` falls within the inclusive line range.
    #[must_use]
    pub const fn contains_line(self, line: usize) -> bool {
        self.start_line <= line && line <= self.end_line
    }
}

/// Compact source location shown in agent-facing reports: a file path plus a
/// span, composing [`SourceSpan`] rather than restating its fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeLocation {
    /// Repository-relative file path.
    pub file_path: SourcePath,
    /// Byte and line span within the file.
    pub span: SourceSpan,
}

impl CodeLocation {
    /// Creates a location from a path and span.
    #[must_use]
    pub fn new(file_path: impl Into<SourcePath>, span: SourceSpan) -> Self {
        Self {
            file_path: file_path.into(),
            span,
        }
    }
}
