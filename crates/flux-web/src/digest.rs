//! Tier 3 — the **page digest** (D-122): the heart of non-visual browsing.
//!
//! A digest is what a screen reader sees — roles, names, states — plus condensed readable text, and
//! it *is* the action space: every interactive element gets a stable `e<N>` ref the act ops target.
//! The builder is **pure** over a captured `Accessibility.getFullAXTree` payload joined with DOM node
//! identity (`backendDOMNodeId`), so goldens don't need Chrome. Output ordering is document order
//! (AX node order) — replay/`flux diff` friendly.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// Which slices of the digest to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Header + content + actions.
    Full,
    /// Header + actions only.
    Actions,
    /// Header + content only.
    Content,
}

impl View {
    pub fn parse(s: &str) -> View {
        match s {
            "actions" => View::Actions,
            "content" => View::Content,
            _ => View::Full,
        }
    }
}

/// Byte budgets for the two sections (the A-24 "`len <= cap`" discipline; overridable per call).
#[derive(Debug, Clone, Copy)]
pub struct DigestCaps {
    pub content_bytes: usize,
    pub actions_bytes: usize,
}

impl Default for DigestCaps {
    fn default() -> Self {
        Self {
            content_bytes: 24 * 1024,
            actions_bytes: 16 * 1024,
        }
    }
}

/// Stable `e<N>` ↔ `backendDOMNodeId` map, held session-side so refs survive re-observation while the
/// node lives. Dead nodes are marked dead, never silently renumbered.
#[derive(Default)]
pub struct RefMap {
    by_backend: HashMap<i64, u32>,
    next: u32,
    alive: HashSet<i64>,
}

impl RefMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the existing ref for a backend node, or assign the next one.
    fn ref_for(&mut self, backend: i64) -> u32 {
        if let Some(&n) = self.by_backend.get(&backend) {
            return n;
        }
        self.next += 1;
        self.by_backend.insert(backend, self.next);
        self.next
    }

    /// The `backendDOMNodeId` a ref points at, if known.
    pub fn backend_of(&self, n: u32) -> Option<i64> {
        self.by_backend
            .iter()
            .find(|(_, &v)| v == n)
            .map(|(&k, _)| k)
    }

    /// Whether ref `n` resolves to a node present in the latest snapshot.
    pub fn is_alive(&self, n: u32) -> bool {
        self.backend_of(n)
            .map(|b| self.alive.contains(&b))
            .unwrap_or(false)
    }
}

/// Interactive AX roles that become action-space entries.
fn is_interactive(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "textbox"
            | "combobox"
            | "searchbox"
            | "checkbox"
            | "radio"
            | "switch"
            | "tab"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "slider"
            | "spinbutton"
    )
}

/// Extract the `value` string from an AX property bag entry like `{"value": {"value": X}}`.
fn ax_value(node: &Value, key: &str) -> Option<Value> {
    node.get(key).and_then(|v| v.get("value")).cloned()
}

fn ax_str(node: &Value, key: &str) -> String {
    ax_value(node, key)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Read a named AX property (`properties: [{name, value:{value}}]`).
fn ax_property<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node.get("properties")?.as_array()?.iter().find_map(|p| {
        if p.get("name").and_then(Value::as_str) == Some(name) {
            p.get("value").and_then(|v| v.get("value"))
        } else {
            None
        }
    })
}

/// The DOM-heuristic fallback: a `generic`/`GenericContainer` node that is focusable and named is a
/// div-soup clickable — surface it as a button so unlabeled interactives still appear.
fn is_fallback_clickable(role: &str, node: &Value, name: &str) -> bool {
    let generic = matches!(
        role,
        "generic" | "GenericContainer" | "none" | "presentation"
    );
    let focusable = ax_property(node, "focusable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    generic && focusable && !name.is_empty()
}

/// One rendered action line + the state suffix, for a node.
fn action_line(reef: u32, role: &str, name: &str, node: &Value) -> String {
    let mut states: Vec<String> = Vec::new();
    if ax_property(node, "disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        states.push("disabled".into());
    }
    if let Some(checked) = ax_property(node, "checked") {
        match checked.as_str() {
            Some("true") => states.push("checked".into()),
            Some("false") => states.push("unchecked".into()),
            Some("mixed") => states.push("mixed".into()),
            _ => {}
        }
    }
    if let Some(expanded) = ax_property(node, "expanded").and_then(Value::as_bool) {
        states.push(if expanded { "expanded" } else { "collapsed" }.into());
    }
    // Current value for inputs.
    let value = ax_str(node, "value");
    if !value.is_empty() {
        states.push(format!("value: {value:?}"));
    } else if matches!(role, "textbox" | "searchbox" | "combobox") {
        states.push("value: \"\"".into());
    }
    let state = if states.is_empty() {
        String::new()
    } else {
        format!(" ({})", states.join(", "))
    };
    let role_disp = if is_interactive(role) { role } else { "button" };
    format!("{}  {:<8} {:?}{}", reef_label(reef), role_disp, name, state)
}

fn reef_label(n: u32) -> String {
    format!("e{n}")
}

/// Build a digest from an AX tree, updating the ref map. `title`/`url` head it.
pub fn build_digest(
    url: &str,
    title: &str,
    ax_tree: &Value,
    refs: &mut RefMap,
    view: View,
    caps: DigestCaps,
) -> String {
    // Begin a fresh snapshot: nothing is alive until we see it.
    refs.alive.clear();

    let nodes = ax_tree
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut content = String::new();
    let mut actions = String::new();

    for node in &nodes {
        let role = ax_str(node, "role");
        let name = ax_str(node, "name");
        let backend = node.get("backendDOMNodeId").and_then(Value::as_i64);

        // Ignore nodes AX marks as ignored.
        if node
            .get("ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }

        let interactive = is_interactive(&role) || is_fallback_clickable(&role, node, &name);
        if interactive {
            if let Some(b) = backend {
                let reef = refs.ref_for(b);
                refs.alive.insert(b);
                let line = action_line(reef, &role, &name, node);
                if actions.len() + line.len() < caps.actions_bytes {
                    actions.push_str(&line);
                    actions.push('\n');
                } else if !actions.ends_with("…\n") {
                    actions.push_str("…\n");
                }
            }
            continue;
        }

        // Content: readable text carriers.
        match role.as_str() {
            "heading" if !name.is_empty() => {
                let level = ax_property(node, "level")
                    .and_then(Value::as_i64)
                    .unwrap_or(2)
                    .clamp(1, 6) as usize;
                push_capped(
                    &mut content,
                    &format!("{} {name}\n", "#".repeat(level)),
                    caps.content_bytes,
                );
            }
            "StaticText" | "staticText" | "paragraph" | "text" if !name.is_empty() => {
                push_capped(&mut content, &format!("{name}\n"), caps.content_bytes);
            }
            _ => {}
        }
    }

    let mut out = String::new();
    out.push_str(&format!("{url} · {title:?}\n"));
    if matches!(view, View::Full | View::Content) && !content.trim().is_empty() {
        out.push_str("## content\n");
        out.push_str(content.trim_end());
        out.push('\n');
    }
    if matches!(view, View::Full | View::Actions) && !actions.is_empty() {
        out.push_str("## actions\n");
        out.push_str(actions.trim_end());
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn push_capped(buf: &mut String, s: &str, cap: usize) {
    if buf.len() + s.len() <= cap {
        buf.push_str(s);
    } else if !buf.ends_with("…\n") {
        buf.push_str("…\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ax(nodes: Value) -> Value {
        json!({ "nodes": nodes })
    }

    fn prop(name: &str, value: Value) -> Value {
        json!({ "name": name, "value": { "value": value } })
    }

    #[test]
    fn digest_renders_header_content_and_action_space() {
        let tree = ax(json!([
            { "role": {"value": "heading"}, "name": {"value": "Checkout"}, "properties": [prop("level", json!(1))], "backendDOMNodeId": 1 },
            { "role": {"value": "StaticText"}, "name": {"value": "Review your order."}, "backendDOMNodeId": 2 },
            { "role": {"value": "link"}, "name": {"value": "Edit cart"}, "backendDOMNodeId": 3 },
            { "role": {"value": "textbox"}, "name": {"value": "Email"}, "backendDOMNodeId": 7 },
            { "role": {"value": "checkbox"}, "name": {"value": "Subscribe"}, "properties": [prop("checked", json!("false"))], "backendDOMNodeId": 9 },
            { "role": {"value": "button"}, "name": {"value": "Place order"}, "properties": [prop("disabled", json!(true))], "backendDOMNodeId": 12 },
        ]));
        let mut refs = RefMap::new();
        let d = build_digest(
            "https://shop.example/checkout",
            "Checkout — Shop",
            &tree,
            &mut refs,
            View::Full,
            DigestCaps::default(),
        );
        assert!(
            d.contains("https://shop.example/checkout · \"Checkout — Shop\""),
            "{d}"
        );
        assert!(d.contains("# Checkout"), "{d}");
        assert!(d.contains("Review your order."), "{d}");
        assert!(d.contains("## actions"), "{d}");
        assert!(d.contains(r#"e1  link     "Edit cart""#), "{d}");
        assert!(d.contains(r#"textbox  "Email" (value: "")"#), "{d}");
        assert!(d.contains(r#"checkbox "Subscribe" (unchecked)"#), "{d}");
        assert!(d.contains(r#"button   "Place order" (disabled)"#), "{d}");
    }

    #[test]
    fn refs_are_stable_across_a_partial_mutation() {
        let tree1 = ax(json!([
            { "role": {"value": "button"}, "name": {"value": "A"}, "backendDOMNodeId": 10 },
            { "role": {"value": "button"}, "name": {"value": "B"}, "backendDOMNodeId": 20 },
        ]));
        let mut refs = RefMap::new();
        build_digest(
            "u",
            "t",
            &tree1,
            &mut refs,
            View::Actions,
            DigestCaps::default(),
        );
        let a_ref = refs.by_backend[&10];
        let b_ref = refs.by_backend[&20];

        // Re-snapshot: node 20 gone, a new node 30 appears. 10 keeps its ref; 30 gets a fresh one.
        let tree2 = ax(json!([
            { "role": {"value": "button"}, "name": {"value": "A"}, "backendDOMNodeId": 10 },
            { "role": {"value": "button"}, "name": {"value": "C"}, "backendDOMNodeId": 30 },
        ]));
        build_digest(
            "u",
            "t",
            &tree2,
            &mut refs,
            View::Actions,
            DigestCaps::default(),
        );
        assert_eq!(refs.by_backend[&10], a_ref, "live ref unchanged");
        assert!(refs.is_alive(a_ref), "10 still alive");
        assert!(!refs.is_alive(b_ref), "20 marked dead, not renumbered");
        assert_ne!(refs.by_backend[&30], a_ref);
        assert_ne!(refs.by_backend[&30], b_ref);
    }

    #[test]
    fn div_soup_clickable_surfaces_via_fallback() {
        // No semantic role — a focusable, named generic node (div with a handler).
        let tree = ax(json!([
            { "role": {"value": "generic"}, "name": {"value": "Fake Button"},
              "properties": [prop("focusable", json!(true))], "backendDOMNodeId": 5 },
        ]));
        let mut refs = RefMap::new();
        let d = build_digest(
            "u",
            "t",
            &tree,
            &mut refs,
            View::Actions,
            DigestCaps::default(),
        );
        assert!(
            d.contains(r#"button   "Fake Button""#),
            "fallback surfaced: {d}"
        );
    }

    #[test]
    fn sections_are_byte_budgeted() {
        let mut nodes = Vec::new();
        for i in 0..500 {
            nodes.push(json!({ "role": {"value": "StaticText"},
                "name": {"value": "x".repeat(100)}, "backendDOMNodeId": i }));
        }
        let mut refs = RefMap::new();
        let caps = DigestCaps {
            content_bytes: 1024,
            actions_bytes: 1024,
        };
        let d = build_digest("u", "t", &ax(json!(nodes)), &mut refs, View::Content, caps);
        // Header + "## content" + <=cap content + omission marker — comfortably bounded.
        assert!(
            d.len() <= 1024 + 64,
            "content section is capped: len={}",
            d.len()
        );
        assert!(d.contains('…'), "omission marker present");
    }

    #[test]
    fn view_filters_sections() {
        let tree = ax(json!([
            { "role": {"value": "StaticText"}, "name": {"value": "hello"}, "backendDOMNodeId": 1 },
            { "role": {"value": "button"}, "name": {"value": "Go"}, "backendDOMNodeId": 2 },
        ]));
        let mut refs = RefMap::new();
        let actions_only = build_digest(
            "u",
            "t",
            &tree,
            &mut refs,
            View::Actions,
            DigestCaps::default(),
        );
        assert!(actions_only.contains("## actions"));
        assert!(!actions_only.contains("## content"));
        let content_only = build_digest(
            "u",
            "t",
            &tree,
            &mut RefMap::new(),
            View::Content,
            DigestCaps::default(),
        );
        assert!(content_only.contains("## content"));
        assert!(!content_only.contains("## actions"));
    }
}
