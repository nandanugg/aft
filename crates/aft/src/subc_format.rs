//! Agent-facing text formatters for subc-mode tool results (parity with TS plugins).

use crate::protocol::Response;
use serde_json::Value;

const MAX_UNCHECKED_FILES_IN_FOOTER: usize = 10;

fn is_core_agent_tool(bare_name: &str) -> bool {
    matches!(
        bare_name,
        "status" | "read" | "write" | "edit" | "grep" | "search" | "outline" | "inspect"
    )
}

/// Render the text block for a tool `CallToolResult` from the structured AFT `Response`.
pub fn format_response(
    bare_name: &str,
    response: &Response,
    agent_specified_range: bool,
) -> String {
    if !is_core_agent_tool(bare_name) {
        return serde_json::to_string(response).unwrap_or_else(|_| "{}".to_string());
    }

    let data = &response.data;
    if !response.success {
        return format_error(data);
    }

    match bare_name {
        "edit" | "write" => format_edit_summary(data),
        "read" => format_read(data, agent_specified_range),
        "grep" => field_text_or_fallback(data, "grep: no output"),
        "search" => format_search(data),
        "outline" => format_outline(data),
        "inspect" => format_inspect(data),
        "status" => format_status(data),
        _ => unreachable!("core agent tools are exhaustive"),
    }
}

fn format_error(data: &Value) -> String {
    let code = data
        .get("code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let message = data
        .get("message")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("request failed");
    match code {
        Some(c) => format!("{c}: {message}"),
        None => message.to_string(),
    }
}

fn field_text_or_fallback(data: &Value, fallback: &str) -> String {
    data.get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

fn format_edit_summary(data: &Value) -> String {
    if data.get("rolled_back").and_then(Value::as_bool) == Some(true) {
        return "Edit rolled back: the change produced invalid syntax, so the file was left unchanged."
            .to_string();
    }

    if let Some(n) = data.get("files_modified").and_then(Value::as_u64) {
        let n = n as usize;
        return format!(
            "Applied edits to {} file{}.",
            n,
            if n == 1 { "" } else { "s" }
        );
    }

    if let Some(files) = data.get("total_files").and_then(Value::as_u64) {
        let files = files as usize;
        let reps = data
            .get("total_replacements")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        return format!(
            "Edited {} file{} ({} replacement{}).",
            files,
            if files == 1 { "" } else { "s" },
            reps,
            if reps == 1 { "" } else { "s" }
        );
    }

    let additions = data
        .get("diff")
        .and_then(Value::as_object)
        .and_then(|d| d.get("additions"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let deletions = data
        .get("diff")
        .and_then(Value::as_object)
        .and_then(|d| d.get("deletions"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let counts = format!("+{additions}/-{deletions}");

    if data.get("created").and_then(Value::as_bool) == Some(true) {
        let mut s = format!("Created file ({counts}).");
        if is_truthy_formatted(data) {
            s.push_str(&format_auto_formatted_suffix(data));
        }
        return s;
    }

    let mut detail = counts.clone();
    if let Some(n) = data.get("edits_applied").and_then(Value::as_u64) {
        if n > 1 {
            detail = format!("{counts}, {n} edits");
        }
    } else if let Some(n) = data.get("replacements").and_then(Value::as_u64) {
        if n > 1 {
            detail = format!("{counts}, {n} replacements");
        }
    }

    let mut s = format!("Edited ({detail}).");
    if is_truthy_formatted(data) {
        s.push_str(&format_auto_formatted_suffix(data));
    }
    s
}

fn is_truthy_formatted(data: &Value) -> bool {
    data.get("formatted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn format_auto_formatted_suffix(data: &Value) -> String {
    let reformatted = data.get("reformatted").and_then(Value::as_object);
    if let Some(text) = reformatted
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return format!(
            "\nAuto-formatted — the formatter reflowed your edit. On disk now:\n{text}"
        );
    }
    if reformatted
        .and_then(|r| r.get("extensive"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return " Auto-formatted — extensive reflow; re-read the file before your next anchored edit."
            .to_string();
    }
    " Auto-formatted.".to_string()
}

fn format_read(data: &Value, agent_specified_range: bool) -> String {
    if let Some(entries) = data.get("entries").and_then(Value::as_array) {
        return entries
            .iter()
            .filter_map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join("\n");
    }

    if data.get("binary").and_then(Value::as_bool).unwrap_or(false) {
        return data
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Binary file")
            .to_string();
    }

    let mut text = data
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    text.push_str(&format_read_footer(agent_specified_range, data));
    text
}

fn format_read_footer(agent_specified_range: bool, data: &Value) -> String {
    if agent_specified_range {
        return String::new();
    }
    if !data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return String::new();
    }
    let start = data.get("start_line").and_then(Value::as_u64);
    let end = data.get("end_line").and_then(Value::as_u64);
    let total = data.get("total_lines").and_then(Value::as_u64);
    match (start, end, total) {
        (Some(start), Some(end), Some(total)) => format!(
            "\n(Showing lines {start}-{end} of {total}. Use startLine/endLine to read other sections.)"
        ),
        _ => String::new(),
    }
}

fn format_search(data: &Value) -> String {
    let note = extra_honesty_note(data);
    if let Some(text) = data
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return match note {
            Some(n) => format!("{text}\n{n}"),
            None => text.to_string(),
        };
    }
    note.unwrap_or_else(|| "No results.".to_string())
}

fn extra_honesty_note(data: &Value) -> Option<String> {
    let mut notes = Vec::new();
    if data.get("fully_degraded").and_then(Value::as_bool) == Some(true) {
        notes.push("fully degraded");
    }
    if data.get("complete").and_then(Value::as_bool) == Some(false) {
        notes.push("partial/incomplete");
    }
    if notes.is_empty() {
        None
    } else {
        Some(format!("Search status: {}.", notes.join("; ")))
    }
}

fn format_outline(data: &Value) -> String {
    let text = format_outline_text(data);
    let unchecked: Vec<String> = data
        .get("unchecked_files")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let is_partial = data.get("complete").and_then(Value::as_bool) == Some(false)
        || data.get("walk_truncated").and_then(Value::as_bool) == Some(true)
        || !unchecked.is_empty();

    if !is_partial {
        return text;
    }

    let mut footer = Vec::new();
    if data.get("walk_truncated").and_then(Value::as_bool) == Some(true) {
        let suffix = if !unchecked.is_empty() {
            format!(
                " {} additional files in this directory were not indexed.",
                unchecked.len()
            )
        } else {
            " Some files in this directory were not indexed.".to_string()
        };
        footer.push(format!(
            "⚠ Partial result: walk truncated at 200 files.{suffix}"
        ));
    } else {
        let suffix = if !unchecked.is_empty() {
            format!(
                " {} files in this directory were not indexed.",
                unchecked.len()
            )
        } else {
            " Some files in this directory were not indexed.".to_string()
        };
        footer.push(format!("⚠ Partial result:{suffix}"));
    }

    if !unchecked.is_empty() {
        footer.push("Unchecked files:".to_string());
        for file in unchecked.iter().take(MAX_UNCHECKED_FILES_IN_FOOTER) {
            footer.push(format!("  {file}"));
        }
        let remaining = unchecked
            .len()
            .saturating_sub(MAX_UNCHECKED_FILES_IN_FOOTER);
        if remaining > 0 {
            footer.push(format!("  ... +{remaining} more"));
        }
    }

    if text.is_empty() {
        footer.join("\n")
    } else {
        format!("{text}\n\n{}", footer.join("\n"))
    }
}

fn format_outline_text(data: &Value) -> String {
    let text = data.get("text").and_then(Value::as_str).unwrap_or("");
    let skipped = data.get("skipped_files").and_then(Value::as_array);
    let Some(skipped) = skipped.filter(|s| !s.is_empty()) else {
        return text.to_string();
    };

    let lines: Vec<String> = skipped
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let file = obj.get("file").and_then(Value::as_str)?;
            let reason = obj
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("skipped");
            Some(format!("  {file} — {reason}"))
        })
        .collect();
    if lines.is_empty() {
        return text.to_string();
    }
    let header = if text.is_empty() { "" } else { "\n\n" };
    format!(
        "{text}{header}Skipped {} file(s):\n{}",
        lines.len(),
        lines.join("\n")
    )
}

fn format_inspect(data: &Value) -> String {
    let text = data.get("text").and_then(Value::as_str).unwrap_or("");
    append_rendered_diagnostics(text, data)
}

fn append_rendered_diagnostics(text: &str, data: &Value) -> String {
    if text
        .lines()
        .next()
        .map(|line| {
            let lower = line.to_lowercase();
            lower.starts_with("diagnostics:") || lower.starts_with("diagnostics ")
        })
        .unwrap_or(false)
    {
        return text.to_string();
    }
    let diagnostics = render_inspect_diagnostics(data);
    if diagnostics.is_empty() {
        return text.to_string();
    }
    if text.is_empty() {
        diagnostics
    } else {
        format!("{text}\n\n{diagnostics}")
    }
}

fn render_inspect_diagnostics(data: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(summary_line) = format_diagnostics_summary(data.get("summary")) {
        lines.push(summary_line);
    }

    let detail_lines = format_diagnostics_details(data.get("details"));
    if !detail_lines.is_empty() {
        lines.push("diagnostics details:".to_string());
        for line in detail_lines {
            lines.push(format!("- {line}"));
        }
    }

    lines.join("\n")
}

fn format_diagnostics_summary(summary: Option<&Value>) -> Option<String> {
    let section = summary?.get("diagnostics")?.as_object()?;
    let errors = section.get("errors").and_then(Value::as_u64);
    let warnings = section.get("warnings").and_then(Value::as_u64);
    let info = section.get("info").and_then(Value::as_u64);
    let hints = section.get("hints").and_then(Value::as_u64);
    let has_counts = [errors, warnings, info, hints].iter().any(|v| v.is_some());
    let counts = format!(
        "{} errors, {} warnings, {} info, {} hints",
        errors.unwrap_or(0),
        warnings.unwrap_or(0),
        info.unwrap_or(0),
        hints.unwrap_or(0)
    );
    let status = section.get("status").and_then(Value::as_str);

    match status {
        Some("pending") => {
            if has_counts {
                Some(format!(
                    "diagnostics: {counts} so far — still pending (servers: {})",
                    diagnostics_server_summary(section)
                ))
            } else {
                Some(format!(
                    "diagnostics: pending (servers: {})",
                    diagnostics_server_summary(section)
                ))
            }
        }
        Some("incomplete") => {
            if has_counts {
                Some(format!(
                    "diagnostics: {counts} (incomplete — servers: {})",
                    diagnostics_server_summary(section)
                ))
            } else {
                Some(format!(
                    "diagnostics: unavailable (status incomplete; servers: {})",
                    diagnostics_server_summary(section)
                ))
            }
        }
        _ => {
            if has_counts {
                Some(format!("diagnostics: {counts}"))
            } else {
                None
            }
        }
    }
}

fn diagnostics_server_summary(section: &serde_json::Map<String, Value>) -> String {
    let pending = string_array(section.get("servers_pending"));
    let not_installed = string_array(section.get("servers_not_installed"));
    let mut parts = Vec::new();
    if !pending.is_empty() {
        parts.push(format!("pending: {}", pending.join(", ")));
    }
    if !not_installed.is_empty() {
        parts.push(format!("not installed: {}", not_installed.join(", ")));
    }
    if parts.is_empty() {
        "none reported".to_string()
    } else {
        parts.join("; ")
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn format_diagnostics_details(details: Option<&Value>) -> Vec<String> {
    let Some(details) = details.and_then(Value::as_object) else {
        return Vec::new();
    };
    let Some(diagnostics) = details.get("diagnostics").and_then(Value::as_array) else {
        return Vec::new();
    };
    diagnostics
        .iter()
        .filter_map(|item| {
            let d = item.as_object()?;
            let severity = d
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("information");
            let message = d
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("(no message)");
            let source = d.get("source").and_then(Value::as_str);
            let suffix = source.map(|s| format!(" [{s}]")).unwrap_or_default();
            Some(format!(
                "{} {} {}{}",
                format_diagnostic_location(d),
                severity,
                message,
                suffix
            ))
        })
        .collect()
}

fn format_diagnostic_location(d: &serde_json::Map<String, Value>) -> String {
    let file = d
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("(unknown file)");
    let line = d.get("line").and_then(Value::as_u64);
    let column = d.get("column").and_then(Value::as_u64);
    match (line, column) {
        (None, _) => file.to_string(),
        (Some(line), None) => format!("{file}:{line}"),
        (Some(line), Some(col)) => format!("{file}:{line}:{col}"),
    }
}

fn format_status(data: &Value) -> String {
    if let Some(text) = data
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return text.to_string();
    }
    serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string())
}
