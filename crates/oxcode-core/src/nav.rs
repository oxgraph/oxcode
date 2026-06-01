//! Agent-facing navigation over a typed code graph read model.

use oxcode_model::{
    CallGraphReport, ExpandedQueryReport, GraphDirection, SymbolReport, SymbolSummary,
};
use oxgraph::db::{QueryLanguage, QueryResult};

use crate::error::Result;

/// Read-only graph operations needed by agent navigation.
pub(crate) trait CodeGraphRead {
    /// Resolves a selector into all matching symbols.
    fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>>;

    /// Resolves a selector into exactly one symbol.
    fn resolve_one_symbol(&self, selector: &str) -> Result<SymbolSummary>;

    /// Builds a bounded calls-graph report from one selector.
    fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport>;

    /// Executes a raw query against the same read snapshot.
    fn execute_query(&self, language: QueryLanguage, query: &str) -> Result<QueryResult>;

    /// Expands raw query rows using the same read snapshot.
    fn expand_query_result(&self, result: &QueryResult) -> Result<ExpandedQueryReport>;
}

impl CodeGraphRead for crate::store::oxgraph::ReadSession<'_> {
    fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>> {
        Self::resolve_selector(self, selector)
    }

    fn resolve_one_symbol(&self, selector: &str) -> Result<SymbolSummary> {
        Self::resolve_one_symbol(self, selector)
    }

    fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport> {
        Self::call_graph(self, selector, direction, depth, limit)
    }

    fn execute_query(&self, language: QueryLanguage, query: &str) -> Result<QueryResult> {
        Self::execute_query(self, language, query)
    }

    fn expand_query_result(&self, result: &QueryResult) -> Result<ExpandedQueryReport> {
        Self::expand_query_result(self, result)
    }
}

pub(crate) fn resolve_selector(
    read: &impl CodeGraphRead,
    selector: &str,
) -> Result<Vec<SymbolSummary>> {
    read.resolve_selector(selector)
}

pub(crate) fn describe_symbol(read: &impl CodeGraphRead, selector: &str) -> Result<SymbolReport> {
    Ok(SymbolReport {
        selector: selector.to_string(),
        symbol: read.resolve_one_symbol(selector)?,
    })
}

pub(crate) fn call_graph(
    read: &impl CodeGraphRead,
    selector: &str,
    direction: GraphDirection,
    depth: usize,
    limit: usize,
) -> Result<CallGraphReport> {
    read.call_graph(selector, direction, depth, limit)
}

pub(crate) fn query_expanded(
    read: &impl CodeGraphRead,
    language: QueryLanguage,
    query: &str,
) -> Result<ExpandedQueryReport> {
    let rows = read.execute_query(language, query)?;
    read.expand_query_result(&rows)
}

#[cfg(test)]
mod tests {
    use oxcode_model::{CodeLocation, NodeKind};

    use super::*;

    struct FakeRead {
        symbol: SymbolSummary,
    }

    impl CodeGraphRead for FakeRead {
        fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>> {
            Ok((selector == self.symbol.qualified_name)
                .then(|| self.symbol.clone())
                .into_iter()
                .collect())
        }

        fn resolve_one_symbol(&self, selector: &str) -> Result<SymbolSummary> {
            Ok(self
                .resolve_selector(selector)?
                .into_iter()
                .next()
                .expect("fake selector should match"))
        }

        fn call_graph(
            &self,
            _selector: &str,
            direction: GraphDirection,
            depth: usize,
            limit: usize,
        ) -> Result<CallGraphReport> {
            Ok(CallGraphReport {
                selector: self.symbol.qualified_name.clone(),
                seed: self.symbol.clone(),
                direction,
                depth,
                limit,
                symbols: Vec::new(),
                edges: Vec::new(),
            })
        }

        fn execute_query(&self, _language: QueryLanguage, _query: &str) -> Result<QueryResult> {
            unreachable!("not needed by this test")
        }

        fn expand_query_result(&self, _result: &QueryResult) -> Result<ExpandedQueryReport> {
            Ok(ExpandedQueryReport { rows: Vec::new() })
        }
    }

    #[test]
    fn navigation_uses_read_trait_for_symbol_reports() {
        let read = FakeRead {
            symbol: SymbolSummary {
                id: 7,
                stable_key: "symbol:src/lib.rs:function:entry:0".to_string(),
                name: "entry".to_string(),
                qualified_name: "entry".to_string(),
                kind: NodeKind::Function.as_str().to_string(),
                language: "rust".to_string(),
                definition: CodeLocation {
                    file_path: "src/lib.rs".to_string(),
                    start_byte: 0,
                    end_byte: 10,
                    start_line: 1,
                    start_column: 0,
                    end_line: 1,
                    end_column: 10,
                },
                signature: Some("fn entry()".to_string()),
                docstring: None,
                source_preview: Some("fn entry() {}".to_string()),
            },
        };

        let report = describe_symbol(&read, "entry").expect("report");
        assert_eq!(report.symbol.qualified_name, "entry");
    }
}
