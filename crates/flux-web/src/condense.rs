//! Tier 2 condenser: HTML → readable, condensed **markdown**.
//!
//! "Read this page" should return a *document*, not markup. This module parses HTML with the
//! html5ever family (`scraper`), drops boilerplate (script/style/nav/header/footer/aside/forms/…),
//! prefers the main content region, builds a [`flux_markdown`] AST, and renders it with the markdown
//! writer — so the budget buys content, not tags. It is pure: no IO, no egress. The `html_to_markdown`
//! op and tier-2 `web_fetch` both call [`html_to_markdown`].

use flux_markdown::ast::{Alignment, Block, Document, Inline, List, ListItem, Table};
use flux_markdown::writer::to_markdown;
use scraper::{ElementRef, Html, Node, Selector};

/// Convert an HTML document to condensed, readable markdown.
pub fn html_to_markdown(html: &str) -> String {
    let doc = Html::parse_document(html);
    let root = main_content(&doc);
    let mut blocks = Vec::new();
    collect_blocks(root, &mut blocks);
    let document = Document {
        blocks: prune(blocks),
    };
    collapse_blank_lines(&to_markdown(&document))
}

/// Collect the `<a href>` link targets in `html`, each resolved to an absolute URL against `base`.
///
/// The condenser renders links inline as [`Inline::Link`] for reading; the `web.crawl` frontier
/// needs them as *data*. Fragment-only (`#...`) and non-`http(s)` targets (mailto:, javascript:, …)
/// are dropped, and the result is de-duplicated preserving first-seen order.
pub(crate) fn extract_links(html: &str, base: &url::Url) -> Vec<String> {
    let doc = Html::parse_document(html);
    let Ok(sel) = Selector::parse("a[href]") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for el in doc.select(&sel) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') {
            continue;
        }
        if let Ok(resolved) = base.join(href) {
            if matches!(resolved.scheme(), "http" | "https") {
                let s = resolved.to_string();
                if seen.insert(s.clone()) {
                    out.push(s);
                }
            }
        }
    }
    out
}

/// The page's `<title>`, whitespace-collapsed, if present and non-empty.
pub fn page_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("title").ok()?;
    let raw = doc.select(&sel).next().map(|t| raw_text(t))?;
    let title = collapse_ws(&raw).trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// Pick the element that holds the page's main content: the largest of `<main>` / `[role=main]` /
/// `<article>` by text length, falling back to `<body>` (and then the document root). This is the
/// readability step made pragmatic — a div-soup page with none of those tags falls through to
/// `<body>`, which the boilerplate filter still cleans up.
fn main_content(doc: &Html) -> ElementRef<'_> {
    let mut best: Option<(usize, ElementRef)> = None;
    for sel in ["main", "[role=\"main\"]", "[role=main]", "article"] {
        if let Ok(selector) = Selector::parse(sel) {
            for el in doc.select(&selector) {
                let len = text_len(el);
                if best.as_ref().map(|(l, _)| len > *l).unwrap_or(true) {
                    best = Some((len, el));
                }
            }
        }
    }
    // Only trust a semantic region if it actually carries substance; otherwise use <body>.
    if let Some((len, el)) = best {
        if len >= 200 {
            return el;
        }
    }
    if let Ok(body) = Selector::parse("body") {
        if let Some(el) = doc.select(&body).next() {
            return el;
        }
    }
    doc.root_element()
}

/// Total length of the visible text under `el` (boilerplate subtrees excluded).
fn text_len(el: ElementRef<'_>) -> usize {
    let mut n = 0;
    for child in el.children() {
        match child.value() {
            Node::Text(t) => n += t.trim().len(),
            Node::Element(e) if !is_boilerplate(e.name()) => {
                if let Some(ce) = ElementRef::wrap(child) {
                    n += text_len(ce);
                }
            }
            _ => {}
        }
    }
    n
}

/// Tags whose subtree is never content: scripts, styling, chrome, forms, embeds.
fn is_boilerplate(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "noscript"
            | "template"
            | "nav"
            | "footer"
            | "aside"
            | "form"
            | "button"
            | "input"
            | "select"
            | "textarea"
            | "iframe"
            | "svg"
            | "canvas"
            | "object"
            | "embed"
            | "head"
            | "link"
            | "meta"
    )
}

/// Block-level elements that map to a markdown block.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "h1" | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "p"
            | "ul"
            | "ol"
            | "blockquote"
            | "pre"
            | "hr"
            | "table"
    )
}

/// Container elements that carry no markdown structure of their own — recurse through them.
fn is_transparent(name: &str) -> bool {
    matches!(
        name,
        "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "figure"
            | "figcaption"
            | "details"
            | "summary"
            | "body"
            | "html"
            | "dl"
            | "dd"
            | "dt"
    )
}

/// Walk `el`'s children into markdown blocks, buffering loose inline content into paragraphs.
fn collect_blocks(el: ElementRef<'_>, out: &mut Vec<Block>) {
    let mut inline_buf: Vec<Inline> = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(t) => append_text(&mut inline_buf, t),
            Node::Element(e) => {
                let name = e.name();
                let Some(ce) = ElementRef::wrap(child) else {
                    continue;
                };
                if is_boilerplate(name) {
                    // dropped
                } else if is_block(name) {
                    flush_paragraph(&mut inline_buf, out);
                    handle_block(name, e, ce, out);
                } else if is_transparent(name) {
                    flush_paragraph(&mut inline_buf, out);
                    collect_blocks(ce, out);
                } else {
                    // Inline element in block context → accumulate into the pending paragraph.
                    inline_buf.extend(inline_of_element(e, ce));
                }
            }
            _ => {}
        }
    }
    flush_paragraph(&mut inline_buf, out);
}

fn handle_block(name: &str, e: &scraper::node::Element, el: ElementRef<'_>, out: &mut Vec<Block>) {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name[1..].parse::<u8>().unwrap_or(1);
            let mut content = inline_children(el);
            trim_inlines(&mut content);
            if !content.is_empty() {
                out.push(Block::Heading { level, content });
            }
        }
        "p" => {
            let mut content = inline_children(el);
            trim_inlines(&mut content);
            if !content.is_empty() {
                out.push(Block::Paragraph { content });
            }
        }
        "ul" | "ol" => out.push(build_list(name == "ol", e, el)),
        "blockquote" => {
            let mut inner = Vec::new();
            collect_blocks(el, &mut inner);
            let inner = prune(inner);
            if !inner.is_empty() {
                out.push(Block::BlockQuote { blocks: inner });
            }
        }
        "pre" => {
            let literal = raw_text(el);
            let literal = literal.trim_matches('\n').to_string();
            if !literal.trim().is_empty() {
                out.push(Block::CodeBlock {
                    info: code_lang(el),
                    literal,
                });
            }
        }
        "hr" => out.push(Block::ThematicBreak),
        "table" => {
            if let Some(t) = build_table(el) {
                out.push(Block::Table(t));
            }
        }
        _ => {}
    }
}

fn build_list(ordered: bool, e: &scraper::node::Element, el: ElementRef<'_>) -> Block {
    let start = e
        .attr("start")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1);
    let mut items = Vec::new();
    for child in el.children() {
        if let Node::Element(ce) = child.value() {
            if ce.name() == "li" {
                if let Some(li) = ElementRef::wrap(child) {
                    let mut blocks = Vec::new();
                    collect_blocks(li, &mut blocks);
                    let blocks = prune(blocks);
                    if !blocks.is_empty() {
                        items.push(ListItem { blocks });
                    }
                }
            }
        }
    }
    Block::List(List {
        ordered,
        start: if ordered { start } else { 1 },
        tight: true,
        marker: if ordered { '.' } else { '-' },
        items,
    })
}

fn build_table(el: ElementRef<'_>) -> Option<Table> {
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    // Collect every <tr> anywhere under the table (thead/tbody/tfoot or direct children).
    let tr = Selector::parse("tr").ok()?;
    for tr_el in el.select(&tr) {
        let mut cells = Vec::new();
        for cell in tr_el.children() {
            if let Node::Element(ce) = cell.value() {
                if matches!(ce.name(), "td" | "th") {
                    if let Some(cell_el) = ElementRef::wrap(cell) {
                        let mut c = inline_children(cell_el);
                        trim_inlines(&mut c);
                        cells.push(c);
                    }
                }
            }
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    if rows.is_empty() {
        return None;
    }
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    // Normalize ragged rows so the writer emits a well-formed pipe table.
    for r in &mut rows {
        while r.len() < ncols {
            r.push(Vec::new());
        }
    }
    Some(Table {
        alignments: vec![Alignment::None; ncols],
        rows,
    })
}

/// The inline content of `el`'s children (links, emphasis, code, images, text).
fn inline_children(el: ElementRef<'_>) -> Vec<Inline> {
    let mut out = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(t) => append_text(&mut out, t),
            Node::Element(e) => {
                if let Some(ce) = ElementRef::wrap(child) {
                    out.extend(inline_of_element(e, ce));
                }
            }
            _ => {}
        }
    }
    out
}

/// Convert a single inline element (and its subtree) into inline node(s).
fn inline_of_element(e: &scraper::node::Element, ce: ElementRef<'_>) -> Vec<Inline> {
    match e.name() {
        "a" => {
            let mut content = inline_children(ce);
            trim_inlines(&mut content);
            if content.is_empty() {
                return Vec::new();
            }
            vec![Inline::Link {
                dest: e.attr("href").unwrap_or_default().to_string(),
                title: String::new(),
                content,
            }]
        }
        "img" => {
            let alt = e.attr("alt").unwrap_or_default().to_string();
            vec![Inline::Image {
                dest: e.attr("src").unwrap_or_default().to_string(),
                title: String::new(),
                content: if alt.is_empty() {
                    Vec::new()
                } else {
                    vec![Inline::Text(alt)]
                },
            }]
        }
        "strong" | "b" => {
            let c = inline_children(ce);
            if c.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Strong(c)]
            }
        }
        "em" | "i" => {
            let c = inline_children(ce);
            if c.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Emphasis(c)]
            }
        }
        "del" | "s" | "strike" => {
            let c = inline_children(ce);
            if c.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Strikethrough(c)]
            }
        }
        "code" | "kbd" | "samp" => {
            let code = collapse_ws(&raw_text(ce));
            if code.trim().is_empty() {
                Vec::new()
            } else {
                vec![Inline::Code(code.trim().to_string())]
            }
        }
        "br" => vec![Inline::HardBreak],
        n if is_boilerplate(n) => Vec::new(),
        // Any other inline/unknown element: recurse transparently.
        _ => inline_children(ce),
    }
}

/// All descendant text of `el`, verbatim (used for code blocks/spans).
fn raw_text(el: ElementRef<'_>) -> String {
    let mut s = String::new();
    for child in el.children() {
        match child.value() {
            Node::Text(t) => s.push_str(t),
            Node::Element(e) if !is_boilerplate(e.name()) => {
                if let Some(ce) = ElementRef::wrap(child) {
                    s.push_str(&raw_text(ce));
                }
            }
            _ => {}
        }
    }
    s
}

/// `<pre><code class="language-rust">` → `rust`.
fn code_lang(pre: ElementRef<'_>) -> String {
    if let Ok(code) = Selector::parse("code") {
        if let Some(c) = pre.select(&code).next() {
            if let Some(class) = c.value().attr("class") {
                for tok in class.split_whitespace() {
                    if let Some(lang) = tok.strip_prefix("language-").or(tok.strip_prefix("lang-"))
                    {
                        return lang.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// Append a text node's content, collapsing HTML whitespace runs to single spaces. Consecutive
/// whitespace-only nodes don't stack up.
fn append_text(buf: &mut Vec<Inline>, text: &str) {
    let collapsed = collapse_ws(text);
    if collapsed.is_empty() {
        return;
    }
    // Merge with a trailing Text to keep the inline vector compact.
    if let Some(Inline::Text(prev)) = buf.last_mut() {
        prev.push_str(&collapsed);
    } else {
        buf.push(Inline::Text(collapsed));
    }
}

/// Collapse every run of ASCII/Unicode whitespace to a single space (HTML inline whitespace rules).
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// Trim leading/trailing whitespace from an inline run's edge text nodes, dropping any that empty out.
fn trim_inlines(content: &mut Vec<Inline>) {
    if let Some(Inline::Text(first)) = content.first_mut() {
        let trimmed = first.trim_start().to_string();
        *first = trimmed;
    }
    if let Some(Inline::Text(last)) = content.last_mut() {
        let trimmed = last.trim_end().to_string();
        *last = trimmed;
    }
    content.retain(|i| !matches!(i, Inline::Text(t) if t.is_empty()));
}

/// Push the buffered inline content as a paragraph (if it has any visible text), clearing the buffer.
fn flush_paragraph(buf: &mut Vec<Inline>, out: &mut Vec<Block>) {
    if buf.is_empty() {
        return;
    }
    let mut content = std::mem::take(buf);
    trim_inlines(&mut content);
    if !content.is_empty() {
        out.push(Block::Paragraph { content });
    }
}

/// Drop empty leading/trailing thematic breaks and collapse doubled rules.
fn prune(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for b in blocks {
        if matches!(b, Block::ThematicBreak) {
            // no leading rule, no doubled rules
            if out.is_empty() || matches!(out.last(), Some(Block::ThematicBreak)) {
                continue;
            }
        }
        out.push(b);
    }
    while matches!(out.last(), Some(Block::ThematicBreak)) {
        out.pop();
    }
    out
}

/// Collapse 3+ consecutive newlines to a blank-line separator.
fn collapse_blank_lines(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut newlines = 0;
    for ch in md.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                out.push(ch);
            }
        } else {
            newlines = 0;
            out.push(ch);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_article_condenses_to_markdown() {
        let html = r#"
            <html><head><title>T</title><style>.x{}</style></head>
            <body>
              <nav><a href="/">Home</a> <a href="/about">About</a></nav>
              <header><h1>Site chrome should be dropped when article exists</h1></header>
              <article>
                <h1>The Real Title</h1>
                <p>First paragraph with a <a href="https://example.com/x">link</a> and
                   <strong>bold</strong> plus <em>italic</em> text that is long enough to matter
                   for the readability threshold so the article wins over the body element.</p>
                <ul><li>one</li><li>two</li></ul>
                <pre><code class="language-rust">fn main() {}</code></pre>
              </article>
              <footer>© drop me</footer>
              <script>var tracking = 1;</script>
            </body></html>
        "#;
        let md = html_to_markdown(html);
        assert!(md.contains("# The Real Title"), "heading kept: {md}");
        assert!(
            md.contains("[link](https://example.com/x)"),
            "link kept: {md}"
        );
        assert!(md.contains("**bold**"), "strong kept: {md}");
        assert!(md.contains("- one"), "list kept: {md}");
        assert!(md.contains("```rust"), "code fence + lang kept: {md}");
        assert!(md.contains("fn main() {}"), "code body kept: {md}");
        // Boilerplate is gone.
        assert!(!md.contains("drop me"), "footer dropped: {md}");
        assert!(!md.contains("tracking"), "script dropped: {md}");
        assert!(!md.to_lowercase().contains("home"), "nav dropped: {md}");
        assert!(!md.contains("<"), "no raw markup leaks: {md}");
    }

    #[test]
    fn div_soup_page_falls_back_to_body_and_still_condenses() {
        // No <article>/<main>: a div-soup page. Content must still come through from <body>, with
        // scripts/styles stripped.
        let html = r#"
            <html><body>
              <div class="wrapper">
                <div class="post">
                  <div class="title"><span>Div Soup Heading</span></div>
                  <div class="content">
                    <div>Some meaningful paragraph text sitting in a bare div with no semantic tags,
                    followed by a <a href="/next">next</a> link.</div>
                    <div><a href="/a">a</a><a href="/b">b</a></div>
                  </div>
                </div>
              </div>
              <script>ignore()</script>
            </body></html>
        "#;
        let md = html_to_markdown(html);
        assert!(md.contains("Div Soup Heading"), "div text kept: {md}");
        assert!(md.contains("meaningful paragraph text"), "body text: {md}");
        assert!(md.contains("[next](/next)"), "link kept: {md}");
        assert!(!md.contains("ignore"), "script dropped: {md}");
        assert!(!md.contains("<"), "no markup leaks: {md}");
    }

    #[test]
    fn table_becomes_a_pipe_table() {
        let html = r#"<body><main><p>long enough content to pass the readability threshold so the
            main element is selected as the content region for this particular fixture page.</p>
            <table><thead><tr><th>Name</th><th>Qty</th></tr></thead>
            <tbody><tr><td>Apples</td><td>3</td></tr></tbody></table></main></body>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("| Name | Qty |"), "table header: {md}");
        assert!(md.contains("| Apples | 3 |"), "table row: {md}");
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(html_to_markdown(""), "");
        assert_eq!(html_to_markdown("<html><body></body></html>"), "");
    }

    #[test]
    fn extract_links_resolves_dedups_and_filters() {
        let base = url::Url::parse("https://example.com/dir/page.html").unwrap();
        let html = r##"
            <a href="/abs">absolute-path</a>
            <a href="rel">relative</a>
            <a href="https://other.com/x">cross-host</a>
            <a href="/abs">dup of the first</a>
            <a href="#frag">fragment only</a>
            <a href="mailto:a@b.com">mail</a>
            <a href="javascript:void(0)">js</a>
        "##;
        let links = extract_links(html, &base);
        assert_eq!(
            links,
            vec![
                "https://example.com/abs".to_string(),
                "https://example.com/dir/rel".to_string(),
                "https://other.com/x".to_string(),
            ],
            "resolved against base, de-duplicated, fragments + non-http(s) dropped: {links:?}"
        );
    }
}
