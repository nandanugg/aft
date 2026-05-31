use serde::Serialize;
use serde_json::{json, Value};

use crate::callgraph::{
    CallTreeNode, CallersResult, DispatchedByResult, DispatchesResult, GraphEdge, GraphEdgeKind,
    ImpactResult, ImplementationsResult, TraceDataResult, TraceToResult, TraceToSymbolResult,
    WritersResult,
};
use crate::protocol::{RawRequest, Response};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Compact,
}

impl OutputFormat {
    pub fn from_request(req: &RawRequest) -> Self {
        req.params
            .get("output")
            .or_else(|| req.params.get("format"))
            .and_then(Value::as_str)
            .and_then(Self::from_name)
            .unwrap_or(Self::Json)
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "json" | "structured" => Some(Self::Json),
            "compact" | "text" | "dense" => Some(Self::Compact),
            _ => None,
        }
    }
}

pub trait OutputProcessor<T> {
    fn process(&self, value: &T) -> Value;
}

#[derive(Debug, Clone, Copy)]
pub struct JsonOutput;

impl<T> OutputProcessor<T> for JsonOutput
where
    T: Serialize,
{
    fn process(&self, value: &T) -> Value {
        serde_json::to_value(value).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompactGraphOutput;

pub fn graph_response<T>(req: &RawRequest, value: &T) -> Response
where
    T: Serialize,
    CompactGraphOutput: OutputProcessor<T>,
{
    let body = match OutputFormat::from_request(req) {
        OutputFormat::Json => JsonOutput.process(value),
        OutputFormat::Compact => apply_compact_options(req, CompactGraphOutput.process(value)),
    };
    Response::success(&req.id, body)
}

fn compact_value(text: String) -> Value {
    json!({
        "output": "compact",
        "text": text,
    })
}

#[derive(Debug, Clone)]
struct CompactOptions {
    limit_chars: usize,
    cursor: usize,
    filter: Option<String>,
}

impl CompactOptions {
    const DEFAULT_LIMIT_CHARS: usize = 6_000;
    const MAX_LIMIT_CHARS: usize = 50_000;

    fn from_request(req: &RawRequest) -> Self {
        let limit_chars = req
            .params
            .get("output_limit_chars")
            .or_else(|| req.params.get("limit_chars"))
            .or_else(|| req.params.get("max_chars"))
            .and_then(value_as_usize)
            .map(|value| value.clamp(1, Self::MAX_LIMIT_CHARS))
            .unwrap_or(Self::DEFAULT_LIMIT_CHARS);
        let cursor = req
            .params
            .get("output_cursor")
            .or_else(|| req.params.get("cursor"))
            .and_then(value_as_usize)
            .unwrap_or(0);
        let filter = req
            .params
            .get("output_filter")
            .or_else(|| req.params.get("compact_filter"))
            .or_else(|| req.params.get("filter"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        Self {
            limit_chars,
            cursor,
            filter,
        }
    }
}

fn value_as_usize(value: &Value) -> Option<usize> {
    if let Some(value) = value.as_u64() {
        return usize::try_from(value).ok();
    }
    value.as_str()?.trim().parse::<usize>().ok()
}

fn apply_compact_options(req: &RawRequest, mut value: Value) -> Value {
    let Some(text) = value.get("text").and_then(Value::as_str) else {
        return value;
    };
    let options = CompactOptions::from_request(req);
    let filtered = filter_compact_text(text, options.filter.as_deref());
    let total_chars = filtered.chars().count();
    let cursor = options.cursor.min(total_chars);
    let end = page_end_at_line_boundary(&filtered, cursor, options.limit_chars, total_chars);
    let page = slice_chars(&filtered, cursor, end);
    let has_more = end < total_chars;

    if let Some(obj) = value.as_object_mut() {
        obj.insert("text".to_string(), Value::String(page));
        obj.insert("cursor".to_string(), Value::String(cursor.to_string()));
        obj.insert("limit_chars".to_string(), json!(options.limit_chars));
        obj.insert("total_chars".to_string(), json!(total_chars));
        obj.insert("has_more".to_string(), json!(has_more));
        obj.insert(
            "omitted_chars".to_string(),
            json!(total_chars.saturating_sub(end)),
        );
        if has_more {
            obj.insert("next_cursor".to_string(), Value::String(end.to_string()));
        } else {
            obj.remove("next_cursor");
        }
        if let Some(filter) = options.filter {
            obj.insert("filter".to_string(), Value::String(filter));
            obj.insert("filtered".to_string(), json!(true));
        } else {
            obj.insert("filtered".to_string(), json!(false));
        }
    }

    value
}

fn filter_compact_text(text: &str, filter: Option<&str>) -> String {
    let Some(filter) = filter else {
        return text.to_string();
    };
    let needle = filter.to_lowercase();
    let mut out = text
        .lines()
        .filter(|line| line.to_lowercase().contains(&needle))
        .collect::<Vec<_>>()
        .join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn slice_chars(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn page_end_at_line_boundary(
    text: &str,
    cursor: usize,
    limit_chars: usize,
    total_chars: usize,
) -> usize {
    let hard_end = (cursor + limit_chars).min(total_chars);
    if hard_end == total_chars {
        return hard_end;
    }

    let mut last_line_end = None;
    for (idx, ch) in text
        .chars()
        .enumerate()
        .skip(cursor)
        .take(hard_end.saturating_sub(cursor))
    {
        if ch == '\n' {
            last_line_end = Some(idx + 1);
        }
    }

    last_line_end
        .filter(|end| *end > cursor)
        .unwrap_or(hard_end)
}

fn edge_prefix(edge: Option<&GraphEdge>) -> String {
    let Some(edge) = edge else {
        return String::new();
    };
    let mut out = edge_kind_label(edge.kind).to_string();
    if let Some(key) = edge.dispatch_key.as_deref() {
        out.push('[');
        out.push_str(key);
        out.push(']');
    }
    if let Some(via) = edge.dispatch_via.as_deref() {
        out.push_str(" via ");
        out.push_str(via);
    }
    if let Some(receiver) = edge.receiver.as_deref() {
        out.push_str(" receiver ");
        out.push_str(receiver);
    }
    out
}

fn edge_kind_label(kind: GraphEdgeKind) -> &'static str {
    match kind {
        GraphEdgeKind::DirectCall => "call",
        GraphEdgeKind::ConcreteCall => "concrete",
        GraphEdgeKind::InterfaceCall => "interface",
        GraphEdgeKind::DispatchRegistration => "dispatch",
        GraphEdgeKind::Goroutine => "go",
        GraphEdgeKind::Defer => "defer",
        GraphEdgeKind::Implements => "implements",
        GraphEdgeKind::Writes => "writes",
    }
}

impl OutputProcessor<CallersResult> for CompactGraphOutput {
    fn process(&self, value: &CallersResult) -> Value {
        let mut out = format!(
            "callers {} {} total={} files={} scanned={}\n",
            value.symbol,
            value.file,
            value.total_callers,
            value.callers.len(),
            value.scanned_files
        );
        for group in &value.callers {
            out.push_str(&format!("  {}:\n", group.file));
            for caller in &group.callers {
                out.push_str(&format!(
                    "    {} {}:{} {}\n",
                    edge_prefix(Some(&caller.edge)),
                    caller.symbol,
                    caller.line,
                    group.file
                ));
            }
        }
        if value.depth_limited {
            out.push_str(&format!("  depth_limited truncated={}\n", value.truncated));
        }
        compact_value(out)
    }
}

impl OutputProcessor<TraceToResult> for CompactGraphOutput {
    fn process(&self, value: &TraceToResult) -> Value {
        let mut out = format!(
            "trace_to {} {} paths={} entries={} truncated={}\n",
            value.target_symbol,
            value.target_file,
            value.total_paths,
            value.entry_points_found,
            value.truncated_paths
        );
        for (idx, path) in value.paths.iter().enumerate() {
            out.push_str(&format!("  path {}:\n", idx + 1));
            for (hop_idx, hop) in path.hops.iter().enumerate() {
                if hop_idx == 0 {
                    out.push_str(&format!("    {} {}:{}\n", hop.symbol, hop.file, hop.line));
                } else {
                    out.push_str(&format!(
                        "    -> {} {} {}:{}\n",
                        edge_prefix(hop.incoming_edge.as_ref()),
                        hop.symbol,
                        hop.file,
                        hop.line
                    ));
                }
            }
        }
        if value.max_depth_reached {
            out.push_str("  max_depth_reached\n");
        }
        compact_value(out)
    }
}

impl OutputProcessor<ImpactResult> for CompactGraphOutput {
    fn process(&self, value: &ImpactResult) -> Value {
        let mut out = format!(
            "impact {} {} affected={} files={}\n",
            value.symbol, value.file, value.total_affected, value.affected_files
        );
        for caller in &value.callers {
            out.push_str(&format!(
                "  {} {} {}:{}\n",
                edge_prefix(Some(&caller.edge)),
                caller.caller_symbol,
                caller.caller_file,
                caller.line
            ));
            if let Some(expr) = caller.call_expression.as_deref() {
                out.push_str(&format!("    {}\n", expr));
            }
        }
        if value.depth_limited {
            out.push_str(&format!("  depth_limited truncated={}\n", value.truncated));
        }
        compact_value(out)
    }
}

impl OutputProcessor<CallTreeNode> for CompactGraphOutput {
    fn process(&self, value: &CallTreeNode) -> Value {
        let mut out = String::new();
        render_tree(value, 0, &mut out);
        compact_value(out)
    }
}

fn render_tree(node: &CallTreeNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    if depth == 0 {
        out.push_str(&format!(
            "{}{} {}:{}\n",
            indent, node.name, node.file, node.line
        ));
    } else {
        out.push_str(&format!(
            "{}{} {} {}:{}\n",
            indent,
            edge_prefix(node.incoming_edge.as_ref()),
            node.name,
            node.file,
            node.line
        ));
    }
    if node.depth_limited {
        out.push_str(&format!(
            "{}  depth_limited truncated={}\n",
            indent, node.truncated
        ));
    }
    for child in &node.children {
        render_tree(child, depth + 1, out);
    }
}

impl OutputProcessor<TraceDataResult> for CompactGraphOutput {
    fn process(&self, value: &TraceDataResult) -> Value {
        let mut out = format!(
            "trace_data {} {}:{} hops={}\n",
            value.expression,
            value.origin_file,
            value.origin_symbol,
            value.hops.len()
        );
        for hop in &value.hops {
            let approx = if hop.approximate { " approximate" } else { "" };
            out.push_str(&format!(
                "  {} {}.{} {}:{}{}\n",
                hop.flow_type, hop.symbol, hop.variable, hop.file, hop.line, approx
            ));
        }
        if value.depth_limited {
            out.push_str("  depth_limited\n");
        }
        compact_value(out)
    }
}

impl OutputProcessor<TraceToSymbolResult> for CompactGraphOutput {
    fn process(&self, value: &TraceToSymbolResult) -> Value {
        let mut out = String::from("trace_to_symbol");
        if let Some(path) = &value.path {
            out.push_str(&format!(" hops={}\n", path.len()));
            for hop in path {
                out.push_str(&format!("  {} {}:{}\n", hop.symbol, hop.file, hop.line));
            }
        } else {
            out.push_str(" no_path");
            if let Some(reason) = value.reason.as_deref() {
                out.push_str(&format!(" reason={reason}"));
            }
            out.push('\n');
        }
        if !value.complete {
            out.push_str("  incomplete\n");
        }
        compact_value(out)
    }
}

impl OutputProcessor<DispatchesResult> for CompactGraphOutput {
    fn process(&self, value: &DispatchesResult) -> Value {
        compact_value(value.render_text())
    }
}

impl OutputProcessor<Vec<DispatchesResult>> for CompactGraphOutput {
    fn process(&self, value: &Vec<DispatchesResult>) -> Value {
        compact_value(
            value
                .iter()
                .map(DispatchesResult::render_text)
                .collect::<Vec<_>>()
                .join(""),
        )
    }
}

impl OutputProcessor<DispatchedByResult> for CompactGraphOutput {
    fn process(&self, value: &DispatchedByResult) -> Value {
        compact_value(value.render_text())
    }
}

impl OutputProcessor<ImplementationsResult> for CompactGraphOutput {
    fn process(&self, value: &ImplementationsResult) -> Value {
        compact_value(value.render_text())
    }
}

impl OutputProcessor<WritersResult> for CompactGraphOutput {
    fn process(&self, value: &WritersResult) -> Value {
        compact_value(value.render_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(params: Value) -> RawRequest {
        RawRequest {
            id: "req-1".to_string(),
            command: "dispatches".to_string(),
            lsp_hints: None,
            session_id: None,
            params,
        }
    }

    #[test]
    fn graph_response_defaults_to_structured_json_contract() {
        let result = DispatchesResult {
            key: "provider_settlement:file_id".to_string(),
            handlers: Vec::new(),
        };

        let response = graph_response(&request(json!({})), &result);
        let value = serde_json::to_value(response).expect("response json");

        assert_eq!(value["success"], true);
        assert_eq!(value["key"], "provider_settlement:file_id");
        assert_eq!(value["handlers"].as_array().unwrap().len(), 0);
        assert!(value.get("text").is_none());
        assert!(value.get("output").is_none());
    }

    #[test]
    fn graph_response_compact_uses_output_projection() {
        let result = DispatchesResult {
            key: "provider_settlement:file_id".to_string(),
            handlers: Vec::new(),
        };

        let response = graph_response(&request(json!({ "output": "compact" })), &result);
        let value = serde_json::to_value(response).expect("response json");

        assert_eq!(value["success"], true);
        assert_eq!(value["output"], "compact");
        assert!(value["text"]
            .as_str()
            .unwrap()
            .contains("dispatches key=provider_settlement:file_id"));
        assert!(value.get("handlers").is_none());
    }

    #[test]
    fn graph_response_compact_paginates_with_cursor() {
        let result = DispatchesResult {
            key: "provider_settlement:file_id".to_string(),
            handlers: Vec::new(),
        };

        let response = graph_response(
            &request(json!({
                "output": "compact",
                "output_limit_chars": 20,
            })),
            &result,
        );
        let first = serde_json::to_value(response).expect("response json");
        assert_eq!(first["has_more"], true);
        assert_eq!(first["cursor"], "0");
        assert_eq!(first["next_cursor"], "20");
        assert_eq!(first["text"].as_str().unwrap().chars().count(), 20);

        let response = graph_response(
            &request(json!({
                "output": "compact",
                "output_limit_chars": 20,
                "output_cursor": first["next_cursor"].as_str().unwrap(),
            })),
            &result,
        );
        let second = serde_json::to_value(response).expect("response json");
        assert_eq!(second["cursor"], "20");
        assert_ne!(first["text"], second["text"]);
    }

    #[test]
    fn graph_response_compact_filters_case_insensitively_before_pagination() {
        let result = DispatchesResult {
            key: "provider_settlement:file_id".to_string(),
            handlers: Vec::new(),
        };

        let response = graph_response(
            &request(json!({
                "output": "compact",
                "output_filter": "NO HANDLERS",
                "output_limit_chars": 200,
            })),
            &result,
        );
        let value = serde_json::to_value(response).expect("response json");

        assert_eq!(value["filtered"], true);
        assert_eq!(value["filter"], "NO HANDLERS");
        let text = value["text"].as_str().unwrap();
        assert!(text.contains("no handlers found"));
        assert!(!text.contains("dispatches key="));
    }
}
