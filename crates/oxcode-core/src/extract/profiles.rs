//! The compiled-in set of generic, query-driven language profiles.
//!
//! Each entry adds a language with a `.scm` query plus a capture→role table.
//! These are best-effort: symbols, containment, and approximate call edges (no
//! receiver typing). Promote a language to a hand-written extractor when its
//! call-graph fidelity starts to matter.

use oxcode_model::{EdgeKind, NodeKind, ReferenceKind};

use crate::extract::{
    profile::{
        CaptureRole,
        CaptureRole::{Definition, Name, Qualifier, Reference},
        LanguageProfile,
    },
    scope::ScopeKind,
};

/// A `@reference.call` capture mapped to a `Calls` edge.
const CALL: CaptureRole = Reference {
    edge: EdgeKind::Calls,
    hint: ReferenceKind::Function,
};

/// The generic profiles, registered after the hand-written extractors.
pub(crate) static PROFILES: &[LanguageProfile] = &[
    // Python: dotted packages via `__init__.py`; `#` doc comments.
    LanguageProfile {
        language_id: "python",
        extensions: &["py", "pyi"],
        parser_name: "python",
        query_source: include_str!("queries/python.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("definition.class", Definition(NodeKind::Class)),
            ("name", Name),
            ("reference.call", CALL),
            ("reference.qualifier", Qualifier),
        ],
        scope: ScopeKind::PythonPackage,
        doc_prefixes: &["#"],
    },
    // Java: package-rooted file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "java",
        extensions: &["java"],
        parser_name: "java",
        query_source: include_str!("queries/java.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.interface", Definition(NodeKind::Interface)),
            ("definition.enum", Definition(NodeKind::Enum)),
            ("definition.method", Definition(NodeKind::Method)),
            ("definition.field", Definition(NodeKind::Field)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // C: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "c",
        extensions: &["c", "h"],
        parser_name: "c",
        query_source: include_str!("queries/c.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("definition.struct", Definition(NodeKind::Struct)),
            ("definition.enum", Definition(NodeKind::Enum)),
            ("definition.type", Definition(NodeKind::TypeAlias)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // C++: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
        parser_name: "cpp",
        query_source: include_str!("queries/cpp.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.struct", Definition(NodeKind::Struct)),
            ("definition.enum", Definition(NodeKind::Enum)),
            ("definition.namespace", Definition(NodeKind::Namespace)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
];
