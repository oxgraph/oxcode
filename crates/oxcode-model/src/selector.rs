//! The single selector grammar accepted by agent navigation.

use serde::{Deserialize, Serialize};

use crate::{QualifiedName, SourcePath, SymbolId};

/// Parsed selector accepted by agent navigation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    /// Exact OxGraph element ID (`element:<id>`).
    Element(SymbolId),
    /// Exact qualified name (the bare default form).
    QualifiedName(QualifiedName),
    /// Exact simple symbol name (`name:<name>`).
    Name(String),
    /// Innermost symbol covering a source line (`file:<path>:<line>`).
    FileLine {
        /// Repository-relative source path.
        path: SourcePath,
        /// One-based source line.
        line: usize,
    },
}

/// Error returned when selector text cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorParseError {
    /// The original selector text.
    pub input: String,
    /// Why parsing failed.
    pub reason: &'static str,
}

impl core::fmt::Display for SelectorParseError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "invalid selector {:?}: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for SelectorParseError {}

impl Selector {
    /// Parses selector text into a typed selector.
    ///
    /// This is the one place the selector grammar lives; the store matches on
    /// the resulting enum and the CLI documents these prefixes.
    ///
    /// # Errors
    ///
    /// Returns [`SelectorParseError`] when a prefixed form is malformed (a
    /// non-numeric `element:` id, or a `file:` selector missing its line).
    pub fn parse(input: &str) -> Result<Self, SelectorParseError> {
        let trimmed = input.trim();
        if let Some(raw) = trimmed.strip_prefix("element:") {
            let id = raw.parse::<u64>().map_err(|_| SelectorParseError {
                input: input.to_owned(),
                reason: "element selector requires a numeric id",
            })?;
            return Ok(Self::Element(SymbolId::new(id)));
        }
        if let Some(name) = trimmed.strip_prefix("name:") {
            return Ok(Self::Name(name.to_owned()));
        }
        if let Some(rest) = trimmed.strip_prefix("file:") {
            let (path, line) = rest.rsplit_once(':').ok_or(SelectorParseError {
                input: input.to_owned(),
                reason: "file selector requires file:<path>:<line>",
            })?;
            let line = line.parse::<usize>().map_err(|_| SelectorParseError {
                input: input.to_owned(),
                reason: "file selector line must be a positive integer",
            })?;
            return Ok(Self::FileLine {
                path: SourcePath::from(path),
                line,
            });
        }
        Ok(Self::QualifiedName(QualifiedName::from(trimmed)))
    }
}

impl core::fmt::Display for Selector {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Element(id) => write!(formatter, "element:{id}"),
            Self::Name(name) => write!(formatter, "name:{name}"),
            Self::FileLine { path, line } => write!(formatter, "file:{path}:{line}"),
            Self::QualifiedName(name) => formatter.write_str(name.as_str()),
        }
    }
}
