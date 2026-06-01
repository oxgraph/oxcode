//! Identifier newtypes shared across the workspace.

/// OxGraph element identifier for a symbol.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct SymbolId(pub u64);

impl SymbolId {
    /// Creates a symbol ID.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw OxGraph element ID.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl core::fmt::Display for SymbolId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl From<u64> for SymbolId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

string_newtype!(LanguageId, "Language identifier owned by an extractor.");
string_newtype!(SymbolKey, "Stable, deterministic symbol key.");
string_newtype!(QualifiedName, "Qualified language-level symbol name.");
string_newtype!(SourcePath, "Repository-relative source path.");
