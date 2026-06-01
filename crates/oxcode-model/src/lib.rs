//! Storage-neutral model, schema vocabulary, and report types for oxcode.
//!
//! This crate owns the typed vocabulary the rest of the workspace shares: the
//! code-graph kinds ([`NodeKind`]/[`EdgeKind`]), the identifiers and newtypes,
//! the graph schema catalog ([`ElementProperty`]/[`RelationProperty`]), the
//! selector grammar ([`Selector`]), and the agent-facing report types. It is
//! intentionally dependency-light (no storage or CLI dependencies) so it can be
//! the single source of truth both the extractor/resolver and the storage layer
//! derive their behavior from.

#[macro_use]
mod macros;

mod extract;
mod ids;
mod index;
mod kind;
mod report;
mod schema;
mod selector;
mod span;

pub use crate::{
    extract::*, ids::*, index::*, kind::*, report::*, schema::*, selector::*, span::*,
};

/// Error returned when a stored string does not match any enum variant.
///
/// Produced by the generated `FromStr`/`TryFrom<&str>` impls so the read path
/// can surface schema drift loudly instead of silently coercing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant {
    /// The enum type name that failed to parse.
    pub kind: &'static str,
    /// The unrecognized value.
    pub value: String,
}

impl core::fmt::Display for UnknownVariant {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "unknown {} {:?}", self.kind, self.value)
    }
}

impl std::error::Error for UnknownVariant {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtypes_serialize_as_plain_values() {
        assert_eq!(
            serde_json::to_string(&SymbolKey::from("symbol:one")).expect("json"),
            "\"symbol:one\""
        );
        assert_eq!(
            serde_json::to_string(&SymbolId::new(42)).expect("json"),
            "42"
        );
    }

    #[test]
    fn node_kind_round_trips_through_storage_spelling() {
        for kind in NodeKind::ALL {
            assert_eq!(
                kind.as_str().parse::<NodeKind>().expect("parse"),
                kind,
                "{kind} should round-trip"
            );
        }
        // The diagnostic pseudo-kind round-trips but stays out of ALL.
        assert_eq!(
            "unresolved_reference".parse::<NodeKind>().expect("parse"),
            NodeKind::Unresolved
        );
        assert!(!NodeKind::ALL.contains(&NodeKind::Unresolved));
        assert!("not_a_kind".parse::<NodeKind>().is_err());
    }

    #[test]
    fn edge_and_direction_round_trip() {
        for kind in EdgeKind::ALL {
            assert_eq!(kind.as_str().parse::<EdgeKind>().expect("parse"), kind);
        }
        assert_eq!(GraphDirection::default(), GraphDirection::Outgoing);
        assert_eq!(
            "both".parse::<GraphDirection>().expect("parse"),
            GraphDirection::Both
        );
    }

    #[test]
    fn enum_json_stays_snake_case() {
        assert_eq!(
            serde_json::to_string(&NodeKind::ImplBlock).expect("json"),
            "\"impl_block\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::TypeAlias).expect("json"),
            "\"type_alias\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::Calls).expect("json"),
            "\"calls\""
        );
        assert_eq!(
            serde_json::to_string(&GraphDirection::Incoming).expect("json"),
            "\"incoming\""
        );
        let parsed: NodeKind = serde_json::from_str("\"impl_block\"").expect("from json");
        assert_eq!(parsed, NodeKind::ImplBlock);
    }

    #[test]
    fn property_catalog_keys_are_unique_and_indexed_subset() {
        let mut keys: Vec<&str> = ElementProperty::ALL.iter().map(|p| p.key()).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "element property keys must be unique");

        let mut relation_keys: Vec<&str> = RelationProperty::ALL.iter().map(|p| p.key()).collect();
        let relation_total = relation_keys.len();
        relation_keys.sort_unstable();
        relation_keys.dedup();
        assert_eq!(relation_keys.len(), relation_total);

        for indexed in ElementProperty::INDEXED {
            assert!(
                ElementProperty::ALL.contains(indexed),
                "{} indexed but not in ALL",
                indexed.key()
            );
        }
    }

    #[test]
    fn selector_parse_covers_every_form() {
        assert_eq!(
            Selector::parse("element:42").expect("element"),
            Selector::Element(SymbolId::new(42))
        );
        assert_eq!(
            Selector::parse("name:helper").expect("name"),
            Selector::Name("helper".to_owned())
        );
        assert_eq!(
            Selector::parse("file:src/lib.rs:10").expect("file"),
            Selector::FileLine {
                path: SourcePath::from("src/lib.rs"),
                line: 10,
            }
        );
        assert_eq!(
            Selector::parse("crate::entry").expect("qualified"),
            Selector::QualifiedName(QualifiedName::from("crate::entry"))
        );
        assert!(Selector::parse("element:nope").is_err());
        assert!(Selector::parse("file:src/lib.rs").is_err());
    }

    #[test]
    fn selector_display_round_trips() {
        for input in ["element:7", "name:foo", "file:src/a.rs:3", "crate::a::b"] {
            let selector = Selector::parse(input).expect("parse");
            assert_eq!(
                Selector::parse(&selector.to_string()).expect("reparse"),
                selector
            );
        }
    }
}
