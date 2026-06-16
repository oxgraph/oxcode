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
        /// An impl block to the concrete type it implements (distinct from
        /// `Implements`, which points at the trait).
        ImplementsFor => "implements_for",
        /// A container (crate/module/file) depends on another because a symbol it
        /// contains references a symbol the other contains. Lifted, not extracted.
        DependsOn => "depends_on",
    }
}

string_enum! {
    /// Kind of n-ary code relationship stored as an OxGraph relation type,
    /// complementary to [`EdgeKind`] for facts with more than two participants.
    pub enum HyperedgeKind {
        /// A trait implementation grouped as one fact: the impl block (anchor),
        /// its concrete type, the implemented trait, and the methods.
        ImplGroup => "impl_group",
        /// A container and its direct members grouped as one fact (a module,
        /// file, package, or other container and what it directly contains).
        Membership => "membership",
    }
}

string_enum! {
    /// Structural role a participant plays in a hyperedge.
    ///
    /// Source-side roles ([`Self::SOURCE`]) are the parts; the single target-side
    /// role [`Self::Anchor`] is the unit they belong to. Personalized hypergraph
    /// PageRank flows rank from sources to targets, so orienting parts → anchor
    /// makes anchors (containers, impl blocks) accrue architectural centrality.
    pub enum ParticipantRole {
        /// The unit a hyperedge centers on: an impl block or a container
        /// (target side).
        Anchor => "anchor",
        /// A generic member or part — a contained symbol or an impl method
        /// (source side).
        Member => "member",
        /// The concrete type of an impl group (source side).
        ImplType => "impl_type",
        /// The implemented trait of an impl group (source side).
        ImplTrait => "impl_trait",
    }
}

impl ParticipantRole {
    /// Roles whose participants are source-side (the parts).
    pub const SOURCE: &'static [Self] = &[Self::Member, Self::ImplType, Self::ImplTrait];
    /// Roles whose participants are target-side (the anchor unit).
    pub const TARGET: &'static [Self] = &[Self::Anchor];
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
        /// A reference to a concrete type (e.g. the self type of an impl block).
        Type => "type",
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
