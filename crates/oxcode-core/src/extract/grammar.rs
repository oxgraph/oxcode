//! Statically-linked tree-sitter grammars.
//!
//! Each supported language's grammar is an ordinary crate dependency compiled
//! into the binary, so parsing is offline, deterministic, and works from a
//! `cargo install` / prebuilt binary with no runtime download. This replaces
//! `tree-sitter-language-pack` as the grammar provider.

use tree_sitter::{Language, Parser, Tree};

/// Returns the statically-linked grammar for a parser name, if oxcode bundles it.
pub(crate) fn language(parser_name: &str) -> Option<Language> {
    Some(match parser_name {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        "objc" => tree_sitter_objc::LANGUAGE.into(),
        "pascal" => tree_sitter_pascal::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "luau" => tree_sitter_luau::LANGUAGE.into(),
        _ => return None,
    })
}

/// Parses `source` with the named grammar, returning `None` if the grammar is
/// unavailable or the parser produces no tree.
pub(crate) fn parse(parser_name: &str, source: &[u8]) -> Option<Tree> {
    let language = language(parser_name)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_grammar_loads_and_parses() {
        // A missing/ABI-incompatible grammar crate would fail here rather than
        // at runtime on a user's machine.
        for name in [
            "rust",
            "go",
            "python",
            "java",
            "c",
            "cpp",
            "javascript",
            "typescript",
            "tsx",
            "ruby",
            "csharp",
            "php",
            "scala",
            "swift",
            "dart",
            "objc",
            "pascal",
            "kotlin",
            "lua",
            "luau",
        ] {
            assert!(language(name).is_some(), "{name}: no grammar");
            assert!(parse(name, b"").is_some(), "{name}: no parse tree");
        }
    }
}
