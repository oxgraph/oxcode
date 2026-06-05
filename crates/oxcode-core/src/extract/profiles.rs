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
    // Java: file-path scope; `//` line comments.
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
    // C#: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "csharp",
        extensions: &["cs"],
        parser_name: "csharp",
        query_source: include_str!("queries/csharp.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.interface", Definition(NodeKind::Interface)),
            ("definition.struct", Definition(NodeKind::Struct)),
            ("definition.enum", Definition(NodeKind::Enum)),
            ("definition.namespace", Definition(NodeKind::Namespace)),
            ("definition.method", Definition(NodeKind::Method)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // PHP: file-path scope; `//` and `#` line comments.
    LanguageProfile {
        language_id: "php",
        extensions: &["php"],
        parser_name: "php",
        query_source: include_str!("queries/php.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.interface", Definition(NodeKind::Interface)),
            ("definition.trait", Definition(NodeKind::Trait)),
            ("definition.enum", Definition(NodeKind::Enum)),
            ("definition.method", Definition(NodeKind::Method)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//", "#"],
    },
    // Ruby: file-path scope; `#` line comments.
    LanguageProfile {
        language_id: "ruby",
        extensions: &["rb"],
        parser_name: "ruby",
        query_source: include_str!("queries/ruby.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.module", Definition(NodeKind::Module)),
            ("definition.method", Definition(NodeKind::Method)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["#"],
    },
    // Swift: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "swift",
        extensions: &["swift"],
        parser_name: "swift",
        query_source: include_str!("queries/swift.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.interface", Definition(NodeKind::Interface)),
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // Kotlin: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "kotlin",
        extensions: &["kt", "kts"],
        parser_name: "kotlin",
        query_source: include_str!("queries/kotlin.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // Scala: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "scala",
        extensions: &["scala", "sc"],
        parser_name: "scala",
        query_source: include_str!("queries/scala.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.trait", Definition(NodeKind::Trait)),
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // Dart: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "dart",
        extensions: &["dart"],
        parser_name: "dart",
        query_source: include_str!("queries/dart.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // Lua: file-path scope; `--` line comments.
    LanguageProfile {
        language_id: "lua",
        extensions: &["lua"],
        parser_name: "lua",
        query_source: include_str!("queries/lua.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["--"],
    },
    // Luau: Roblox Lua dialect; reuses the Lua query.
    LanguageProfile {
        language_id: "luau",
        extensions: &["luau"],
        parser_name: "luau",
        query_source: include_str!("queries/lua.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
            ("reference.call", CALL),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["--"],
    },
    // Objective-C: `.m`/`.mm` only (`.h` stays with C); `//` line comments.
    LanguageProfile {
        language_id: "objc",
        extensions: &["m", "mm"],
        parser_name: "objc",
        query_source: include_str!("queries/objc.scm"),
        captures: &[
            ("definition.class", Definition(NodeKind::Class)),
            ("definition.method", Definition(NodeKind::Method)),
            ("name", Name),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
    // Pascal/Delphi: file-path scope; `//` line comments.
    LanguageProfile {
        language_id: "pascal",
        extensions: &["pas", "dpr", "dpk", "lpr"],
        parser_name: "pascal",
        query_source: include_str!("queries/pascal.scm"),
        captures: &[
            ("definition.function", Definition(NodeKind::Function)),
            ("name", Name),
        ],
        scope: ScopeKind::FileStem,
        doc_prefixes: &["//"],
    },
];
