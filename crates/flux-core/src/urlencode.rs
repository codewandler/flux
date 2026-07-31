//! The one RFC 3986 percent-encoder in the tree.
//!
//! Three crates independently hand-rolled the identical loop — the OAuth authorize URL builder
//! (`flux-credentials`), the SigV4 canonical-URI encoder (`flux-providers`), and the plugin host's
//! endpoint-template substitution — and C-303 needed a fourth for `http.request`'s structured
//! `query` map. A percent-encoder is exactly the kind of thing that must not be copied: a copy
//! drifts on the character where it matters, and the character where it matters is the one an
//! attacker supplies. It lives here, in L0, because every one of those crates already depends on
//! `flux-core`.
//!
//! This is **not** the form-body encoder. `application/x-www-form-urlencoded` spells a space `+`
//! and is implemented by `flux-lang`'s `urlencode_component`; RFC 3986 spells it `%20`. The two
//! differ on exactly the byte most likely to appear in a real value, so they stay separate
//! functions with separate names rather than one function with a flag.

/// Percent-encode `s` as a single URI component per [RFC 3986 §2.3][rfc].
///
/// The unreserved set — ASCII alphanumerics and `-`, `.`, `_`, `~` — travels as itself; **every**
/// other byte of the UTF-8 encoding becomes `%XX` with upper-case hex. That deliberately includes
/// the sub-delimiters (`&`, `=`, `+`, `;`, `,`) and the generic delimiters (`?`, `#`, `/`, `:`),
/// because a component is being placed *into* a URI: an unencoded `&` or `=` in a query value adds
/// a parameter the caller did not write, and an unencoded `/` in a path segment adds a segment.
///
/// Upper-case hex is the RFC's own recommendation and is what SigV4 canonicalization requires, so
/// a shared encoder cannot use the lower-case spelling.
///
/// [rfc]: https://datatracker.ietf.org/doc/html/rfc3986#section-2.3
pub fn percent_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreserved_bytes_travel_as_themselves() {
        let unreserved = "abcXYZ019-._~";
        assert_eq!(percent_encode_component(unreserved), unreserved);
    }

    #[test]
    fn every_byte_that_could_restructure_a_uri_is_encoded() {
        // The sub-delimiters and generic delimiters, one by one — this is the set that makes an
        // interpolated value able to rewrite the request.
        for (raw, encoded) in [
            ("&", "%26"),
            ("=", "%3D"),
            ("?", "%3F"),
            ("#", "%23"),
            ("/", "%2F"),
            (":", "%3A"),
            ("+", "%2B"),
            (";", "%3B"),
            (",", "%2C"),
            ("%", "%25"),
        ] {
            assert_eq!(percent_encode_component(raw), encoded, "encoding {raw:?}");
        }
    }

    #[test]
    fn a_space_is_pct_twenty_not_plus() {
        // The single line that separates this from the form-body serializer.
        assert_eq!(percent_encode_component("a b"), "a%20b");
    }

    #[test]
    fn non_ascii_is_encoded_per_utf8_byte_in_upper_case_hex() {
        assert_eq!(percent_encode_component("ü"), "%C3%BC");
        assert_eq!(percent_encode_component("日"), "%E6%97%A5");
    }
}
