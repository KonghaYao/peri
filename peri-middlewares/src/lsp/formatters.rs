use lsp_types::{Location, LocationLink, SymbolKind};
use peri_resources::lsp::protocol::lsp_types;
use peri_resources::lsp::uri::uri_to_path as lsp_uri_to_path;

/// 将 LSP Location 转为 `path:line:col` 格式（1-based）
fn format_location(loc: &Location) -> String {
    let path = uri_to_path(&loc.uri);
    let line = loc.range.start.line + 1;
    let col = loc.range.start.character + 1;
    format!("{path}:{line}:{col}")
}

/// 将 file:// URI 转为文件路径（percent-decode）
fn uri_to_path(uri: &lsp_types::Uri) -> String {
    lsp_uri_to_path(uri.as_str())
}

/// SymbolKind → 可读名称
fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "File",
        SymbolKind::MODULE => "Module",
        SymbolKind::NAMESPACE => "Namespace",
        SymbolKind::PACKAGE => "Package",
        SymbolKind::CLASS => "Class",
        SymbolKind::METHOD => "Method",
        SymbolKind::PROPERTY => "Property",
        SymbolKind::FIELD => "Field",
        SymbolKind::CONSTRUCTOR => "Constructor",
        SymbolKind::ENUM => "Enum",
        SymbolKind::INTERFACE => "Interface",
        SymbolKind::FUNCTION => "Function",
        SymbolKind::VARIABLE => "Variable",
        SymbolKind::CONSTANT => "Constant",
        SymbolKind::STRUCT => "Struct",
        SymbolKind::EVENT => "Event",
        SymbolKind::OPERATOR => "Operator",
        SymbolKind::TYPE_PARAMETER => "TypeParameter",
        _ => "Unknown",
    }
}

/// 从 MarkedString 提取文本
fn marked_string_to_text(ms: &lsp_types::MarkedString) -> String {
    match ms {
        lsp_types::MarkedString::String(s) => s.clone(),
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("```{}\n{}```", ls.language, ls.value)
        }
    }
}

/// 格式化 goToDefinition / goToImplementation 结果
pub fn format_locations(locations: &[Location]) -> String {
    if locations.is_empty() {
        return "No definition found.".to_string();
    }
    if locations.len() == 1 {
        format!("Defined in {}", format_location(&locations[0]))
    } else {
        let mut lines = vec![format!("Found {} definitions:", locations.len())];
        for loc in locations {
            lines.push(format!("  {}", format_location(loc)));
        }
        lines.join("\n")
    }
}

/// 格式化 goToDefinition（支持 Location 或 LocationLink）
pub fn format_definition_result(result: &serde_json::Value) -> String {
    // Location 数组
    if let Ok(locs) = serde_json::from_value::<Vec<Location>>(result.clone()) {
        return format_locations(&locs);
    }
    // LocationLink 数组
    if let Ok(links) = serde_json::from_value::<Vec<LocationLink>>(result.clone()) {
        let locs: Vec<Location> = links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_range,
            })
            .collect();
        return format_locations(&locs);
    }
    // 单个 Location
    if let Ok(loc) = serde_json::from_value::<Location>(result.clone()) {
        return format_locations(&[loc]);
    }
    // 单个 LocationLink
    if let Ok(link) = serde_json::from_value::<LocationLink>(result.clone()) {
        return format_locations(&[Location {
            uri: link.target_uri,
            range: link.target_range,
        }]);
    }
    "No definition found.".to_string()
}

/// 格式化 findReferences 结果
pub fn format_references(result: &serde_json::Value) -> String {
    let locations = match serde_json::from_value::<Vec<Location>>(result.clone()) {
        Ok(locs) => locs,
        Err(_) => return "No references found.".to_string(),
    };

    if locations.is_empty() {
        return "No references found.".to_string();
    }

    let total = locations.len();

    // 按文件分组
    let mut file_groups: Vec<(String, Vec<(u32, u32)>)> = Vec::new();
    for loc in &locations {
        let path = uri_to_path(&loc.uri);
        let line = loc.range.start.line + 1;
        let col = loc.range.start.character + 1;
        if let Some(group) = file_groups.iter_mut().find(|(p, _)| p == &path) {
            group.1.push((line, col));
        } else {
            file_groups.push((path, vec![(line, col)]));
        }
    }

    if file_groups.len() == 1 {
        let (path, entries) = &file_groups[0];
        let lines: Vec<String> = entries
            .iter()
            .map(|(l, c)| format!("  Line {l}:{c}"))
            .collect();
        format!(
            "Found {total} reference{}:\n{path}:\n{}",
            if total != 1 { "s" } else { "" },
            lines.join("\n")
        )
    } else {
        let file_count = file_groups.len();
        let mut lines = vec![format!(
            "Found {total} references across {file_count} files:"
        )];
        for (path, entries) in &file_groups {
            lines.push(format!("\n{path}:"));
            for (l, c) in entries {
                lines.push(format!("  Line {l}:{c}"));
            }
        }
        lines.join("\n")
    }
}

/// 格式化 hover 结果
pub fn format_hover(result: &serde_json::Value) -> String {
    let hover = match serde_json::from_value::<lsp_types::Hover>(result.clone()) {
        Ok(h) => h,
        Err(_) => return "No hover information available.".to_string(),
    };

    let text = match hover.contents {
        lsp_types::HoverContents::Scalar(ref ms) => marked_string_to_text(ms),
        lsp_types::HoverContents::Array(ref arr) => arr
            .iter()
            .map(marked_string_to_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        lsp_types::HoverContents::Markup(ref markup) => markup.value.clone(),
    };

    if text.is_empty() {
        return "No hover information available.".to_string();
    }

    let mut output = String::new();
    if let Some(range) = hover.range {
        let line = range.start.line + 1;
        let col = range.start.character + 1;
        output.push_str(&format!("Hover info at {line}:{col}:\n\n"));
    }
    output.push_str(&text);
    output
}

/// 格式化 documentSymbol 结果
pub fn format_document_symbols(result: &serde_json::Value) -> String {
    use lsp_types::{DocumentSymbol, SymbolInformation};

    // 尝试 DocumentSymbol[] 格式（层级）
    if let Ok(symbols) = serde_json::from_value::<Vec<DocumentSymbol>>(result.clone()) {
        if symbols.is_empty() {
            return "No symbols found in document.".to_string();
        }
        let mut lines = vec!["Document symbols:".to_string()];
        format_doc_symbols_tree(&symbols, 0, &mut lines);
        return lines.join("\n");
    }

    // 尝试 SymbolInformation[] 格式（扁平）
    if let Ok(symbols) = serde_json::from_value::<Vec<SymbolInformation>>(result.clone()) {
        if symbols.is_empty() {
            return "No symbols found in document.".to_string();
        }
        let mut lines = vec!["Document symbols:".to_string()];
        for sym in &symbols {
            let line = sym.location.range.start.line + 1;
            let kind = symbol_kind_name(sym.kind);
            lines.push(format!("  {} ({}) - Line {}", sym.name, kind, line));
        }
        return lines.join("\n");
    }

    "No symbols found in document.".to_string()
}

fn format_doc_symbols_tree(
    symbols: &[lsp_types::DocumentSymbol],
    depth: usize,
    lines: &mut Vec<String>,
) {
    let indent = "  ".repeat(depth);
    for sym in symbols {
        let kind = symbol_kind_name(sym.kind);
        let line = sym.range.start.line + 1;
        lines.push(format!("{}{} ({}) - Line {}", indent, sym.name, kind, line));
        if let Some(children) = &sym.children {
            if !children.is_empty() {
                format_doc_symbols_tree(children, depth + 1, lines);
            }
        }
    }
}

/// 格式化 workspaceSymbol 结果
pub fn format_workspace_symbols(result: &serde_json::Value) -> String {
    let symbols = match serde_json::from_value::<Vec<lsp_types::SymbolInformation>>(result.clone())
    {
        Ok(s) => s,
        Err(_) => return "No symbols found in workspace.".to_string(),
    };

    if symbols.is_empty() {
        return "No symbols found in workspace.".to_string();
    }

    let total = symbols.len();

    // 按文件分组
    let mut file_groups: Vec<(String, Vec<&lsp_types::SymbolInformation>)> = Vec::new();
    for sym in &symbols {
        let path = uri_to_path(&sym.location.uri);
        if let Some(group) = file_groups.iter_mut().find(|(p, _)| p == &path) {
            group.1.push(sym);
        } else {
            file_groups.push((path, vec![sym]));
        }
    }

    let mut lines = vec![format!("Found {total} symbols in workspace:")];

    for (path, syms) in &file_groups {
        lines.push(format!("\n{path}:"));
        for sym in syms {
            let kind = symbol_kind_name(sym.kind);
            let line = sym.location.range.start.line + 1;
            let container = sym.container_name.as_deref().unwrap_or("");
            if !container.is_empty() {
                lines.push(format!(
                    "  {} ({}) - Line {} in {}",
                    sym.name, kind, line, container
                ));
            } else {
                lines.push(format!("  {} ({}) - Line {}", sym.name, kind, line));
            }
        }
    }

    lines.join("\n")
}

/// 格式化 prepareCallHierarchy 结果
pub fn format_call_hierarchy_items(result: &serde_json::Value) -> String {
    let items = match serde_json::from_value::<Vec<lsp_types::CallHierarchyItem>>(result.clone()) {
        Ok(items) => items,
        Err(_) => return "No call hierarchy item found at this position.".to_string(),
    };

    if items.is_empty() {
        return "No call hierarchy item found at this position.".to_string();
    }

    if items.len() == 1 {
        let item = &items[0];
        let path = uri_to_path(&item.uri);
        let kind = symbol_kind_name(item.kind);
        let line = item.range.start.line + 1;
        format!(
            "Call hierarchy item: {} ({}) - {}:{}",
            item.name, kind, path, line
        )
    } else {
        let mut lines = vec![format!("Found {} call hierarchy items:", items.len())];
        for item in &items {
            let path = uri_to_path(&item.uri);
            let kind = symbol_kind_name(item.kind);
            let line = item.range.start.line + 1;
            lines.push(format!("  {} ({}) - {}:{}", item.name, kind, path, line));
        }
        lines.join("\n")
    }
}

/// 格式化 incomingCalls 结果
pub fn format_incoming_calls(calls: &[lsp_types::CallHierarchyIncomingCall]) -> String {
    if calls.is_empty() {
        return "No incoming calls found (nothing calls this function).".to_string();
    }

    let total: usize = calls.iter().map(|c| c.from_ranges.len()).sum();
    let mut lines = vec![format!(
        "Found {total} incoming call{}:",
        if total != 1 { "s" } else { "" }
    )];

    // 按文件分组
    let mut file_groups: Vec<(String, Vec<(&str, &str, u32, Vec<(u32, u32)>)>)> = Vec::new();
    for call in calls {
        let path = uri_to_path(&call.from.uri);
        let kind = symbol_kind_name(call.from.kind);
        let line = call.from.range.start.line + 1;
        let name = &call.from.name;
        let ranges: Vec<(u32, u32)> = call
            .from_ranges
            .iter()
            .map(|r| (r.start.line + 1, r.start.character + 1))
            .collect();

        if let Some(group) = file_groups.iter_mut().find(|(p, _)| p == &path) {
            group.1.push((name, kind, line, ranges));
        } else {
            file_groups.push((path, vec![(name, kind, line, ranges)]));
        }
    }

    for (path, entries) in &file_groups {
        lines.push(format!("\n{path}:"));
        for (name, kind, line, ranges) in entries {
            let sites: Vec<String> = ranges.iter().map(|(l, c)| format!("{l}:{c}")).collect();
            lines.push(format!(
                "  {} ({}) - Line {} [calls at: {}]",
                name,
                kind,
                line,
                sites.join(", ")
            ));
        }
    }

    lines.join("\n")
}

/// 格式化 outgoingCalls 结果
pub fn format_outgoing_calls(calls: &[lsp_types::CallHierarchyOutgoingCall]) -> String {
    if calls.is_empty() {
        return "No outgoing calls found (this function calls nothing).".to_string();
    }

    let total = calls.len();
    let mut lines = vec![format!(
        "Found {total} outgoing call{}:",
        if total != 1 { "s" } else { "" }
    )];

    // 按文件分组
    let mut file_groups: Vec<(String, Vec<(&str, &str, u32, Vec<(u32, u32)>)>)> = Vec::new();
    for call in calls {
        let path = uri_to_path(&call.to.uri);
        let kind = symbol_kind_name(call.to.kind);
        let line = call.to.range.start.line + 1;
        let name = &call.to.name;
        let ranges: Vec<(u32, u32)> = call
            .from_ranges
            .iter()
            .map(|r| (r.start.line + 1, r.start.character + 1))
            .collect();

        if let Some(group) = file_groups.iter_mut().find(|(p, _)| p == &path) {
            group.1.push((name, kind, line, ranges));
        } else {
            file_groups.push((path, vec![(name, kind, line, ranges)]));
        }
    }

    for (path, entries) in &file_groups {
        lines.push(format!("\n{path}:"));
        for (name, kind, line, ranges) in entries {
            let sites: Vec<String> = ranges.iter().map(|(l, c)| format!("{l}:{c}")).collect();
            lines.push(format!(
                "  {} ({}) - Line {} [called from: {}]",
                name,
                kind,
                line,
                sites.join(", ")
            ));
        }
    }

    lines.join("\n")
}

/// 格式化诊断结果
pub fn format_diagnostics(entries: &[peri_resources::lsp::diagnostics::DiagnosticEntry]) -> String {
    if entries.is_empty() {
        return "No diagnostics found.".to_string();
    }

    // 按文件分组
    let mut file_groups: std::collections::HashMap<
        String,
        Vec<&peri_resources::lsp::diagnostics::DiagnosticEntry>,
    > = std::collections::HashMap::new();
    for entry in entries {
        file_groups
            .entry(entry.file_uri.clone())
            .or_default()
            .push(entry);
    }

    let mut lines = Vec::new();
    for (file_uri, entries) in &file_groups {
        let path = file_uri.strip_prefix("file://").unwrap_or(file_uri);
        lines.push(format!("{path}:"));
        for entry in entries {
            let severity = match entry.severity {
                peri_resources::lsp::diagnostics::DiagnosticSeverity::Error => "Error",
                peri_resources::lsp::diagnostics::DiagnosticSeverity::Warning => "Warning",
                peri_resources::lsp::diagnostics::DiagnosticSeverity::Information => "Info",
                peri_resources::lsp::diagnostics::DiagnosticSeverity::Hint => "Hint",
            };
            lines.push(format!(
                "  {}:{}: [{}] {}",
                entry.line, entry.character, severity, entry.message
            ));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
#[path = "formatters_test.rs"]
mod tests;
