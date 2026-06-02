use std::path::PathBuf;

use oxcode_model::SymbolSummary;
use oxgraph::db::DbError;

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Oxcode failure surface.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem operation failed.
    #[error("filesystem error at {path}: {source}")]
    Fs {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Tree-sitter parsing failed.
    #[error("parse error in {path}: {message}")]
    Parse {
        /// File being parsed.
        path: PathBuf,
        /// Human-readable parse message.
        message: String,
    },

    /// OxGraph database operation failed.
    #[error("oxgraph database error: {0}")]
    Database(#[from] DbError),

    /// Integer conversion overflowed.
    #[error("property {key} value {value} cannot be represented in the target type")]
    IntegerOverflow {
        /// Property whose value overflowed.
        key: &'static str,
        /// Overflowing value.
        value: usize,
    },

    /// The project database is missing catalog metadata expected by oxcode.
    #[error("database catalog is missing {item} {name}")]
    MissingCatalog {
        /// Catalog item category.
        item: &'static str,
        /// Missing catalog name.
        name: String,
    },

    /// A database subject is missing a property expected by oxcode.
    #[error("database subject is missing property {name}")]
    MissingProperty {
        /// Missing property name.
        name: String,
    },

    /// A stored value does not belong to oxcode's typed vocabulary.
    #[error("database {kind} value {value:?} is not recognized")]
    CorruptValue {
        /// The vocabulary the value should have belonged to.
        kind: &'static str,
        /// The unrecognized stored value.
        value: String,
    },

    /// A selector did not match any symbol.
    #[error("selector {selector:?} did not match any symbol")]
    SelectorNotFound {
        /// Original selector text.
        selector: String,
    },

    /// A selector matched more than one symbol.
    #[error("selector {selector:?} matched multiple symbols")]
    AmbiguousSelector {
        /// Original selector text.
        selector: String,
        /// Candidate matches.
        matches: Vec<SymbolSummary>,
    },

    /// A text navigation query could not be parsed.
    #[error("{0}")]
    InvalidQuery(String),
}

impl Error {
    /// Wraps an I/O error with the path that produced it.
    pub(crate) fn fs(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Fs {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_overflow_names_the_offending_property() {
        let message = Error::IntegerOverflow {
            key: "start_byte",
            value: usize::MAX,
        }
        .to_string();
        assert!(message.contains("start_byte"), "{message}");
    }
}
