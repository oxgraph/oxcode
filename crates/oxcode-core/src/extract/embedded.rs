//! Host extractors for component files that embed a `<script>` block.
//!
//! Svelte and Vue keep their logic in a `<script>` block. This extractor locates
//! those blocks with a byte-scan (no host grammar needed), masks every other byte
//! to whitespace (preserving byte and line offsets), and runs the TypeScript
//! extractor over the masked file — so the extracted spans stay accurate to the
//! original component file.

use oxcode_model::{Extraction, LanguageId};

use crate::{
    error::Result,
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
}

impl ScriptHostExtractor {
    /// Svelte components (`.svelte`).
    pub(crate) const fn svelte() -> Self {
        Self {
            language_id: "svelte",
            extensions: &["svelte"],
        }
    }

    /// Vue single-file components (`.vue`).
    pub(crate) const fn vue() -> Self {
        Self {
            language_id: "vue",
            extensions: &["vue"],
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

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let scope = JsTsScope.base_scope(input.path, &input.relative_path);
        let ranges = script_ranges(&input.source);
        let masked = mask_to_ranges(&input.source, &ranges);
        typescript::extract_script(
            &input.relative_path,
            &scope,
            LanguageId::from(self.language_id),
            &masked,
        )
    }
}

/// Returns the byte ranges of the `<script>…</script>` bodies in a component
/// file, found by a case-insensitive tag scan (no host grammar required).
fn script_ranges(source: &[u8]) -> Vec<(usize, usize)> {
    let Ok(text) = std::str::from_utf8(source) else {
        return Vec::new();
    };
    // ASCII-lowercasing preserves byte length, so offsets map 1:1 to `source`.
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some(open) = find(&bytes[cursor..], b"<script").map(|rel| cursor + rel) {
        let Some(tag_end) = find(&bytes[open..], b">").map(|rel| open + rel + 1) else {
            break;
        };
        let Some(close) = find(&bytes[tag_end..], b"</script>").map(|rel| tag_end + rel) else {
            break;
        };
        if close > tag_end {
            ranges.push((tag_end, close));
        }
        cursor = close + "</script>".len();
    }
    ranges
}

/// Returns the start offset of `needle` in `haystack`, if present.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
    use std::path::Path;

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
