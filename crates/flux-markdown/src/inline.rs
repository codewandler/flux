//! The inline pass: text runs, escapes, code spans, links/images/autolinks, hard/soft breaks, and
//! delimiter-stack emphasis (`*`/`_` with CommonMark flanking + mod-3 rules, GFM `~~`).
//!
//! Entities are NOT decoded and raw HTML is NOT recognized (both stay literal text) — see the
//! [`crate::parser`] docs for the full unsupported list.

use crate::ast::{Inline, ParseOptions};

/// One node of the working list: a finished inline, or a not-yet-resolved delimiter/bracket.
enum Node {
    Done(Inline),
    Delim(Delim),
    Bracket { image: bool, active: bool },
}

struct Delim {
    ch: char,
    count: usize,
    /// The run length at scan time (the mod-3 rule uses original lengths).
    orig: usize,
    can_open: bool,
    can_close: bool,
}

/// Parse a block's inline content. `src` is the block's raw text with lines joined by `\n`
/// (leading whitespace per line already stripped, the final line's trailing whitespace trimmed).
pub(crate) fn parse_inlines(src: &str, opts: &ParseOptions) -> Vec<Inline> {
    let mut nodes = scan(src, opts);
    process_emphasis(&mut nodes, 0);
    finalize(nodes)
}

/// Pass 1: linear scan into finished inlines + unresolved delimiters/brackets.
fn scan(src: &str, opts: &ParseOptions) -> Vec<Node> {
    let b = src.as_bytes();
    let mut nodes: Vec<Node> = Vec::new();
    let mut text = String::new();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !text.is_empty() {
                nodes.push(Node::Done(Inline::Text(std::mem::take(&mut text))));
            }
        };
    }

    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' => {
                if i + 1 < b.len() && b[i + 1] == b'\n' {
                    trim_trailing_ws(&mut text);
                    flush!();
                    nodes.push(Node::Done(Inline::HardBreak));
                    i += 2;
                    i = skip_leading_ws(b, i);
                } else if i + 1 < b.len() && b[i + 1].is_ascii_punctuation() {
                    text.push(b[i + 1] as char);
                    i += 2;
                } else {
                    text.push('\\');
                    i += 1;
                }
            }
            b'\n' => {
                let hard = text.ends_with("  ");
                trim_trailing_ws(&mut text);
                flush!();
                nodes.push(Node::Done(if hard {
                    Inline::HardBreak
                } else {
                    Inline::SoftBreak
                }));
                i += 1;
                i = skip_leading_ws(b, i);
            }
            b'`' => {
                let n = run_len(b, i, b'`');
                if let Some((content, end)) = close_code_span(src, i + n, n) {
                    flush!();
                    nodes.push(Node::Done(Inline::Code(content)));
                    i = end;
                } else {
                    text.push_str(&src[i..i + n]);
                    i += n;
                }
            }
            b'<' => {
                if let Some((uri, end)) = autolink(src, i) {
                    flush!();
                    nodes.push(Node::Done(Inline::Link {
                        dest: uri.clone(),
                        title: String::new(),
                        content: vec![Inline::Text(uri)],
                    }));
                    i = end;
                } else {
                    text.push('<');
                    i += 1;
                }
            }
            b'!' if i + 1 < b.len() && b[i + 1] == b'[' => {
                flush!();
                nodes.push(Node::Bracket {
                    image: true,
                    active: true,
                });
                i += 2;
            }
            b'[' => {
                flush!();
                nodes.push(Node::Bracket {
                    image: false,
                    active: true,
                });
                i += 1;
            }
            b']' => {
                flush!();
                i += 1;
                i = close_bracket(&mut nodes, src, i);
            }
            b'*' | b'_' => {
                let n = run_len(b, i, c);
                flush!();
                let (can_open, can_close) = flanking(src, i, n, c as char);
                nodes.push(Node::Delim(Delim {
                    ch: c as char,
                    count: n,
                    orig: n,
                    can_open,
                    can_close,
                }));
                i += n;
            }
            b'~' if opts.strikethrough => {
                let n = run_len(b, i, b'~');
                if n == 2 {
                    flush!();
                    let (can_open, can_close) = flanking(src, i, n, '*');
                    nodes.push(Node::Delim(Delim {
                        ch: '~',
                        count: n,
                        orig: n,
                        can_open,
                        can_close,
                    }));
                } else {
                    text.push_str(&src[i..i + n]);
                }
                i += n;
            }
            _ => {
                // Advance one full UTF-8 char.
                let ch = src[i..].chars().next().unwrap();
                text.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    flush!();
    nodes
}

fn trim_trailing_ws(s: &mut String) {
    while s.ends_with([' ', '\t']) {
        s.pop();
    }
}

fn skip_leading_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

fn run_len(b: &[u8], i: usize, ch: u8) -> usize {
    let mut n = 0;
    while i + n < b.len() && b[i + n] == ch {
        n += 1;
    }
    n
}

/// Find a closing backtick run of exactly `n` after `start`; returns the normalized content and the
/// index just past the closer.
fn close_code_span(src: &str, start: usize, n: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    let mut i = start;
    while i < b.len() {
        if b[i] == b'`' {
            let run = run_len(b, i, b'`');
            if run == n {
                let mut content = src[start..i].replace('\n', " ");
                // Strip one space from each side when both are spaces and content isn't all spaces.
                if content.starts_with(' ')
                    && content.ends_with(' ')
                    && content.chars().any(|c| c != ' ')
                {
                    content = content[1..content.len() - 1].to_string();
                }
                return Some((content, i + run));
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// `<scheme:...>` autolink at `i` (which points at `<`). Returns (uri, index past `>`).
fn autolink(src: &str, i: usize) -> Option<(String, usize)> {
    let rest = &src[i + 1..];
    let end = rest.find('>')?;
    let uri = &rest[..end];
    let (scheme, tail) = uri.split_once(':')?;
    if scheme.is_empty()
        || scheme.len() > 32
        || !scheme.chars().next().unwrap().is_ascii_alphabetic()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        || tail.chars().any(|c| c.is_whitespace() || c == '<')
    {
        return None;
    }
    Some((uri.to_string(), i + 1 + end + 1))
}

/// Handle `]` at byte `i` (already consumed): resolve the most recent bracket into a link/image if
/// an inline `(dest "title")` tail follows, else degrade to literal text. Returns the new index.
fn close_bracket(nodes: &mut Vec<Node>, src: &str, i: usize) -> usize {
    let Some(open) = nodes
        .iter()
        .rposition(|n| matches!(n, Node::Bracket { .. }))
    else {
        nodes.push(Node::Done(Inline::Text("]".into())));
        return i;
    };
    let (image, active) = match nodes[open] {
        Node::Bracket { image, active } => (image, active),
        _ => unreachable!(),
    };
    let tail = if active {
        parse_link_tail(src, i)
    } else {
        None
    };
    let Some((dest, title, end)) = tail else {
        // No link: the bracket becomes literal text.
        nodes[open] = Node::Done(Inline::Text(if image { "![".into() } else { "[".into() }));
        nodes.push(Node::Done(Inline::Text("]".into())));
        return i;
    };

    // Emphasis inside the link content resolves now, scoped to the bracket.
    let mut content: Vec<Node> = nodes.split_off(open + 1);
    process_emphasis(&mut content, 0);
    let content = finalize(content);
    nodes[open] = Node::Done(if image {
        Inline::Image {
            dest,
            title,
            content,
        }
    } else {
        Inline::Link {
            dest,
            title,
            content,
        }
    });
    if !image {
        // Links don't nest: deactivate earlier link openers.
        for n in nodes.iter_mut() {
            if let Node::Bracket {
                image: false,
                active,
            } = n
            {
                *active = false;
            }
        }
    }
    end
}

/// Parse `(dest "title")` at byte `i` (pointing just past `]`). Returns (dest, title, next index).
fn parse_link_tail(src: &str, i: usize) -> Option<(String, String, usize)> {
    let b = src.as_bytes();
    if i >= b.len() || b[i] != b'(' {
        return None;
    }
    let mut p = skip_ws(b, i + 1);
    // Destination: <...> form or a balanced-paren run without whitespace.
    let mut dest = String::new();
    if p < b.len() && b[p] == b'<' {
        p += 1;
        while p < b.len() && b[p] != b'>' {
            if b[p] == b'\n' {
                return None;
            }
            if b[p] == b'\\' && p + 1 < b.len() && b[p + 1].is_ascii_punctuation() {
                dest.push(b[p + 1] as char);
                p += 2;
            } else {
                let c = src[p..].chars().next().unwrap();
                dest.push(c);
                p += c.len_utf8();
            }
        }
        if p >= b.len() {
            return None;
        }
        p += 1; // consume '>'
    } else {
        let mut depth = 0usize;
        while p < b.len() {
            let c = b[p];
            if c.is_ascii_whitespace() {
                break;
            }
            match c {
                b'(' => {
                    depth += 1;
                    dest.push('(');
                    p += 1;
                }
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    dest.push(')');
                    p += 1;
                }
                b'\\' if p + 1 < b.len() && b[p + 1].is_ascii_punctuation() => {
                    dest.push(b[p + 1] as char);
                    p += 2;
                }
                _ => {
                    let ch = src[p..].chars().next().unwrap();
                    dest.push(ch);
                    p += ch.len_utf8();
                }
            }
        }
        if depth != 0 {
            return None;
        }
    }
    p = skip_ws(b, p);
    // Optional title.
    let mut title = String::new();
    if p < b.len() && matches!(b[p], b'"' | b'\'' | b'(') {
        let close = match b[p] {
            b'"' => b'"',
            b'\'' => b'\'',
            _ => b')',
        };
        p += 1;
        loop {
            if p >= b.len() {
                return None;
            }
            if b[p] == close {
                p += 1;
                break;
            }
            if b[p] == b'\\' && p + 1 < b.len() && b[p + 1].is_ascii_punctuation() {
                title.push(b[p + 1] as char);
                p += 2;
            } else {
                let c = src[p..].chars().next().unwrap();
                title.push(c);
                p += c.len_utf8();
            }
        }
        p = skip_ws(b, p);
    }
    if p < b.len() && b[p] == b')' {
        Some((dest, title, p + 1))
    } else {
        None
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// CommonMark flanking for a delimiter run at byte `i` of length `n`. `kind` is `'*'` (also used
/// for `~~`) or `'_'` (stricter: no intraword emphasis).
fn flanking(src: &str, i: usize, n: usize, kind: char) -> (bool, bool) {
    let prev = src[..i].chars().next_back();
    let next = src[i + n..].chars().next();
    let ws = |c: Option<char>| c.is_none_or(|c| c.is_whitespace());
    let punct = |c: Option<char>| c.is_some_and(|c| c.is_ascii_punctuation());

    let left = !ws(next) && (!punct(next) || ws(prev) || punct(prev));
    let right = !ws(prev) && (!punct(prev) || ws(next) || punct(next));
    if kind == '_' {
        (
            left && (!right || punct(prev)),
            right && (!left || punct(next)),
        )
    } else {
        (left, right)
    }
}

/// The CommonMark `process_emphasis` loop over the working list, from `bottom` up: match closers to
/// the nearest compatible opener (honoring the mod-3 rule), wrapping the span in
/// `Strong`/`Emphasis`/`Strikethrough` nodes until no matches remain.
fn process_emphasis(nodes: &mut Vec<Node>, bottom: usize) {
    let mut closer = bottom;
    'outer: while closer < nodes.len() {
        let Node::Delim(c) = &nodes[closer] else {
            closer += 1;
            continue;
        };
        if !c.can_close {
            closer += 1;
            continue;
        }
        let (c_ch, c_orig, c_can_open) = (c.ch, c.orig, c.can_open);

        // Look back for the nearest compatible opener.
        let mut o = closer;
        while o > bottom {
            o -= 1;
            let Node::Delim(op) = &nodes[o] else { continue };
            if op.ch != c_ch || !op.can_open || op.count == 0 {
                continue;
            }
            if c_ch == '~' {
                // ~~ pairs exactly.
            } else if (c_can_open || op.can_close)
                && (op.orig + c_orig) % 3 == 0
                && !(op.orig % 3 == 0 && c_orig % 3 == 0)
            {
                continue; // mod-3 rule
            }

            // Matched: build the wrapped node.
            let use_delims = {
                let (Node::Delim(op), Node::Delim(cl)) = (&nodes[o], &nodes[closer]) else {
                    unreachable!()
                };
                // `~~` always pairs whole; `*`/`_` take 2 when both runs allow strong.
                if c_ch == '~' || (op.count >= 2 && cl.count >= 2) {
                    2
                } else {
                    1
                }
            };
            // Everything between opener and closer becomes the wrapped content (leftover inner
            // delimiters degrade to literal text — they can no longer match anything).
            let inner: Vec<Node> = nodes.drain(o + 1..closer).collect();
            let content = finalize(inner);
            let wrapped = match (c_ch, use_delims) {
                ('~', _) => Inline::Strikethrough(content),
                (_, 2) => Inline::Strong(content),
                _ => Inline::Emphasis(content),
            };
            // `closer` has shifted left by the drained span; it now sits right after the opener.
            let closer_at = o + 1;
            if let Node::Delim(op) = &mut nodes[o] {
                op.count -= use_delims;
            }
            if let Node::Delim(cl) = &mut nodes[closer_at] {
                cl.count -= use_delims;
            }
            nodes.insert(closer_at, Node::Done(wrapped));
            let mut next_closer = closer_at + 1; // the (possibly spent) closer delimiter
            if let Node::Delim(cl) = &nodes[next_closer] {
                if cl.count == 0 {
                    nodes.remove(next_closer);
                }
            }
            if let Node::Delim(op) = &nodes[o] {
                if op.count == 0 {
                    nodes.remove(o);
                    next_closer -= 1;
                }
            }
            closer = next_closer;
            continue 'outer;
        }

        // No opener found: a closer that can't also open will never match — freeze it as text.
        if !c_can_open {
            let Node::Delim(d) = &nodes[closer] else {
                unreachable!()
            };
            nodes[closer] = Node::Done(Inline::Text(d.ch.to_string().repeat(d.count)));
        }
        closer += 1;
    }
}

/// Turn leftover delimiters/brackets into literal text, merge adjacent text runs, drop empties.
fn finalize(nodes: Vec<Node>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for n in nodes {
        let inline = match n {
            Node::Done(i) => i,
            Node::Delim(d) => Inline::Text(d.ch.to_string().repeat(d.count)),
            Node::Bracket { image, .. } => {
                Inline::Text(if image { "![".into() } else { "[".into() })
            }
        };
        match (&inline, out.last_mut()) {
            (Inline::Text(t), _) if t.is_empty() => {}
            (Inline::Text(t), Some(Inline::Text(prev))) => prev.push_str(t),
            _ => out.push(inline),
        }
    }
    out
}
