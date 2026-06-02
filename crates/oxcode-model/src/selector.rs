//! The single selector grammar accepted by agent navigation.

use serde::{Deserialize, Serialize};

use crate::{EdgeKind, GraphDirection, QualifiedName, SourcePath, SymbolId};

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

/// A parsed text navigation query: walk one edge kind from a seed selector.
///
/// Grammar: `<edge> <direction> <selector> [depth <n>] [limit <n>]`, e.g.
/// `calls outgoing crate::entry depth 2`. This is the text surface that lowers
/// into the typed navigation API; the same engine drives both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavQuery {
    /// Edge kind to traverse.
    pub edge_kind: EdgeKind,
    /// Traversal direction from the seed.
    pub direction: GraphDirection,
    /// Seed selector text (parsed by [`Selector::parse`] downstream).
    pub selector: String,
    /// Maximum hop depth.
    pub depth: usize,
    /// Maximum number of rows to return.
    pub limit: usize,
}

/// Error returned when navigation query text cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavQueryParseError {
    /// The original navigation query text.
    pub input: String,
    /// Why parsing failed.
    pub reason: &'static str,
}

impl core::fmt::Display for NavQueryParseError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "invalid navigation query {:?}: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for NavQueryParseError {}

impl NavQuery {
    /// Default hop depth when the query omits `depth`.
    const DEFAULT_DEPTH: usize = 2;
    /// Default row limit when the query omits `limit`.
    const DEFAULT_LIMIT: usize = 1000;

    /// Parses navigation query text into a typed query.
    ///
    /// # Errors
    ///
    /// Returns [`NavQueryParseError`] when the edge kind or direction is
    /// unknown, the seed is missing, or an option is malformed.
    pub fn parse(input: &str) -> Result<Self, NavQueryParseError> {
        let error = |reason: &'static str| NavQueryParseError {
            input: input.to_owned(),
            reason,
        };
        let tokens: Vec<&str> = input.split_whitespace().collect();
        let [edge, direction, selector, rest @ ..] = tokens.as_slice() else {
            return Err(error("expected '<edge> <direction> <selector>'"));
        };
        let edge_kind = EdgeKind::try_from(*edge).map_err(|_| error("unknown edge kind"))?;
        let direction = GraphDirection::try_from(*direction)
            .map_err(|_| error("unknown direction (use outgoing|incoming|both)"))?;

        let mut depth = Self::DEFAULT_DEPTH;
        let mut limit = Self::DEFAULT_LIMIT;
        let mut options = rest.iter();
        while let Some(key) = options.next() {
            let value = options
                .next()
                .ok_or_else(|| error("trailing option is missing a value"))?;
            let parsed = value
                .parse::<usize>()
                .map_err(|_| error("option value must be a non-negative integer"))?;
            match *key {
                "depth" => depth = parsed,
                "limit" => limit = parsed,
                _ => return Err(error("unknown option (use depth|limit)")),
            }
        }

        Ok(Self {
            edge_kind,
            direction,
            selector: (*selector).to_owned(),
            depth,
            limit,
        })
    }
}

impl core::fmt::Display for NavQuery {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{} {} {} depth {} limit {}",
            self.edge_kind.as_str(),
            self.direction.as_str(),
            self.selector,
            self.depth,
            self.limit
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_query_parses_full_grammar() {
        let query = NavQuery::parse("calls outgoing name:entry depth 4 limit 25").expect("parse");
        assert_eq!(query.edge_kind, EdgeKind::Calls);
        assert_eq!(query.direction, GraphDirection::Outgoing);
        assert_eq!(query.selector, "name:entry");
        assert_eq!(query.depth, 4);
        assert_eq!(query.limit, 25);
    }

    #[test]
    fn nav_query_applies_defaults_when_options_omitted() {
        let query = NavQuery::parse("contains incoming crate::a::b").expect("parse");
        assert_eq!(query.depth, NavQuery::DEFAULT_DEPTH);
        assert_eq!(query.limit, NavQuery::DEFAULT_LIMIT);
    }

    #[test]
    fn nav_query_round_trips_through_display() {
        let query = NavQuery::parse("references both element:7 depth 1 limit 9").expect("parse");
        let reparsed = NavQuery::parse(&query.to_string()).expect("reparse");
        assert_eq!(query, reparsed);
    }

    #[test]
    fn nav_query_rejects_malformed_input() {
        assert!(NavQuery::parse("calls").is_err(), "missing direction+seed");
        assert!(
            NavQuery::parse("bogus outgoing x").is_err(),
            "unknown edge kind"
        );
        assert!(
            NavQuery::parse("calls sideways x").is_err(),
            "unknown direction"
        );
        assert!(
            NavQuery::parse("calls outgoing x depth").is_err(),
            "option without a value"
        );
        assert!(
            NavQuery::parse("calls outgoing x depth two").is_err(),
            "non-numeric option value"
        );
        assert!(
            NavQuery::parse("calls outgoing x stride 2").is_err(),
            "unknown option"
        );
    }
}
