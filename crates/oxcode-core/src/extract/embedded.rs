//! Host extractors for component files that embed a `<script>` block.
//!
//! Svelte and Vue parse their script bodies as opaque `raw_text`, so symbols
//! live in the embedded JS/TS. This extractor locates the script ranges with
//! the host grammar, masks every other byte to whitespace (preserving byte and
//! line offsets), and runs the TypeScript extractor over the masked file — so
//! the extracted spans stay accurate to the original component file.

use std::path::Path;

use oxcode_model::{Extraction, LanguageId};
use tree_sitter::{Node, Parser};

use crate::{
    error::{Error, Result},
    extract::{
        ExtractionInput, LanguageExtractor,
        scope::{JsTsScope, ScopeStrategy},
        typescript,
    },
};

/// An extractor for a component format whose logic lives in a `<script>` block.
pub(crate) struct ScriptHostExtractor {
    language_id: &'static str,
    extensions: &'static [&'static str],
    parser_name: &'static str,
}

impl ScriptHostExtractor {
    /// Svelte components (`.svelte`).
    pub(crate) const fn svelte() -> Self {
        Self {
            language_id: "svelte",
            extensions: &["svelte"],
            parser_name: "svelte",
        }
    }

    /// Vue single-file components (`.vue`).
    pub(crate) const fn vue() -> Self {
        Self {
            language_id: "vue",
            extensions: &["vue"],
            parser_name: "vue",
        }
    }
}

impl LanguageExtractor for ScriptHostExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::from(self.language_id)
    }

    fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    fn parser_name(&self) -> &'static str {
        self.parser_name
    }

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let scope = JsTsScope.base_scope(input.path, &input.relative_path);
        let ranges = script_ranges(self.parser_name, input.path, &input.source)?;
        let masked = mask_to_ranges(&input.source, &ranges);
        typescript::extract_script(
            &input.relative_path,
            &scope,
            LanguageId::from(self.language_id),
            &masked,
        )
    }
}

/// Returns the byte ranges of the `<script>` bodies in a component file.
fn script_ranges(parser_name: &str, path: &Path, source: &[u8]) -> Result<Vec<(usize, usize)>> {
    let language =
        tree_sitter_language_pack::get_language(parser_name).map_err(|error| Error::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| Error::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let tree = parser.parse(source, None).ok_or_else(|| Error::Parse {
        path: path.to_path_buf(),
        message: "tree-sitter returned no parse tree".to_string(),
    })?;
    let mut ranges = Vec::new();
    collect_script_text(tree.root_node(), false, &mut ranges);
    Ok(ranges)
}

/// Collects the byte ranges of `raw_text` nodes inside a `script_element`.
fn collect_script_text(node: Node, in_script: bool, ranges: &mut Vec<(usize, usize)>) {
    let in_script = in_script || node.kind() == "script_element";
    if in_script && node.kind() == "raw_text" {
        ranges.push((node.start_byte(), node.end_byte()));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_script_text(child, in_script, ranges);
    }
}

/// Returns a copy of `source` with every byte outside `ranges` blanked to a
/// space (newlines preserved), keeping the script bodies at their file offsets.
fn mask_to_ranges(source: &[u8], ranges: &[(usize, usize)]) -> Vec<u8> {
    source
        .iter()
        .enumerate()
        .map(|(index, &byte)| {
            if ranges
                .iter()
                .any(|(start, end)| index >= *start && index < *end)
            {
                byte
            } else if byte == b'\n' {
                b'\n'
            } else {
                b' '
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use oxcode_model::NodeKind;

    use super::*;

    fn extract(extractor: &ScriptHostExtractor, relative: &str, source: &str) -> Extraction {
        extractor
            .extract(ExtractionInput {
                path: Path::new(relative),
                relative_path: relative.to_string(),
                source: source.as_bytes().to_vec(),
            })
            .expect("extract")
    }

    #[test]
    fn svelte_script_functions_are_extracted_at_file_offsets() {
        let source = "<div>{count}</div>\n<script>\nfunction helper() {}\nfunction entry() { helper(); }\n</script>\n";
        let extraction = extract(&ScriptHostExtractor::svelte(), "src/App.svelte", source);
        let entry = extraction
            .nodes
            .iter()
            .find(|node| node.name == "entry" && node.kind == NodeKind::Function)
            .expect("entry function");
        // The span is accurate to the original component file, not the script.
        assert!(source[entry.span.start_byte..].starts_with("function entry"));
        // The in-script call is captured.
        assert!(
            extraction
                .references
                .iter()
                .any(|reference| reference.text.contains("helper()"))
        );
    }

    #[test]
    fn vue_script_setup_functions_are_extracted() {
        let source =
            "<template><div/></template>\n<script setup>\nfunction useThing() {}\n</script>\n";
        let extraction = extract(&ScriptHostExtractor::vue(), "src/Thing.vue", source);
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.name == "useThing" && node.kind == NodeKind::Function)
        );
    }
}
