use oxcode_model::{
    CallGraphReport, CodeLocation, ExpandedQueryReport, SymbolReport, SymbolSummary,
};
use oxgraph::db::{PropertySubject, QueryValue};

/// Formats one query value for compact CLI output.
#[must_use]
pub fn format_query_value(value: &QueryValue) -> String {
    match value {
        QueryValue::Element(id) => format!("element:{}", id.get()),
        QueryValue::Relation(id) => format!("relation:{}", id.get()),
        QueryValue::Incidence(record) => format!(
            "incidence:{} relation={} element={} role={}",
            record.id.get(),
            record.relation.get(),
            record.element.get(),
            record.role.get()
        ),
        QueryValue::Subject(subject) => match subject {
            PropertySubject::Element(id) => format!("element:{}", id.get()),
            PropertySubject::Relation(id) => format!("relation:{}", id.get()),
            PropertySubject::Incidence(id) => format!("incidence:{}", id.get()),
        },
        QueryValue::Property(value) => value.to_string(),
        QueryValue::Text(value) => value.clone(),
        QueryValue::Projection(id) => format!("projection:{}", id.get()),
    }
}

/// Formats one symbol report for agent-facing CLI output.
#[must_use]
pub fn format_symbol_report(report: &SymbolReport) -> String {
    let mut output = String::new();
    push_symbol_block(&mut output, "symbol", &report.symbol);
    output
}

/// Formats one call graph report for agent-facing CLI output.
#[must_use]
pub fn format_call_graph_report(report: &CallGraphReport) -> String {
    let mut output = String::new();
    push_symbol_block(&mut output, "seed", &report.seed);
    output.push_str(&format!(
        "walk calls direction={} depth={} limit={}\n",
        report.direction, report.depth, report.limit
    ));
    if report.edges.is_empty() {
        output.push_str("  no call edges found\n");
        return output;
    }
    for edge in &report.edges {
        output.push_str(&format!(
            "  depth {} relation:{}\n",
            depth_label(edge.depth),
            edge.relation_id
        ));
        output.push_str(&format!(
            "    {} -> {}\n",
            symbol_inline(&edge.source),
            symbol_inline(&edge.target)
        ));
        if let Some(call_site) = &edge.call_site {
            output.push_str(&format!(
                "    called from {}\n",
                location_range(&call_site.location)
            ));
            if !call_site.text.is_empty() {
                output.push_str(&format!("    expression {}\n", call_site.text));
            }
        } else {
            output.push_str("    call site unavailable\n");
        }
    }
    output
}

/// Formats expanded query rows for agent-facing CLI output.
#[must_use]
pub fn format_expanded_query_report(report: &ExpandedQueryReport) -> String {
    let mut output = String::new();
    if report.rows.is_empty() {
        output.push_str("no rows\n");
        return output;
    }
    for (index, row) in report.rows.iter().enumerate() {
        output.push_str(&format!("row {}\n", index + 1));
        for value in &row.values {
            output.push_str(&format!("  {}\n", value.raw));
            if let Some(symbol) = &value.symbol {
                output.push_str(&format!("    {}\n", symbol_inline(symbol)));
                output.push_str(&format!(
                    "    defined at {}\n",
                    location_range(&symbol.definition)
                ));
            }
            if let Some(edge) = &value.call_edge {
                output.push_str(&format!(
                    "    calls {} -> {}\n",
                    symbol_inline(&edge.source),
                    symbol_inline(&edge.target)
                ));
                if let Some(call_site) = &edge.call_site {
                    output.push_str(&format!(
                        "    called from {}\n",
                        location_range(&call_site.location)
                    ));
                    if !call_site.text.is_empty() {
                        output.push_str(&format!("    expression {}\n", call_site.text));
                    }
                }
            }
        }
    }
    output
}

/// Formats selector ambiguity matches for agent-facing CLI output.
#[must_use]
pub fn format_selector_matches(selector: &str, matches: &[SymbolSummary]) -> String {
    let mut output = format!("selector {selector:?} matched multiple symbols\n");
    for symbol in matches {
        output.push_str(&format!(
            "  {} retry: element:{} or {}\n",
            symbol_inline(symbol),
            symbol.id,
            symbol.qualified_name
        ));
    }
    output
}

fn push_symbol_block(output: &mut String, label: &str, symbol: &SymbolSummary) {
    output.push_str(&format!("{label} element:{}\n", symbol.id));
    output.push_str(&format!("  {}\n", symbol.qualified_name));
    output.push_str(&format!("  {}\n", symbol.kind));
    output.push_str(&format!(
        "  defined at {}\n",
        location_range(&symbol.definition)
    ));
}

/// Formats one symbol on a single line.
fn symbol_inline(symbol: &SymbolSummary) -> String {
    format!(
        "element:{} {} {} {}",
        symbol.id,
        symbol.qualified_name,
        symbol.kind,
        location_range(&symbol.definition)
    )
}

/// Formats one source range.
fn location_range(location: &CodeLocation) -> String {
    format!(
        "{}:{}:{}-{}:{}",
        location.file_path,
        location.span.start_line,
        location.span.start_column,
        location.span.end_line,
        location.span.end_column
    )
}

/// Formats a traversal hop depth, using `-` for query-expanded edges.
fn depth_label(depth: Option<usize>) -> String {
    depth.map_or_else(|| "-".to_string(), |depth| depth.to_string())
}
