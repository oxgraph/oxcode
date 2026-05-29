//! Code-graph kinds and categories, generated from one declaration each.

string_enum! {
    /// Kind of code symbol stored as an OxGraph element label.
    ///
    /// [`NodeKind::Unresolved`] is a diagnostic pseudo-kind: it round-trips
    /// through storage but is intentionally excluded from [`NodeKind::ALL`] so
    /// it is never registered as an ordinary symbol label.
    pub enum NodeKind {
        File => "file",
        Module => "module",
        Namespace => "namespace",
        Package => "package",
        Class => "class",
        Struct => "struct",
        Enum => "enum",
        Trait => "trait",
        Interface => "interface",
        ImplBlock => "impl_block",
        Function => "function",
        Method => "method",
        Field => "field",
        Variable => "variable",
        Constant => "constant",
        TypeAlias => "type_alias",
        Macro => "macro",
    }
    extra {
        Unresolved => "unresolved_reference",
    }
}

string_enum! {
    /// Kind of code relationship stored as an OxGraph relation type.
    pub enum EdgeKind {
        Contains => "contains",
        Imports => "imports",
        Calls => "calls",
        References => "references",
        Implements => "implements",
        Defines => "defines",
    }
}

string_enum! {
    /// Direction for agent-friendly call graph navigation.
    pub enum GraphDirection {
        Outgoing => "outgoing",
        Incoming => "incoming",
        Both => "both",
    }
    default = Outgoing;
}

string_enum! {
    /// Language-neutral category an extractor attaches to a reference target.
    ///
    /// Lets the resolver disambiguate (e.g. a method call from a free function
    /// call) without re-parsing language syntax.
    pub enum ReferenceKind {
        Function => "function",
        Method => "method",
        Macro => "macro",
        Trait => "trait",
        Import => "import",
        ImportGlob => "import_glob",
    }
}

string_enum! {
    /// How a resolved edge's target was chosen, surfaced so consumers can filter
    /// confident edges from best-effort ones.
    pub enum ResolutionKind {
        /// Matched a unique crate-qualified name.
        Exact => "exact",
        /// Matched within the reference's enclosing module scope.
        Scoped => "scoped",
        /// Matched through an in-scope import.
        Import => "import",
        /// Matched a `Type::method` via the reference's receiver type.
        Receiver => "receiver",
        /// Matched a unique bare last-segment name.
        Simple => "simple",
        /// One of several equally-plausible candidates (kept, not dropped).
        Ambiguous => "ambiguous",
    }
}
