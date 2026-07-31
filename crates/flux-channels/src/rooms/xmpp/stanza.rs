//! The XML the [`XmppMucRoom`](super::XmppMucRoom) reads and writes — a namespace-resolving element
//! tree over `quick-xml`, and nothing more.
//!
//! **Why not an XMPP client crate.** `tokio-xmpp` opens its own TCP socket and resolves its own DNS,
//! neither of which can be routed through `flux_system::net`'s egress guard, and it carries a full XEP
//! stack for a surface that needs presence, groupchat and private messages. So flux borrows a *parser*
//! and owns the protocol.
//!
//! **The namespace is not optional, and this module is where that is enforced.** [`Element::new`]
//! demands a namespace, so there is no way to build one without deciding; [`stanza`] is the only
//! constructor for the three stanza kinds and it always answers `jabber:client`. An unqualified stanza
//! is answered by prosody with `<unsupported-stanza-type/>` and the stream is closed — the spike paid
//! for that finding, and `tests/xmpp_room.rs` regresses it on the raw bytes rather than trusting this
//! comment.

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};

use flux_core::{Error, Result};

/// The namespace **every** `message`/`presence`/`iq` this crate emits carries.
pub(super) const NS_CLIENT: &str = "jabber:client";
/// RFC 7395's stream-framing namespace — `<open/>` and `<close/>`.
pub(super) const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
pub(super) const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
pub(super) const NS_BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";
pub(super) const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub(super) const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub(super) const NS_PING: &str = "urn:xmpp:ping";
pub(super) const NS_DELAY: &str = "urn:xmpp:delay";

/// One XML element: its local name, its resolved namespace, unprefixed attributes, text and children.
///
/// Prefixes are resolved away on the way in (a server may write `<stream:features>` or
/// `<features xmlns=…>`; both arrive here as local name `features`) and never written on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Element {
    name: String,
    /// `None` only for an element a server sent under a prefix it never bound — tolerated on read so
    /// one malformed frame does not end a live session, and impossible on write.
    ns: Option<String>,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Element>,
}

impl Element {
    /// A new element in `ns`. The namespace is a parameter and not an `Option` on purpose: it is the
    /// single thing that makes a stanza acceptable to a real server.
    pub(super) fn new(name: impl Into<String>, ns: &str) -> Self {
        Self {
            name: name.into(),
            ns: Some(ns.to_string()),
            attrs: Vec::new(),
            text: String::new(),
            children: Vec::new(),
        }
    }

    /// Set an attribute. An empty value is dropped, so an absent `to` never becomes `to=''`.
    pub(super) fn attr(mut self, name: &str, value: impl AsRef<str>) -> Self {
        let value = value.as_ref();
        if !value.is_empty() {
            self.attrs.push((name.to_string(), value.to_string()));
        }
        self
    }

    /// Set the element's character data.
    pub(super) fn text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    /// Append a child element.
    pub(super) fn child(mut self, child: Element) -> Self {
        self.children.push(child);
        self
    }

    /// Append a child only when `child` is `Some` — the shape optional protocol bits take.
    pub(super) fn maybe_child(self, child: Option<Element>) -> Self {
        match child {
            Some(child) => self.child(child),
            None => self,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn ns(&self) -> Option<&str> {
        self.ns.as_deref()
    }

    pub(super) fn get(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub(super) fn body_text(&self) -> &str {
        &self.text
    }

    pub(super) fn children(&self) -> impl Iterator<Item = &Element> {
        self.children.iter()
    }

    /// The first child with this local name, at any depth of *this* element's direct children.
    pub(super) fn child_named(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The first direct child in `ns`.
    pub(super) fn child_in(&self, ns: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.ns.as_deref() == Some(ns))
    }

    /// Whether this element or any descendant is in `ns` — how a `<delay/>` marker buried in a
    /// message, or a `<ping/>` inside an IQ, is spotted without hard-coding a path.
    pub(super) fn has_descendant_in(&self, ns: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.ns.as_deref() == Some(ns) || c.has_descendant_in(ns))
    }

    /// Serialize to a single WebSocket frame's worth of XML. A root element always declares its own
    /// namespace — which is what makes a stanza acceptable to a real server.
    pub(super) fn to_xml(&self) -> String {
        let mut out = String::new();
        self.write_into(&mut out, None);
        out
    }

    /// `inherited` is the namespace already in scope from the enclosing element, so a child in the
    /// same namespace does not repeat the declaration.
    fn write_into(&self, out: &mut String, inherited: Option<&str>) {
        out.push('<');
        out.push_str(&self.name);
        if self.ns.as_deref() != inherited {
            if let Some(ns) = &self.ns {
                out.push_str(" xmlns='");
                escape_into(ns, out);
                out.push('\'');
            }
        }
        for (k, v) in &self.attrs {
            out.push(' ');
            out.push_str(k);
            out.push_str("='");
            escape_into(v, out);
            out.push('\'');
        }
        if self.text.is_empty() && self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        escape_into(&self.text, out);
        for child in &self.children {
            child.write_into(out, self.ns.as_deref());
        }
        out.push_str("</");
        out.push_str(&self.name);
        out.push('>');
    }
}

/// A `message` / `presence` / `iq`, **always** `jabber:client`-qualified. The only door to the three
/// stanza kinds, so "did we remember the namespace?" is not a question a caller can get wrong.
pub(super) fn stanza(name: &str) -> Element {
    Element::new(name, NS_CLIENT)
}

/// Escape the five XML metacharacters. Single quotes are escaped because attribute values are written
/// single-quoted; both quote forms are escaped so text and attributes share one routine.
fn escape_into(raw: &str, out: &mut String) {
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
}

/// Parse one WebSocket frame into its root element. RFC 7395 puts exactly one element per frame, so a
/// frame that is empty, whitespace, or holds no element is an error rather than a silent `None`.
pub(super) fn parse(frame: &str) -> Result<Element> {
    let mut reader = NsReader::from_str(frame);
    reader.config_mut().expand_empty_elements = false;

    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut buf = Vec::new();

    loop {
        let (resolved, event) = reader
            .read_resolved_event_into(&mut buf)
            .map_err(|e| Error::Other(format!("xmpp: malformed stanza: {e}")))?;
        match event {
            Event::Start(ref start) => stack.push(open(resolved, start)?),
            Event::Empty(ref start) => {
                let element = open(resolved, start)?;
                close(element, &mut stack, &mut root);
            }
            Event::End(_) => {
                let Some(element) = stack.pop() else {
                    return Err(Error::Other("xmpp: unbalanced stanza".into()));
                };
                close(element, &mut stack, &mut root);
            }
            Event::Text(ref text) => {
                if let Some(top) = stack.last_mut() {
                    let decoded = text
                        .decode()
                        .map_err(|e| Error::Other(format!("xmpp: undecodable text: {e}")))?;
                    top.text.push_str(decoded.as_ref());
                }
            }
            Event::CData(data) => {
                if let Some(top) = stack.last_mut() {
                    top.text
                        .push_str(&String::from_utf8_lossy(data.into_inner().as_ref()));
                }
            }
            // quick-xml reports `&amp;` / `&#38;` as their own event rather than folding them into
            // the surrounding text, so an unresolved one would silently *delete* a character from a
            // room message. Resolve it, or reject the frame.
            Event::GeneralRef(reference) => {
                let name = reference
                    .decode()
                    .map_err(|e| Error::Other(format!("xmpp: undecodable entity: {e}")))?;
                let resolved = resolve_reference(&name)
                    .ok_or_else(|| Error::Other(format!("xmpp: unknown entity &{name};")))?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&resolved);
                }
            }
            Event::Eof => break,
            // Declarations, comments, processing instructions and doctypes carry nothing a stanza
            // means, and a server that sends one must not end the session.
            _ => {}
        }
        buf.clear();
    }

    root.ok_or_else(|| Error::Other("xmpp: frame carried no element".into()))
}

/// One entity or character reference, as text. XMPP defines no entities of its own (RFC 6120 §11.1
/// forbids a DTD outright), so this is the five predefined ones plus numeric character references —
/// and an unknown name is an error rather than an empty string.
fn resolve_reference(name: &str) -> Option<String> {
    if let Some(predefined) = quick_xml::escape::resolve_xml_entity(name) {
        return Some(predefined.to_string());
    }
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code).map(String::from)
}

/// Build an element from a start tag, resolving its namespace and dropping `xmlns` declarations from
/// the attribute list (they are the namespace, not data).
fn open(resolved: ResolveResult<'_>, start: &quick_xml::events::BytesStart<'_>) -> Result<Element> {
    let name = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
    let ns = match resolved {
        ResolveResult::Bound(ns) => Some(String::from_utf8_lossy(ns.as_ref()).into_owned()),
        // A prefix the server never bound. Tolerated: the element still has a usable local name, and
        // one odd frame must not tear down a live room.
        ResolveResult::Unbound | ResolveResult::Unknown(_) => None,
    };

    let mut attrs = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| Error::Other(format!("xmpp: bad attribute: {e}")))?;
        let key = attribute.key;
        if key.as_ref() == b"xmlns" || key.prefix().is_some_and(|p| p.as_ref() == b"xmlns") {
            continue;
        }
        // XMPP is XML 1.0 (RFC 6120 §11.1), and the WebSocket binding carries no XML declaration.
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|e| Error::Other(format!("xmpp: bad attribute value: {e}")))?
            .into_owned();
        attrs.push((
            String::from_utf8_lossy(key.local_name().as_ref()).into_owned(),
            value,
        ));
    }

    Ok(Element {
        name,
        ns,
        attrs,
        text: String::new(),
        children: Vec::new(),
    })
}

/// Attach a finished element to its parent, or make it the root.
fn close(element: Element, stack: &mut [Element], root: &mut Option<Element>) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stanza_kind_is_jabber_client_qualified() {
        // The unit-level half of the regression: the only constructor for a stanza qualifies it, so a
        // caller cannot forget. `tests/xmpp_room.rs` asserts the same property on the wire.
        for kind in ["message", "presence", "iq"] {
            let xml = stanza(kind).to_xml();
            assert!(
                xml.starts_with(&format!("<{kind} xmlns='jabber:client'")),
                "unqualified stanza would be refused by prosody: {xml}"
            );
        }
    }

    #[test]
    fn text_and_attributes_are_escaped() {
        // Room text is untrusted multi-party input: an occupant typing `</body>` must not be able to
        // forge a stanza boundary.
        let xml = stanza("message")
            .attr("to", "a'b")
            .child(Element::new("body", NS_CLIENT).text("</body><x>&"))
            .to_xml();
        assert!(xml.contains("to='a&apos;b'"), "{xml}");
        assert!(xml.contains("&lt;/body&gt;&lt;x&gt;&amp;"), "{xml}");
        assert!(!xml.contains("<x>"), "no injected element survives: {xml}");
    }

    #[test]
    fn an_empty_attribute_is_omitted_rather_than_written_blank() {
        assert_eq!(
            stanza("presence").attr("to", "").to_xml(),
            "<presence xmlns='jabber:client'/>"
        );
    }

    #[test]
    fn a_prefixed_server_element_resolves_to_its_local_name_and_namespace() {
        let features = parse(
            "<stream:features xmlns:stream='http://etherx.jabber.org/streams'>\
             <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></stream:features>",
        )
        .unwrap();
        assert_eq!(features.name(), "features");
        assert!(features.child_in(NS_BIND).is_some());
    }

    #[test]
    fn an_unbound_prefix_is_tolerated_rather_than_fatal() {
        // Some servers write `<stream:features>` without rebinding the prefix on the WebSocket
        // binding. One odd frame must not end a live room.
        let features = parse(
            "<stream:features><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/></stream:features>",
        )
        .unwrap();
        assert_eq!(features.name(), "features");
        assert_eq!(features.ns(), None);
        assert!(features.child_in(NS_BIND).is_some());
    }

    #[test]
    fn a_child_in_its_parents_namespace_does_not_repeat_the_declaration() {
        // The stanza still declares `jabber:client`; the `<body>` inherits it, the way every real
        // client writes it. A nested element in a *different* namespace still declares its own.
        assert_eq!(
            stanza("message")
                .child(Element::new("body", NS_CLIENT).text("hi"))
                .to_xml(),
            "<message xmlns='jabber:client'><body>hi</body></message>"
        );
        assert_eq!(
            stanza("presence").child(Element::new("x", NS_MUC)).to_xml(),
            "<presence xmlns='jabber:client'><x xmlns='http://jabber.org/protocol/muc'/></presence>"
        );
    }

    #[test]
    fn a_message_round_trips_through_the_parser() {
        let out = stanza("message")
            .attr("to", "standup@conference.example.org")
            .attr("type", "groupchat")
            .child(Element::new("body", NS_CLIENT).text("hi & bye"))
            .to_xml();
        let back = parse(&out).unwrap();
        assert_eq!(back.name(), "message");
        assert_eq!(back.ns(), Some(NS_CLIENT));
        assert_eq!(back.get("type"), Some("groupchat"));
        assert_eq!(back.child_named("body").unwrap().body_text(), "hi & bye");
    }

    #[test]
    fn entities_and_character_references_survive_the_trip() {
        // quick-xml surfaces `&amp;` as its own event; dropping it would silently delete a character
        // from somebody's message.
        let message = parse(
            "<message xmlns='jabber:client'><body>a &amp; b &lt;c&gt; &#38; &#x1F600;</body></message>",
        )
        .unwrap();
        assert_eq!(
            message.child_named("body").unwrap().body_text(),
            "a & b <c> & \u{1F600}"
        );
        assert!(
            parse("<message xmlns='jabber:client'><body>&nope;</body></message>").is_err(),
            "an entity we cannot resolve is a bad frame, not a silent deletion"
        );
    }

    #[test]
    fn a_delay_marker_is_found_at_any_depth() {
        let history = parse(
            "<message xmlns='jabber:client' type='groupchat' from='r@c/x'><body>old</body>\
             <delay xmlns='urn:xmpp:delay' stamp='2026-07-30T09:00:00Z'/></message>",
        )
        .unwrap();
        assert!(history.has_descendant_in(NS_DELAY));
        assert!(!history.has_descendant_in(NS_MUC_USER));
    }

    #[test]
    fn an_empty_frame_is_an_error_not_a_silent_nothing() {
        assert!(parse("").is_err());
        assert!(
            parse("   ").is_err(),
            "a whitespace frame carries no stanza"
        );
    }
}
