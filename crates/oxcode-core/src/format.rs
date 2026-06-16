use oxcode_model::{
    CallEdgeSummary, CallGraphReport, CodeLocation, ContextReport, ExpandedQueryReport,
    ExpandedQueryValue, FileSearchReport, ParticipantRole, SymbolReport, SymbolSearchReport,
    SymbolSummary,
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

/// Formats one symbol search report for agent-facing CLI output.
#[must_use]
pub fn format_symbol_search_report(report: &SymbolSearchReport) -> String {
    let mut output = String::new();
    if report.matches.is_empty() {
        output.push_str(&format!("no symbols matched {:?}\n", report.query));
        return output;
    }
    for entry in &report.matches {
        output.push_str(&format!(
            "{} score={}\n",
            symbol_inline(&entry.symbol),
            entry.score
        ));
        push_symbol_metadata(&mut output, &entry.symbol, "  ");
    }
    output
}

/// Formats one file search report for agent-facing CLI output.
#[must_use]
pub fn format_file_search_report(report: &FileSearchReport) -> String {
    let mut output = String::new();
    if report.files.is_empty() {
        output.push_str(&format!("no files matched {:?}\n", report.query));
        return output;
    }
    for file in &report.files {
        output.push_str(&format!(
            "{} symbols={} score={}\n",
            file.path, file.symbol_count, file.score
        ));
        for symbol in &file.top_symbols {
            output.push_str(&format!("  {}\n", symbol_inline(symbol)));
        }
    }
    output
}

/// Formats one task-oriented context report for agent-facing CLI output.
#[must_use]
pub fn format_context_report(report: &ContextReport) -> String {
    let mut output = format!("context {:?}\n{}\n", report.query, report.summary);

    output.push_str("symbols\n");
    if report.symbols.is_empty() {
        output.push_str("  none\n");
    }
    for symbol in &report.symbols {
        output.push_str(&format!(
            "  #{} {} [{}] pagerank={:.4}\n",
            symbol.id.get(),
            symbol.qualified_name,
            symbol.kind,
            symbol.pagerank,
        ));
        if let Some(signature) = &symbol.signature {
            output.push_str(&format!(
                "    signature {}\n",
                one_line_preview(signature, 240)
            ));
        }
    }

    output.push_str("relationships\n");
    if report.relationships.is_empty() {
        output.push_str("  none\n");
    }
    for relation in &report.relationships {
        output.push_str(&format!(
            "  {} #{} -> #{} relation:{}\n",
            relation.kind,
            relation.source_id.get(),
            relation.target_id.get(),
            relation.relation_id,
        ));
    }

    if !report.hyperedges.is_empty() {
        output.push_str("hyperedges\n");
        for hyperedge in &report.hyperedges {
            let anchor = hyperedge
                .participants
                .iter()
                .find(|participant| participant.role == ParticipantRole::Anchor)
                .map_or_else(
                    || "-".to_owned(),
                    |participant| format!("#{}", participant.id.get()),
                );
            let members = hyperedge
                .participants
                .iter()
                .filter(|participant| participant.role != ParticipantRole::Anchor)
                .map(|participant| format!("#{}", participant.id.get()))
                .collect::<Vec<_>>()
                .join(", ");
            output.push_str(&format!(
                "  {} pagerank={:.4} anchor:{anchor} members:[{members}] relation:{}\n",
                hyperedge.kind, hyperedge.pagerank, hyperedge.relation_id,
            ));
        }
    }

    if !report.blast_radius.callers.is_empty() || !report.blast_radius.tests.is_empty() {
        output.push_str("blast radius\n");
        for caller in &report.blast_radius.callers {
            output.push_str(&format!(
                "  caller {} ({})\n",
                caller.qualified_name, caller.path
            ));
        }
        for caller in &report.blast_radius.tests {
            output.push_str(&format!(
                "  test {} ({})\n",
                caller.qualified_name, caller.path
            ));
        }
    }

    if !report.call_flow.is_empty() {
        output.push_str("call flow\n");
        for hop in &report.call_flow {
            output.push_str(&format!(
                "  #{} -> #{}\n",
                hop.from_id.get(),
                hop.to_id.get()
            ));
        }
    }

    output.push_str("files\n");
    if report.files.is_empty() {
        output.push_str("  none\n");
    }
    for file in &report.files {
        output.push_str(&format!("  {}\n", file.path));
        if let Some(skeleton) = &file.skeleton {
            for line in skeleton.lines() {
                output.push_str("    ");
                output.push_str(line);
                output.push('\n');
            }
        }
    }

    output.push_str(&format!(
        "budget {}/{} chars{}\n",
        report.budget.total_chars,
        report.budget.max_total_chars,
        if report.budget.truncated {
            " (truncated)"
        } else {
            ""
        },
    ));
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
            push_query_value(&mut output, value);
        }
    }
    output
}

/// Formats one expanded query value — its raw text, resolved symbol, and any
/// call edge — into `output`.
fn push_query_value(output: &mut String, value: &ExpandedQueryValue) {
    output.push_str(&format!("  {}\n", value.raw));
    if let Some(symbol) = &value.symbol {
        output.push_str(&format!("    {}\n", symbol_inline(symbol)));
        output.push_str(&format!(
            "    defined at {}\n",
            location_range(&symbol.definition)
        ));
    }
    if let Some(edge) = &value.call_edge {
        push_call_edge(output, edge);
    }
}

/// Formats one call edge — its endpoints and originating call site — into
/// `output`.
fn push_call_edge(output: &mut String, edge: &CallEdgeSummary) {
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
    output.push_str(&format!(
        "use oxcode symbols {:?} --path . to search candidates\n",
        selector_search_query(selector)
    ));
    output
}

/// Formats one selector miss for agent-facing CLI output.
#[must_use]
pub fn format_selector_not_found(selector: &str) -> String {
    format!(
        "selector {selector:?} did not match any symbol\nuse oxcode symbols {:?} --path . to search candidates\n",
        selector_search_query(selector)
    )
}

fn push_symbol_block(output: &mut String, label: &str, symbol: &SymbolSummary) {
    output.push_str(&format!("{label} element:{}\n", symbol.id));
    output.push_str(&format!("  {}\n", symbol.qualified_name));
    output.push_str(&format!("  {}\n", symbol.kind));
    output.push_str(&format!(
        "  defined at {}\n",
        location_range(&symbol.definition)
    ));
    push_symbol_metadata(output, symbol, "  ");
}

fn push_symbol_metadata(output: &mut String, symbol: &SymbolSummary, indent: &str) {
    if let Some(signature) = &symbol.signature {
        output.push_str(&format!("{indent}signature {signature}\n"));
    }
    if let Some(docstring) = &symbol.docstring {
        output.push_str(&format!(
            "{indent}docs {}\n",
            one_line_preview(docstring, 240)
        ));
    }
    if let Some(source_preview) = &symbol.source_preview {
        output.push_str(&format!(
            "{indent}preview {}\n",
            one_line_preview(source_preview, 240)
        ));
    }
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

fn selector_search_query(selector: &str) -> &str {
    selector
        .strip_prefix("name:")
        .or_else(|| selector.strip_prefix("element:"))
        .or_else(|| selector.strip_prefix("file:"))
        .unwrap_or(selector)
}

fn one_line_preview(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(max_chars).collect()
}
