//! Jira operation families and their shared input, ADF, and datasource helpers.

use super::*;

mod attachments;
mod collaboration;
mod comments;
mod issues;
mod transitions;

pub(super) use attachments::*;
pub(super) use collaboration::*;
pub(super) use comments::*;
pub(super) use issues::*;
pub(super) use transitions::*;

// ---------------------------------------------------------------------------------------------------
// Small input helpers
// ---------------------------------------------------------------------------------------------------

pub(super) fn opt_str<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Raw JSON object from `key`, if it is an object.
pub(super) fn raw_obj(input: &Value, key: &str) -> Option<Map<String, Value>> {
    input.get(key).and_then(|v| v.as_object()).cloned()
}

/// The issue key from `key` / `id` / `issue_key` (in order), trimmed.
pub(super) fn issue_key(input: &Value) -> Result<String, String> {
    for k in ["key", "id", "issue_key"] {
        let v = opt_str(input, k).trim();
        if !v.is_empty() {
            return Ok(v.to_string());
        }
    }
    Err("`key` (issue key) required".into())
}

/// Pick a positive limit from the first present key, default-then-cap.
pub(super) fn clamp_limit(input: &Value, keys: &[&str], default: i64, max: i64) -> i64 {
    let mut v = 0;
    for k in keys {
        if let Some(n) = input.get(k).and_then(|x| x.as_i64()) {
            v = n;
            break;
        }
    }
    let v = if v <= 0 { default } else { v };
    v.min(max)
}

/// Build the JQL: explicit `jql` wins, else project/status/query conditions with an order-by tail.
pub(super) fn build_jql(input: &Value) -> String {
    let jql = opt_str(input, "jql").trim();
    if !jql.is_empty() {
        return jql.to_string();
    }
    let mut conds: Vec<String> = Vec::new();
    let project = opt_str(input, "project").trim();
    if !project.is_empty() {
        conds.push(format!("project = {}", jql_string(project)));
    }
    let status = opt_str(input, "status").trim();
    if !status.is_empty() {
        conds.push(format!("status = {}", jql_string(status)));
    }
    let query = {
        let q = opt_str(input, "query").trim();
        if q.is_empty() {
            opt_str(input, "search").trim()
        } else {
            q
        }
    };
    if !query.is_empty() {
        conds.push(format!("text ~ {}", jql_string(query)));
    }
    let order_by = {
        let o = opt_str(input, "order_by").trim();
        if o.is_empty() {
            "updated DESC"
        } else {
            o
        }
    };
    if conds.is_empty() {
        format!("order by {order_by}")
    } else {
        format!("{} order by {order_by}", conds.join(" and "))
    }
}

/// Quote a JQL string literal, escaping backslashes and double quotes.
pub(super) fn jql_string(value: &str) -> String {
    let escaped = value.trim().replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Whether a Jira status/transition-target object matches `target` by name or id (case-insensitive).
pub(super) fn status_matches(status: &Value, target: &str) -> bool {
    let t = target.trim();
    if t.is_empty() {
        return false;
    }
    let name = status.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let id = status.get("id").and_then(|v| v.as_str()).unwrap_or("");
    name.eq_ignore_ascii_case(t) || id.eq_ignore_ascii_case(t)
}

/// Set the typed issue fields shared by create + edit (description/labels/assignee/priority).
pub(super) fn apply_common(fields: &mut Map<String, Value>, input: &Value) {
    let desc = opt_str(input, "description_markdown").trim();
    if !desc.is_empty() {
        fields.insert("description".into(), markdown_to_adf(desc));
    }
    if let Some(arr) = input.get("labels").and_then(|v| v.as_array()) {
        let labels: Vec<String> = arr
            .iter()
            .filter_map(|l| l.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !labels.is_empty() {
            fields.insert("labels".into(), json!(labels));
        }
    }
    let assignee = opt_str(input, "assignee_account_id").trim();
    if !assignee.is_empty() {
        fields.insert("assignee".into(), json!({"accountId": assignee}));
    }
    let priority = opt_str(input, "priority").trim();
    if !priority.is_empty() {
        fields.insert("priority".into(), json!({"name": priority}));
    }
}

// ---------------------------------------------------------------------------------------------------
// Markdown → Atlassian Document Format (ADF).
//
// Ported by hand from fluxplane's `atlassian.MarkdownToADF`, whose underlying `codewandler/md2adf`
// builds an ADF tree off a goldmark parse and then prunes code-incompatible marks. flux has no
// goldmark, so this is a self-contained block+inline converter covering the constructs Jira renders:
// paragraphs, ATX headings (1-6), bullet/ordered lists, fenced code blocks, blockquotes, thematic
// rules, and inline bold/italic/strikethrough/code/links. As in the reference, the ADF code mark may
// only combine with link, so the inline parser never emits code alongside any other mark.
// ---------------------------------------------------------------------------------------------------

/// Convert a Markdown string into a Jira-ready ADF `doc` node.
pub(super) fn markdown_to_adf(markdown: &str) -> Value {
    let content = convert_blocks(markdown);
    json!({ "type": "doc", "version": 1, "content": content })
}

/// Split `markdown` into block nodes (paragraphs, headings, lists, code blocks, blockquotes, rules).
pub(super) fn convert_blocks(markdown: &str) -> Vec<Value> {
    let normalized = markdown.replace('\r', "");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut blocks: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Blank lines separate blocks.
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // Fenced code block (``` or ~~~), optional language on the opening fence.
        if let Some(fence) = code_fence(trimmed) {
            let lang = trimmed[fence.len()..].trim();
            let mut code: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume the closing fence
            }
            let mut node = json!({
                "type": "codeBlock",
                "content": [{"type": "text", "text": code.join("\n")}],
            });
            if !lang.is_empty() {
                node["attrs"] = json!({ "language": lang });
            }
            blocks.push(node);
            continue;
        }

        // Thematic break: ---, ***, ___ (3+).
        if is_thematic_break(trimmed) {
            blocks.push(json!({ "type": "rule" }));
            i += 1;
            continue;
        }

        // ATX heading: 1-6 leading `#` then a space.
        if let Some((level, text)) = atx_heading(trimmed) {
            blocks.push(json!({
                "type": "heading",
                "attrs": { "level": level },
                "content": convert_inline(text),
            }));
            i += 1;
            continue;
        }

        // Blockquote: one or more `>`-prefixed lines; inner text is re-parsed as blocks.
        if trimmed.starts_with('>') {
            let mut inner: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                let l = lines[i].trim_start();
                let stripped = l.strip_prefix('>').unwrap_or(l);
                inner.push(stripped.strip_prefix(' ').unwrap_or(stripped).to_string());
                i += 1;
            }
            blocks.push(json!({
                "type": "blockquote",
                "content": convert_blocks(&inner.join("\n")),
            }));
            continue;
        }

        // List (bullet or ordered): a run of contiguous list-marker lines.
        if list_marker(trimmed).is_some() {
            let ordered = matches!(list_marker(trimmed), Some(ListKind::Ordered));
            let mut items: Vec<Value> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim();
                match list_marker(t) {
                    Some(kind) if (kind == ListKind::Ordered) == ordered => {
                        let text = strip_list_marker(t);
                        items.push(json!({
                            "type": "listItem",
                            "content": [{"type": "paragraph", "content": convert_inline(text)}],
                        }));
                        i += 1;
                    }
                    _ => break,
                }
            }
            blocks.push(json!({
                "type": if ordered { "orderedList" } else { "bulletList" },
                "content": items,
            }));
            continue;
        }

        // Otherwise: a paragraph — consecutive non-blank, non-block lines joined as soft breaks.
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len() {
            let t = lines[i].trim();
            if t.is_empty()
                || code_fence(t).is_some()
                || is_thematic_break(t)
                || atx_heading(t).is_some()
                || t.starts_with('>')
                || list_marker(t).is_some()
            {
                break;
            }
            para.push(lines[i].trim());
            i += 1;
        }
        // Soft line breaks become spaces in ADF (matching the reference converter).
        blocks.push(json!({
            "type": "paragraph",
            "content": convert_inline(&para.join(" ")),
        }));
    }
    if blocks.is_empty() {
        blocks.push(json!({ "type": "paragraph", "content": [] }));
    }
    blocks
}

/// The fence marker (```` ``` ```` or `~~~`) if `line` opens a fenced code block.
pub(super) fn code_fence(line: &str) -> Option<&'static str> {
    if line.starts_with("```") {
        Some("```")
    } else if line.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Whether `line` is a thematic break: 3+ of `-`, `*`, or `_` (ignoring spaces).
pub(super) fn is_thematic_break(line: &str) -> bool {
    for ch in ['-', '*', '_'] {
        let count = line.chars().filter(|&c| c == ch).count();
        if count >= 3 && line.chars().all(|c| c == ch || c == ' ') {
            return true;
        }
    }
    false
}

/// `(level, text)` if `line` is an ATX heading (`#`..`######` + space).
pub(super) fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if let Some(text) = rest.strip_prefix(' ') {
            return Some((hashes as u8, text.trim_end_matches('#').trim_end()));
        }
    }
    None
}

#[derive(PartialEq, Clone, Copy)]
pub(super) enum ListKind {
    Bullet,
    Ordered,
}

/// The list kind if `trimmed` starts with a list marker (`- `/`* `/`+ ` or `N. `/`N) `).
pub(super) fn list_marker(trimmed: &str) -> Option<ListKind> {
    for m in ["- ", "* ", "+ "] {
        if trimmed.starts_with(m) {
            return Some(ListKind::Bullet);
        }
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some(ListKind::Ordered);
        }
    }
    None
}

/// Strip the leading list marker, returning the item text.
pub(super) fn strip_list_marker(trimmed: &str) -> &str {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(m) {
            return rest.trim_start();
        }
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &trimmed[digits..];
    rest.strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))
        .unwrap_or(trimmed)
        .trim_start()
}

/// Convert inline Markdown into ADF text nodes. Recognizes `[text](href)` links, `` `code` ``,
/// `**bold**`/`__bold__`, `*em*`/`_em_`, and `~~strike~~`. The ADF code mark only ever combines with
/// link (never bold/em/strike), matching the reference's mark-pruning.
pub(super) fn convert_inline(text: &str) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut nodes: Vec<Value> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    // Flush the plain-text buffer (with the active marks) into a text node.
    macro_rules! flush {
        ($marks:expr) => {{
            if !buf.is_empty() {
                push_text(&mut nodes, &buf, $marks);
                buf.clear();
            }
        }};
    }

    let empty: &[&str] = &[];
    while i < chars.len() {
        let c = chars[i];

        // Inline code: `...` — highest precedence, no nested marks (only code, per ADF).
        if c == '`' {
            if let Some(end) = find_char(&chars, i + 1, '`') {
                flush!(empty);
                let code: String = chars[i + 1..end].iter().collect();
                push_text(&mut nodes, &code, &["code"]);
                i = end + 1;
                continue;
            }
        }

        // Link: [text](href).
        if c == '[' {
            if let Some((label, href, next)) = parse_link(&chars, i) {
                flush!(empty);
                let mut inner = convert_inline(&label);
                add_link_mark(&mut inner, &href);
                nodes.append(&mut inner);
                i = next;
                continue;
            }
        }

        // Strong: ** or __.
        if let Some(delim) = strong_delim(&chars, i) {
            if let Some(end) = find_delim(&chars, i + 2, delim) {
                flush!(empty);
                let inner: String = chars[i + 2..end].iter().collect();
                let mut sub = convert_inline_marked(&inner, &["strong"]);
                nodes.append(&mut sub);
                i = end + 2;
                continue;
            }
        }

        // Strikethrough: ~~.
        if c == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            if let Some(end) = find_delim(&chars, i + 2, '~') {
                flush!(empty);
                let inner: String = chars[i + 2..end].iter().collect();
                let mut sub = convert_inline_marked(&inner, &["strike"]);
                nodes.append(&mut sub);
                i = end + 2;
                continue;
            }
        }

        // Emphasis: single * or _.
        if (c == '*' || c == '_') && strong_delim(&chars, i).is_none() {
            if let Some(end) = find_char(&chars, i + 1, c) {
                if end > i + 1 {
                    flush!(empty);
                    let inner: String = chars[i + 1..end].iter().collect();
                    let mut sub = convert_inline_marked(&inner, &["em"]);
                    nodes.append(&mut sub);
                    i = end + 1;
                    continue;
                }
            }
        }

        buf.push(c);
        i += 1;
    }
    flush!(empty);
    nodes
}

/// Convert inline Markdown, adding `extra` marks to every produced text node (used for the inside of
/// a bold/em/strike span). The code mark is never extended with `extra` — code stands alone.
pub(super) fn convert_inline_marked(text: &str, extra: &[&str]) -> Vec<Value> {
    let mut nodes = convert_inline(text);
    for node in &mut nodes {
        // Never add formatting marks to a code-marked node (ADF forbids code + bold/em/strike).
        if has_mark(node, "code") {
            continue;
        }
        for m in extra {
            add_mark(node, m);
        }
    }
    nodes
}

/// Push a text node carrying `marks` onto `nodes`.
pub(super) fn push_text(nodes: &mut Vec<Value>, text: &str, marks: &[&str]) {
    let mut node = json!({ "type": "text", "text": text });
    for m in marks {
        add_mark(&mut node, m);
    }
    nodes.push(node);
}

/// Add a simple (attr-less) mark to a text node if not already present.
pub(super) fn add_mark(node: &mut Value, mark: &str) {
    if has_mark(node, mark) {
        return;
    }
    let marks = node
        .as_object_mut()
        .unwrap()
        .entry("marks")
        .or_insert_with(|| json!([]));
    if let Some(arr) = marks.as_array_mut() {
        arr.push(json!({ "type": mark }));
    }
}

/// Add a `link` mark with `href` to every text node in `nodes` (links may combine with code).
pub(super) fn add_link_mark(nodes: &mut [Value], href: &str) {
    for node in nodes.iter_mut() {
        let marks = node
            .as_object_mut()
            .unwrap()
            .entry("marks")
            .or_insert_with(|| json!([]));
        if let Some(arr) = marks.as_array_mut() {
            arr.push(json!({ "type": "link", "attrs": { "href": href } }));
        }
    }
}

/// Whether a text node already carries a mark of `kind`.
pub(super) fn has_mark(node: &Value, kind: &str) -> bool {
    node.get("marks")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .any(|m| m.get("type").and_then(|t| t.as_str()) == Some(kind))
        })
        .unwrap_or(false)
}

/// The next index of `target` at or after `from`, if any.
pub(super) fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// The next index where a doubled `delim` (`**`, `__`, `~~`) begins, at or after `from`. A single
/// trailing delimiter cannot close the span, so the run only matches a doubled occurrence.
pub(super) fn find_delim(chars: &[char], from: usize, delim: char) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == delim && chars[j + 1] == delim {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// The strong delimiter char at `i` (`*` for `**`, `_` for `__`), if a doubled run starts there.
pub(super) fn strong_delim(chars: &[char], i: usize) -> Option<char> {
    if i + 1 < chars.len() && (chars[i] == '*' || chars[i] == '_') && chars[i + 1] == chars[i] {
        Some(chars[i])
    } else {
        None
    }
}

/// Parse a `[label](href)` link starting at `chars[i] == '['`; returns `(label, href, next_index)`.
pub(super) fn parse_link(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let close = find_char(chars, i + 1, ']')?;
    if close + 1 >= chars.len() || chars[close + 1] != '(' {
        return None;
    }
    let href_end = find_char(chars, close + 2, ')')?;
    let label: String = chars[i + 1..close].iter().collect();
    let href: String = chars[close + 2..href_end].iter().collect();
    Some((label, href, href_end + 1))
}

// ---------------------------------------------------------------------------------------------------
// ADF → Markdown rendering (for body_format parity)
// ---------------------------------------------------------------------------------------------------

/// Render an issue's description according to `body_format`.
pub(super) fn render_issue_body_format(issue: &mut Value, format: BodyFormat) {
    if format == BodyFormat::Adf {
        return;
    }
    if let Some(fields) = issue.get_mut("fields") {
        let description = fields.get("description").cloned().unwrap_or(Value::Null);
        let rendered = render_body_format(&description, format);
        if let Some(obj) = fields.as_object_mut() {
            if format == BodyFormat::Both {
                obj.insert("description_adf".into(), description);
            }
            obj.insert("description".into(), rendered);
        }
    }
}

/// Render a comment's body according to `body_format`.
pub(super) fn render_comment_body_format(comment: &mut Value, format: BodyFormat) {
    if format == BodyFormat::Adf {
        return;
    }
    let body = comment.get("body").cloned().unwrap_or(Value::Null);
    let rendered = render_body_format(&body, format);
    if let Some(obj) = comment.as_object_mut() {
        if format == BodyFormat::Both {
            obj.insert("body_adf".into(), body);
        }
        obj.insert("body".into(), rendered);
    }
}

/// Render a single rich-text body value to Markdown (or return it as-is when already a string).
pub(super) fn render_body_format(body: &Value, format: BodyFormat) -> Value {
    match format {
        BodyFormat::Adf => body.clone(),
        BodyFormat::Markdown | BodyFormat::Both => {
            if let Some(s) = body.as_str() {
                return Value::String(s.to_string());
            }
            let doc = body.as_object();
            if doc.and_then(|o| o.get("type")).and_then(|v| v.as_str()) == Some("doc") {
                if let Some(content) = doc.unwrap().get("content").and_then(|v| v.as_array()) {
                    return Value::String(adf_doc_to_markdown(content).trim().to_string());
                }
            }
            body.clone()
        }
    }
}

pub(super) fn adf_doc_to_markdown(content: &[Value]) -> String {
    let parts: Vec<String> = content
        .iter()
        .map(|b| adf_block_to_markdown(b, "", None))
        .filter(|s| !s.is_empty())
        .collect();
    parts.join("\n\n")
}

pub(super) fn adf_block_to_markdown(
    block: &Value,
    indent: &str,
    list_prefix: Option<&str>,
) -> String {
    let typ = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let content = block.get("content").and_then(|v| v.as_array());
    match typ {
        "paragraph" => {
            let text =
                adf_inline_nodes_to_markdown(content.map_or(&[] as &[Value], |v| v.as_slice()));
            if text.is_empty() {
                String::new()
            } else {
                prefix_first_line(&text, list_prefix, indent)
            }
        }
        "heading" => {
            let level = block
                .get("attrs")
                .and_then(|a| a.get("level"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;
            let level = level.clamp(1, 6);
            let text =
                adf_inline_nodes_to_markdown(content.map_or(&[] as &[Value], |v| v.as_slice()));
            let line = format!("{} {}", "#".repeat(level), text);
            prefix_first_line(&line, list_prefix, indent)
        }
        "codeBlock" => {
            let lang = block
                .get("attrs")
                .and_then(|a| a.get("language"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let code: String = content
                .map_or(&[] as &[Value], |v| v.as_slice())
                .iter()
                .filter_map(|n| n.get("text").and_then(|v| v.as_str()))
                .collect();
            let fenced = format!("```{lang}\n{code}\n```");
            prefix_first_line(&fenced, list_prefix, indent)
        }
        "blockquote" => {
            let inner = adf_doc_to_markdown(content.map_or(&[] as &[Value], |v| v.as_slice()));
            let mut out = String::new();
            for line in inner.lines() {
                out.push_str(indent);
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.trim_end().to_string()
        }
        "bulletList" => {
            let items = content.map_or(&[] as &[Value], |v| v.as_slice());
            let item_indent = format!("{indent}  ");
            items
                .iter()
                .map(|item| adf_block_to_markdown(item, &item_indent, Some("-")))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "orderedList" => {
            let items = content.map_or(&[] as &[Value], |v| v.as_slice());
            let item_indent = format!("{indent}  ");
            items
                .iter()
                .enumerate()
                .map(|(idx, item)| {
                    adf_block_to_markdown(item, &item_indent, Some(&format!("{}.", idx + 1)))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "listItem" => {
            let parts: Vec<String> = content
                .map_or(&[] as &[Value], |v| v.as_slice())
                .iter()
                .enumerate()
                .map(|(idx, b)| {
                    if idx == 0 {
                        adf_block_to_markdown(b, indent, list_prefix)
                    } else {
                        adf_block_to_markdown(b, indent, None)
                    }
                })
                .filter(|s| !s.is_empty())
                .collect();
            parts.join("\n")
        }
        "rule" => prefix_first_line("---", list_prefix, indent),
        _ => String::new(),
    }
}

pub(super) fn prefix_first_line(text: &str, prefix: Option<&str>, indent: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut out = if let Some(p) = prefix {
        format!("{indent}{p} {first}")
    } else {
        format!("{indent}{first}")
    };
    for line in lines {
        out.push('\n');
        out.push_str(indent);
        out.push_str("  ");
        out.push_str(line);
    }
    out
}

pub(super) fn adf_inline_nodes_to_markdown(nodes: &[Value]) -> String {
    nodes.iter().map(adf_inline_to_markdown).collect()
}

pub(super) fn adf_inline_to_markdown(node: &Value) -> String {
    let typ = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match typ {
        "text" => {
            let text = node.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let marks = node
                .get("marks")
                .and_then(|v| v.as_array())
                .map_or(&[] as &[Value], |v| v.as_slice());
            apply_adf_marks(text, marks)
        }
        "hardBreak" => "\n".to_string(),
        _ => String::new(),
    }
}

pub(super) fn apply_adf_marks(text: &str, marks: &[Value]) -> String {
    if let Some(link) = marks
        .iter()
        .find(|m| m.get("type").and_then(|v| v.as_str()) == Some("link"))
    {
        let href = link
            .get("attrs")
            .and_then(|a| a.get("href"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return format!("[{text}]({href})");
    }
    let mut has_code = false;
    let mut has_strong = false;
    let mut has_em = false;
    let mut has_strike = false;
    for m in marks {
        match m.get("type").and_then(|v| v.as_str()) {
            Some("code") => has_code = true,
            Some("strong") => has_strong = true,
            Some("em") => has_em = true,
            Some("strike") => has_strike = true,
            _ => {}
        }
    }
    if has_code {
        return format!("`{text}`");
    }
    let mut out = text.to_string();
    if has_strike {
        out = format!("~~{out}~~");
    }
    if has_strong {
        out = format!("**{out}**");
    }
    if has_em {
        out = format!("*{out}*");
    }
    out
}

// ---------------------------------------------------------------------------------------------------
// Datasource contribution
// ---------------------------------------------------------------------------------------------------

/// Contribute one `jira.issue` record per issue in `result.issues[]`. Returns the record count.
pub(super) fn contribute_issues(host: &mut Host, result: &Value) -> usize {
    let Some(arr) = result.get("issues").and_then(|v| v.as_array()) else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let key = it.get("key").and_then(|v| v.as_str())?;
            let fields = it.get("fields");
            let summary = fields
                .and_then(|f| f.get("summary"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let status = fields
                .and_then(|f| f.get("status"))
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str());
            let body = match status {
                Some(s) => format!("{summary} [{s}]"),
                None => summary.to_string(),
            };
            Some(Record::new(
                Source::new("jira"),
                "jira.issue",
                key,
                summary,
                body,
            ))
        })
        .collect();
    let n = records.len();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
    n
}

/// Contribute one `jira.user` record per user in the `users[]` array. Returns the record count.
pub(super) fn contribute_users(host: &mut Host, users: &Value) -> usize {
    let Some(arr) = users.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|u| {
            let id = u.get("accountId").and_then(|v| v.as_str())?;
            let name = u.get("displayName").and_then(|v| v.as_str()).unwrap_or(id);
            let email = u.get("emailAddress").and_then(|v| v.as_str()).unwrap_or("");
            Some(Record::new(
                Source::new("jira"),
                "jira.user",
                id,
                name,
                email,
            ))
        })
        .collect();
    let n = records.len();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
    n
}

/// Percent-encode a query/path component: unreserved chars (`alnum -_.~`) pass through, all else `%XX`.
pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
