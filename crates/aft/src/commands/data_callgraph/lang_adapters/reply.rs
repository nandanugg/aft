//! Reply finishing (owner: R2): `finish_reply`.
//!
//! Called once per graph command reply, after upstream built `response` from `graph`. It adds
//! only these keys (so a comparison with a stock reply can strip them):
//! - `adapters`: the status of every language that has an adapter for the root;
//! - `adapter_reason` and `complete: false`, when a language with an adapter is not fresh or
//!   partial (also for `call_tree`, `trace_to` and `trace_data`, which have no flag upstream);
//! - `text`: upstream's own text form (`subc_format::format_callgraph`), with adapter-added
//!   targets marked `adapter:<language> (<kind>)`, followed by our status lines. A mark also
//!   carries the tags of the adapter's site: `adapter:go (interface) [in_loop, in_error_branch]`.
//!   A call upstream resolved itself, at a site that has tags, gets the tags alone: `[in_loop]`.
//!
//! Nothing else in the reply changes. A failed reply passes through untouched.

use serde_json::Value;

use crate::commands::data_callgraph::OpsGraph;
use crate::protocol::{RawRequest, Response};

use super::graph::ServedEdge;

/// The request parameter that asks `call_tree` text to list every unresolved leaf.
const INCLUDE_UNRESOLVED_PARAM: &str = "includeUnresolved";

pub(crate) fn finish_reply(req: &RawRequest, operation: &str, graph: &OpsGraph, mut response: Response) -> Response {
    if !response.success {
        return response;
    }
    let Some(data) = response.data.as_object_mut() else { return response };

    let statuses = graph.statuses();
    let not_served: Vec<String> =
        statuses.iter().filter(|status| !status.serves_edges()).map(|status| status.reason_line()).collect();
    if !not_served.is_empty() {
        data.insert("complete".to_string(), Value::Bool(false));
        data.insert("adapter_reason".to_string(), Value::String(not_served.join("; ")));
    }

    // The text is built from the reply as upstream made it (plus `complete`), with the
    // adapter-added targets marked on a copy, so the marks never reach the JSON.
    let mut for_text = Value::Object(data.clone());
    mark_adapter_edges(operation, &mut for_text, &graph.served_edges());
    let include_unresolved = req.params.get(INCLUDE_UNRESOLVED_PARAM).and_then(Value::as_bool).unwrap_or(false);
    let mut lines = vec![crate::subc_format::format_callgraph(operation, &for_text, include_unresolved)];
    lines.extend(status_lines(graph));

    data.insert("adapters".to_string(), serde_json::to_value(statuses).unwrap_or(Value::Null));
    data.insert("text".to_string(), Value::String(lines.join("\n")));
    response
}

/// One line per language: its status, what it added to this answer, and what was left out.
fn status_lines(graph: &OpsGraph) -> Vec<String> {
    let served = graph.served_edges();
    let hidden = graph.hidden_counts();
    graph
        .statuses()
        .iter()
        .map(|status| {
            let mut line = format!("adapter {}", status.reason_line());
            let added = served.iter().filter(|edge| edge.added && edge.language == status.language).count();
            if added > 0 {
                line.push_str(&format!("; {added} adapter edge{} considered", if added == 1 { "" } else { "s" }));
            }
            if let Some((_, count)) = hidden.iter().find(|(language, _)| *language == status.language) {
                let tags = graph.hidden_tags(&status.language).join(", ");
                line.push_str(&format!("; {count} target{} tagged {tags} left out", if *count == 1 { "" } else { "s" }));
            }
            line
        })
        .collect()
}

/// Marks the adapter-added entries of a reply, for the text only.
///
/// A mark is `ServedEdge::mark`. Where the reply names the call line (`callers`, `impact`), one
/// edge matches. Where it does not (`call_tree`, paths), every edge that joins the two symbols
/// matches: the first added edge names the adapter, and the tags of all of them are united.
///
/// - `callers`: the entry gets `via adapter:go (kind)`.
/// - `call_tree`: the child's name gets the mark after it.
/// - `impact`: the caller's symbol gets the mark after it.
/// - Paths (`trace_to`, `trace_to_symbol`, `trace_data`): the symbol of a hop gets the
///   mark when an adapter edge joins it to the hop before it.
fn mark_adapter_edges(operation: &str, reply: &mut Value, edges: &[ServedEdge]) {
    if edges.is_empty() {
        return;
    }
    match operation {
        "callers" => mark_callers(reply, edges),
        "call_tree" => mark_call_tree(reply, edges),
        "impact" => mark_impact(reply, edges),
        _ => mark_hops(reply, edges),
    }
}

fn mark_callers(value: &mut Value, edges: &[ServedEdge]) {
    match value {
        Value::Object(object) => {
            let group_file = object.get("file").and_then(Value::as_str).map(str::to_string);
            if let (Some(file), Some(Value::Array(entries))) = (group_file, object.get_mut("callers")) {
                for entry in entries.iter_mut() {
                    let (Some(symbol), Some(line)) = (
                        entry.get("symbol").and_then(Value::as_str).map(str::to_string),
                        entry.get("line").and_then(Value::as_u64),
                    ) else {
                        continue;
                    };
                    let matching = edges.iter().filter(|edge| {
                        edge.caller_file == file && edge.caller_symbol == symbol && u64::from(edge.call_line) == line
                    });
                    if let (Some(mark), Some(entry)) = (combined_mark(matching), entry.as_object_mut()) {
                        entry.entry("via").or_insert_with(|| Value::String(mark));
                    }
                }
            }
            for child in object.values_mut() {
                mark_callers(child, edges);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| mark_callers(item, edges)),
        _ => {}
    }
}

fn mark_call_tree(node: &mut Value, edges: &[ServedEdge]) {
    let parent_file = node.get("file").and_then(Value::as_str).map(str::to_string);
    let Some(Value::Array(children)) = node.get_mut("children") else { return };
    for child in children.iter_mut() {
        let mark = {
            let name = child.get("name").and_then(Value::as_str);
            let file = child.get("file").and_then(Value::as_str);
            let line = child.get("line").and_then(Value::as_u64);
            combined_mark(edges.iter().filter(|edge| {
                Some(edge.caller_file.as_str()) == parent_file.as_deref()
                    && Some(edge.target_file.as_str()) == file
                    && Some(edge.target_symbol.as_str()) == name
                    && Some(u64::from(edge.target_line)) == line
            }))
        };
        if let (Some(mark), Some(Value::String(name))) = (mark, child.get_mut("name")) {
            *name = format!("{name} {mark}");
        }
        mark_call_tree(child, edges);
    }
}

fn mark_impact(reply: &mut Value, edges: &[ServedEdge]) {
    let Some(Value::Array(callers)) = reply.get_mut("callers") else { return };
    for caller in callers.iter_mut() {
        let mark = {
            let symbol = caller.get("caller_symbol").and_then(Value::as_str);
            let file = caller.get("caller_file").and_then(Value::as_str);
            let line = caller.get("line").and_then(Value::as_u64);
            combined_mark(edges.iter().filter(|edge| {
                Some(edge.caller_symbol.as_str()) == symbol
                    && Some(edge.caller_file.as_str()) == file
                    && Some(u64::from(edge.call_line)) == line
            }))
        };
        if let (Some(mark), Some(Value::String(symbol))) = (mark, caller.get_mut("caller_symbol")) {
            *symbol = format!("{symbol} {mark}");
        }
    }
}

fn mark_hops(value: &mut Value, edges: &[ServedEdge]) {
    match value {
        Value::Array(items) => {
            let joined = |a: &Value, b: &Value| {
                let key = |value: &Value| {
                    Some((value.get("file")?.as_str()?.to_string(), value.get("symbol")?.as_str()?.to_string()))
                };
                let (Some(a), Some(b)) = (key(a), key(b)) else { return None };
                Some(edges.iter().filter(move |edge| {
                    let from = (edge.caller_file.clone(), edge.caller_symbol.clone());
                    let to = (edge.target_file.clone(), edge.target_symbol.clone());
                    (from == a && to == b) || (from == b && to == a)
                }))
            };
            let marks: Vec<Option<String>> = (0..items.len())
                .map(|index| {
                    let previous = items.get(index.checked_sub(1)?)?;
                    combined_mark(joined(previous, &items[index])?)
                })
                .collect();
            for (item, mark) in items.iter_mut().zip(marks) {
                if let (Some(mark), Some(Value::String(symbol))) = (mark, item.get_mut("symbol")) {
                    *symbol = format!("{symbol} {mark}");
                }
            }
            items.iter_mut().for_each(|item| mark_hops(item, edges));
        }
        Value::Object(object) => object.values_mut().for_each(|child| mark_hops(child, edges)),
        _ => {}
    }
}

/// The mark for the edges that match one place in a reply: the first added edge names the adapter,
/// and the tags of all of them are united, each once, in the order met. `None` when there is
/// nothing to say.
fn combined_mark<'a>(matching: impl Iterator<Item = &'a ServedEdge>) -> Option<String> {
    let matching: Vec<&ServedEdge> = matching.collect();
    let mut tags: Vec<&str> = Vec::new();
    for tag in matching.iter().flat_map(|edge| edge.tags.iter()) {
        if !tags.contains(&tag.as_str()) {
            tags.push(tag);
        }
    }
    let united = ServedEdge {
        tags: tags.into_iter().map(str::to_string).collect(),
        ..(*matching.iter().find(|edge| edge.added).or(matching.first())?).clone()
    };
    united.mark()
}
