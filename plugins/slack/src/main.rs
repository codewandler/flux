//! `slack` — a flux integration plugin for the Slack Web API: token info, messaging, threads, search,
//! reactions, channels, files (via host blobs), bookmarks, users, presence, and emoji. Authenticates
//! with tokens injected as bearer headers — purpose `bot_token` for most calls, `user_token` for the
//! search-scoped reads (search/mentions/unreads) and presence writes. Every call goes through the host
//! by endpoint reference (`slack.endpoint`, base URL from the required `SLACK_API_URL` env) plus the
//! method path — the plugin never composes a URL. List ops contribute datasource records
//! (`slack.channel` / `slack.user`) so the agent can search them; `slack.index.build` rebuilds both.
//!
//! Slack replies are JSON carrying an `"ok": bool`; a falsey `ok` is surfaced as an error built from the
//! response's `"error"` field. File ops never inline base64 — `slack.file.upload` reads its bytes from a
//! host `blob_ref`, and the download ops stage the fetched bytes back into the blob store, returning a ref.

use base64::Engine as _;
use host_kit::*;
use serde_json::{json, Value};

mod client;
mod manifest;
mod operations;
mod schema;

use client::*;
use manifest::manifest_builder;
use operations::*;
use schema::*;

fn main() -> Result<(), String> {
    manifest_builder().try_serve()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Plugin {
        manifest_builder().build()
    }

    fn host() -> MockHost {
        MockHost::default()
            .with_endpoint_ref("slack.endpoint", "https://slack.com/api")
            .with_secret("bot_token", "xoxb")
            .with_secret("user_token", "xoxp")
    }

    #[test]
    fn auth_test_probes_both_tokens() {
        let mut h = host().with_http(
            "auth.test",
            json!({ "ok": true, "team": "Acme", "user": "bot", "team_id": "T1", "user_id": "U1" }),
        );
        let out = plugin().call("slack.test", json!({}), &mut h).unwrap();
        assert_eq!(out["status"], "ok");
        assert_eq!(out["count"], 2);
        assert_eq!(out["tokens"][0]["role"], "user");
        assert_eq!(out["tokens"][1]["role"], "bot");
    }

    #[test]
    fn info_reports_identity() {
        let mut h = host().with_http(
            "auth.test",
            json!({ "ok": true, "team": "Acme", "user_id": "U1" }),
        );
        let out = plugin().call("slack.info", json!({}), &mut h).unwrap();
        assert_eq!(out["tokens"][0]["team"], "Acme");
    }

    #[test]
    fn message_send_posts_and_returns_the_ts() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "123.45" }));
        let out = plugin()
            .call(
                "slack.message.send",
                json!({ "channel": "C1", "text": "hello", "thread_ts": "100.1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ts"], "123.45");
    }

    #[test]
    fn message_list_reads_history() {
        let mut h = host().with_http(
            "conversations.history",
            json!({
                "ok": true,
                "messages": [{
                    "ts": "1.1",
                    "text": "hi",
                    "vendor_message_field": { "nested": true }
                }],
                "has_more": true,
                "response_metadata": { "next_cursor": "next-page" },
                "vendor_envelope_field": [1, 2, 3]
            }),
        );
        let out = plugin()
            .call(
                "slack.message.list",
                json!({
                    "channel": "C1",
                    "limit": 5,
                    "cursor": "cursor 1",
                    "oldest": "1.0",
                    "latest": "2.0"
                }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"][0]["text"], "hi");
        assert_eq!(out["messages"][0]["vendor_message_field"]["nested"], true);
        assert_eq!(out["response_metadata"]["next_cursor"], "next-page");
        assert_eq!(out["vendor_envelope_field"], json!([1, 2, 3]));
        let calls = h.calls.borrow();
        let (_, request) = calls
            .iter()
            .find(|(command, _)| command == "http.do")
            .expect("history HTTP request recorded");
        assert_eq!(request["endpoint_ref"], "slack.endpoint");
        assert_eq!(request["auth_purpose"], "bot_token");
        assert_eq!(
            request["path"],
            "/conversations.history?channel=C1&limit=5&inclusive=true&cursor=cursor%201&oldest=1.0&latest=2.0"
        );
    }

    #[test]
    fn message_edit_resolves_ref() {
        let mut h = host().with_http("chat.update", json!({ "ok": true, "ts": "1.1" }));
        let out = plugin()
            .call(
                "slack.message.edit",
                json!({ "ref": "C9:1.1", "text": "fixed" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn message_delete_uses_channel_and_ts() {
        let mut h = host().with_http("chat.delete", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.message.delete",
                json!({ "channel": "C1", "ts": "1.1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn thread_reads_replies_from_permalink_ref() {
        let mut h = host().with_http(
            "conversations.replies",
            json!({
                "ok": true,
                "messages": [
                    { "ts": "1.1", "vendor_root": "kept" },
                    { "ts": "1.2", "vendor_reply": { "kind": "future" } }
                ],
                "response_metadata": { "next_cursor": "thread-next" }
            }),
        );
        let out = plugin()
            .call(
                "slack.thread",
                json!({ "ref": "https://acme.slack.com/archives/C0123ABCD/p1718031600123456" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"].as_array().unwrap().len(), 2);
        assert_eq!(out["messages"][1]["vendor_reply"]["kind"], "future");
        assert_eq!(out["response_metadata"]["next_cursor"], "thread-next");
        let calls = h.calls.borrow();
        let (_, request) = calls
            .iter()
            .find(|(command, _)| command == "http.do")
            .expect("thread HTTP request recorded");
        assert_eq!(request["auth_purpose"], "bot_token");
        assert_eq!(
            request["path"],
            "/conversations.replies?channel=C0123ABCD&ts=1718031600.123456&limit=100&inclusive=true"
        );
    }

    #[test]
    fn search_uses_the_user_token() {
        let mut h = host().with_http(
            "search.messages",
            json!({ "ok": true, "messages": { "matches": [{ "text": "found" }], "total": 1 } }),
        );
        let out = plugin()
            .call("slack.search", json!({ "query": "deploy" }), &mut h)
            .unwrap();
        assert_eq!(out["messages"]["matches"][0]["text"], "found");
    }

    #[test]
    fn mentions_classifies_handling_status() {
        // The matched mention sits in a thread; the bot identity (U_me) authored a later reply,
        // so the mention classifies as `replied`. The search match carries channel + ts +
        // permalink (with a thread_ts), and own-identity resolution comes from auth.test.
        // Use a current ts: the default (empty) `since` floors to today's midnight, so stale
        // timestamps would be dropped (matching the reference's `mentionSince` semantics).
        let now = unix_now();
        let root = format!("{now}.000000");
        let matched = format!("{now}.000001");
        let later = format!("{now}.000002");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_me> please look",
                    "permalink": format!("https://acme.slack.com/archives/C1/p1001000000?thread_ts={root}"),
                    "channel": { "id": "C1", "name": "dev" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": true, "messages": [
                    { "ts": root, "user": "U2", "text": "root" },
                    { "ts": matched, "user": "U2", "text": "<@U_me> please look" },
                    { "ts": later, "user": "U_me", "text": "on it" }
                ] }),
            );
        let out = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap();
        assert_eq!(out["target"], "U_me");
        assert_eq!(out["count"], 1);
        assert_eq!(out["total"], 1);
        assert_eq!(out["mentions"][0]["channel"], "C1");
        assert_eq!(out["mentions"][0]["ts"], matched);
        assert_eq!(out["mentions"][0]["thread_ts"], root);
        assert_eq!(out["mentions"][0]["status"], "replied");
        // New residual fields are always present in the envelope.
        assert_eq!(out["since"], "");
        assert_eq!(out["unhandled"], false);
        assert!(out["tickets"].as_array().unwrap().is_empty());
    }

    #[test]
    fn mentions_unhandled_filters_out_handled() {
        // Two current-ts matches: the first is replied-to by U_me (→ filtered out by `unhandled`);
        // the second has an empty channel so classification short-circuits to `pending` w/o HTTP.
        let now = unix_now();
        let root = format!("{now}.000000");
        let handled = format!("{now}.000001");
        let later = format!("{now}.000002");
        let pending = format!("{now}.000003");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 2, "matches": [
                    {
                        "ts": handled, "user": "U2", "text": "<@U_me> a",
                        "permalink": format!("https://acme.slack.com/archives/C1/p1001000000?thread_ts={root}"),
                        "channel": { "id": "C1" }
                    },
                    {
                        "ts": pending, "user": "U2", "text": "<@U_me> b",
                        "permalink": "https://acme.slack.com/archives/C2/p2001000000",
                        "channel": { "id": "" }
                    }
                ] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": true, "messages": [
                    { "ts": root, "user": "U2", "text": "root" },
                    { "ts": handled, "user": "U2", "text": "<@U_me> a" },
                    { "ts": later, "user": "U_me", "text": "on it" }
                ] }),
            );
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "unhandled": true }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["unhandled"], true);
        assert_eq!(out["count"], 1);
        assert_eq!(out["mentions"][0]["ts"], pending);
        assert_eq!(out["mentions"][0]["status"], "pending");
        // Total still reflects the raw search total, not the filtered count.
        assert_eq!(out["total"], 2);
    }

    #[test]
    fn mentions_since_drops_older_matches() {
        // A `since` of 1h yields a unix lower bound; the old match (ts 1.0) is dropped, the
        // recent one (now) is kept. (The `after:` search term is exercised by the unit test.)
        let now = unix_now();
        let recent = format!("{now}.000100");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 2, "matches": [
                    {
                        "ts": "1.0", "user": "U2", "text": "<@U_me> old",
                        "permalink": "https://acme.slack.com/archives/C1/p1000000",
                        "channel": { "id": "" }
                    },
                    {
                        "ts": recent, "user": "U2", "text": "<@U_me> new",
                        "permalink": "https://acme.slack.com/archives/C1/pnew",
                        "channel": { "id": "" }
                    }
                ] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "since": "1h" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["since"], "1h");
        assert_eq!(out["count"], 1, "the >1h-old match must be dropped");
        assert_eq!(out["mentions"][0]["text"], "<@U_me> new");
    }

    #[test]
    fn mention_since_builds_after_term() {
        // Empty since → floor to UTC midnight; the `after:` term is one day earlier.
        let (unix, after) = mention_since("").unwrap();
        assert_eq!(unix % 86_400, 0, "empty since floors to UTC midnight");
        assert_eq!(after, civil_date(unix - 86_400));
        // A duration since → now - duration; after term is a valid YYYY-MM-DD.
        let (unix2, after2) = mention_since("7d").unwrap();
        assert!(unix2 > 0);
        assert_eq!(after2.len(), 10);
        assert!(mention_since("bogus").is_err());
    }

    #[test]
    fn unread_since_defaults_to_14d() {
        let (unix, label) = unread_since("").unwrap();
        assert_eq!(label, "14d");
        assert!(unix > 0);
        let (_, label2) = unread_since("1h").unwrap();
        assert_eq!(label2, "1h");
        assert!(unread_since("nope").is_err());
    }

    #[test]
    fn mentions_extracts_tickets() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        // Empty channel → classify short-circuits to pending (no thread HTTP needed).
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched, "user": "U2",
                    "text": "<@U_me> see DEV-42 and TEL-7, also dev-99",
                    "permalink": "https://acme.slack.com/archives/C1/p1001000000",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "tickets": true }),
                &mut h,
            )
            .unwrap();
        // Per-item tickets: default rule is case-sensitive uppercase, so `dev-99` is NOT matched
        // (mirrors fluxplane's non-(?i) default); uppercased, deduped, sorted.
        assert_eq!(out["mentions"][0]["tickets"], json!(["DEV-42", "TEL-7"]));
        // Aggregate: one record per key, sorted, with the permalink.
        let agg = out["tickets"].as_array().unwrap();
        assert_eq!(agg.len(), 2);
        assert_eq!(agg[0]["key"], "DEV-42");
        assert_eq!(agg[0]["mentions"], 1);
        assert_eq!(
            agg[0]["permalinks"][0],
            "https://acme.slack.com/archives/C1/p1001000000"
        );
    }

    #[test]
    fn mentions_tickets_honour_explicit_keys() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched, "user": "U2",
                    "text": "dev-1 ABC-2 TEL-3",
                    "permalink": "https://x",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "user": "U_me", "tickets": true, "ticket_keys": ["dev", "tel"] }),
                &mut h,
            )
            .unwrap();
        // Keyed rule is case-insensitive: `dev-1` matches DEV; ABC-2 is excluded.
        assert_eq!(out["mentions"][0]["tickets"], json!(["DEV-1", "TEL-3"]));
    }

    #[test]
    fn extract_tickets_matches_reference_rule() {
        // Default rule: `[A-Z][A-Z0-9]+-<digits>` on word boundaries, deduped + sorted.
        assert_eq!(
            extract_tickets("FLUX-1 flux-1 NOPE A-1 X9-12 trailing FOO-12bar", &[]),
            // A-1 needs ≥2 prefix chars → excluded; FOO-12bar has a trailing word char → excluded.
            json!(["FLUX-1", "X9-12"]).as_array().unwrap().to_vec()
        );
        // Keyed rule is case-insensitive on the prefix.
        assert_eq!(
            extract_tickets("dev-5 OTHER-9", &["DEV".to_string()]),
            vec!["DEV-5".to_string()]
        );
    }

    #[test]
    fn mentions_pending_when_unanswered() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_me> ping",
                    "permalink": "https://acme.slack.com/archives/C1/p2001000000",
                    "channel": { "id": "C1" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": true, "messages": [
                    { "ts": matched, "user": "U2", "text": "<@U_me> ping" }
                ] }),
            );
        let out = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap();
        assert_eq!(out["mentions"][0]["status"], "pending");
        assert_eq!(out["mentions"][0]["thread_ts"], "");
    }

    #[test]
    fn mentions_surfaces_search_errors() {
        let mut h = host().with_http(
            "search.messages",
            json!({ "ok": false, "error": "invalid_auth" }),
        );
        let err = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap_err();
        assert!(err.contains("invalid_auth"));
    }

    #[test]
    fn mentions_surfaces_thread_errors() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_me> ping",
                    "permalink": "https://acme.slack.com/archives/C1/p2001000000",
                    "channel": { "id": "C1" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }))
            .with_http(
                "conversations.replies",
                json!({ "ok": false, "error": "ratelimited" }),
            );
        let err = plugin()
            .call("slack.mentions", json!({ "user": "U_me" }), &mut h)
            .unwrap_err();
        assert!(err.contains("ratelimited"));
    }

    #[test]
    fn unreads_counts_genuine_unreads_after_last_read() {
        // The channel's `last_read` cursor drives the `oldest` history window so only messages
        // genuinely after the cursor count; Slack returns newest-first so we reverse them.
        let mut h = host()
            .with_http(
                "users.conversations",
                json!({ "ok": true, "channels": [{ "id": "C1", "name": "dev", "last_read": "1.0" }] }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": true, "messages": [
                    { "ts": "1.3", "text": "newest" },
                    { "ts": "1.2", "text": "middle" }
                ] }),
            );
        let out = plugin().call("slack.unreads", json!({}), &mut h).unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["since"], "14d"); // default window echoed
        assert_eq!(out["channels"][0]["unread_count"], 2);
        assert_eq!(out["channels"][0]["last_read"], "1.0");
        // chronological order after reversing Slack's newest-first history
        assert_eq!(out["channels"][0]["messages"][0]["ts"], "1.2");
        assert_eq!(out["channels"][0]["messages"][1]["ts"], "1.3");
    }

    #[test]
    fn unreads_echoes_explicit_since_label() {
        // An explicit `since` is echoed verbatim; the cursor math (last_read) is unchanged.
        let mut h = host()
            .with_http(
                "users.conversations",
                json!({ "ok": true, "channels": [{ "id": "C1", "name": "dev", "last_read": "1.0" }] }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": true, "messages": [ { "ts": "1.3", "text": "x" } ] }),
            );
        let out = plugin()
            .call("slack.unreads", json!({ "since": "7d" }), &mut h)
            .unwrap();
        assert_eq!(out["since"], "7d");
        assert_eq!(out["channels"][0]["last_read"], "1.0");
        assert_eq!(out["channels"][0]["unread_count"], 1);
    }

    #[test]
    fn unreads_surfaces_history_errors() {
        let mut h = host()
            .with_http(
                "users.conversations",
                json!({ "ok": true, "channels": [{
                    "id": "C1",
                    "name": "dev",
                    "last_read": "1.0",
                    "latest": { "ts": "2.0" }
                }] }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": false, "error": "ratelimited" }),
            );
        let err = plugin()
            .call("slack.unreads", json!({}), &mut h)
            .unwrap_err();
        assert!(err.contains("ratelimited"));
    }

    #[test]
    fn unreads_paginates_conversations() {
        let mut h = host()
            .with_http(
                "cursor=page-2",
                json!({
                    "ok": true,
                    "channels": [{
                        "id": "C2",
                        "name": "ops",
                        "last_read": "1.0",
                        "latest": { "ts": "2.0" }
                    }],
                    "response_metadata": { "next_cursor": "" }
                }),
            )
            .with_http(
                "users.conversations",
                json!({
                    "ok": true,
                    "channels": [{
                        "id": "C1",
                        "name": "dev",
                        "last_read": "1.0",
                        "latest": { "ts": "1.0" }
                    }],
                    "response_metadata": { "next_cursor": "page-2" }
                }),
            )
            .with_http(
                "conversations.history",
                json!({ "ok": true, "messages": [{ "ts": "2.0", "text": "page two" }] }),
            );
        let out = plugin().call("slack.unreads", json!({}), &mut h).unwrap();
        assert_eq!(out["scanned"], 2);
        assert_eq!(out["count"], 1);
        assert_eq!(out["channels"][0]["id"], "C2");
    }

    #[test]
    fn unreads_does_not_treat_missing_last_read_as_empty() {
        let mut h = host().with_http(
            "users.conversations",
            json!({ "ok": true, "channels": [{
                "id": "C1",
                "name": "dev",
                "latest": { "ts": "2.0" }
            }] }),
        );
        let out = plugin().call("slack.unreads", json!({}), &mut h).unwrap();
        assert_eq!(out["count"], 0);
        assert_eq!(out["skipped"][0]["id"], "C1");
        assert_eq!(out["skipped"][0]["reason"], "missing_last_read");
    }

    #[test]
    fn reaction_add_posts_name_and_timestamp() {
        let mut h = host().with_http("reactions.add", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.reaction.add",
                json!({ "channel": "C1", "ts": "1.1", "emoji": ":tada:" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn reaction_remove_posts() {
        let mut h = host().with_http("reactions.remove", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.reaction.remove",
                json!({ "channel": "C1", "ts": "1.1", "emoji": "tada" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn channel_list_calls_the_api_and_contributes_records() {
        let mut h = host().with_http(
            "conversations.list",
            json!({
                "ok": true,
                "channels": [{
                    "id": "C1",
                    "name": "dev-team",
                    "topic": { "value": "eng" },
                    "vendor_channel_field": { "retained": true }
                }],
                "response_metadata": { "next_cursor": "channel-next" },
                "vendor_envelope_field": "retained"
            }),
        );
        let out = plugin()
            .call("slack.channel.list", json!({}), &mut h)
            .unwrap();
        assert_eq!(out["channels"][0]["id"], "C1");
        assert_eq!(out["channels"][0]["vendor_channel_field"]["retained"], true);
        assert_eq!(out["response_metadata"]["next_cursor"], "channel-next");
        assert_eq!(out["vendor_envelope_field"], "retained");
        let recs = h.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "slack.channel");
        assert_eq!(recs[0].id, "C1");
        assert_eq!(recs[0].title, "dev-team");
        assert_eq!(recs[0].body, "eng");
        drop(recs);
        let calls = h.calls.borrow();
        let (_, request) = calls
            .iter()
            .find(|(command, _)| command == "http.do")
            .expect("channel-list HTTP request recorded");
        assert_eq!(request["auth_purpose"], "bot_token");
        assert_eq!(
            request["path"],
            "/conversations.list?types=public_channel,private_channel,mpim,im&limit=200"
        );
    }

    #[test]
    fn channel_join_posts() {
        let mut h = host().with_http(
            "conversations.join",
            json!({ "ok": true, "channel": { "id": "C1" } }),
        );
        let out = plugin()
            .call("slack.channel.join", json!({ "channel": "C1" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn channel_mark_posts() {
        let mut h = host().with_http("conversations.mark", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.channel.mark_read",
                json!({ "channel": "C1", "ts": "1.1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    /// D-127: the mrkdwn→Markdown fallthrough must copy CHARS, not bytes — the byte-wise walk
    /// mangled every multi-byte char into mojibake and panicked on the next `text[i..]` slice
    /// (`byte index … is not a char boundary; it is inside '—'`), killing the plugin process on
    /// any `message.list`/`thread`/`mentions` read of a channel containing an em-dash or emoji.
    #[test]
    fn mrkdwn_to_markdown_preserves_multibyte_chars() {
        // The live repro: an em-dash mid-sentence (panicked at byte 13, inside '—').
        assert_eq!(
            mrkdwn_to_markdown("flux 0.14.2 — the hardening release"),
            "flux 0.14.2 — the hardening release"
        );
        // Umlauts and emoji round-trip unmangled.
        assert_eq!(mrkdwn_to_markdown("größer 🚀 fertig"), "größer 🚀 fertig");
        // Conversion still applies around multi-byte chars in the same string.
        assert_eq!(
            mrkdwn_to_markdown("*bold* — <https://x.example|link>"),
            "**bold** — [link](https://x.example)"
        );
    }

    #[test]
    fn file_upload_reads_blob_and_runs_the_external_flow() {
        let mut h = host()
            .with_http(
                "files.getUploadURLExternal",
                json!({ "ok": true, "upload_url": "https://files.slack.test/up", "file_id": "F1" }),
            )
            .with_http("files.slack.test/up", json!({ "ok": true }))
            .with_http(
                "files.completeUploadExternal",
                json!({ "ok": true, "files": [{ "id": "F1", "title": "hello.txt" }] }),
            );
        // Stage non-UTF-8 source bytes directly into the host's blob store, then upload by ref —
        // the byte-exact `http_bytes` send must carry them verbatim (no `from_utf8_lossy`).
        let raw: Vec<u8> = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
        h.blobs
            .borrow_mut()
            .insert("blob-1".into(), ("hello.bin".into(), raw.clone()));
        let out = plugin()
            .call(
                "slack.file.upload",
                json!({ "channel": "C1", "blob_ref": "blob-1", "filename": "hello.bin" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["file_id"], "F1");
        assert_eq!(out["size"], raw.len());
        assert_eq!(out["files"][0]["id"], "F1");
        // The pre-signed-URL leg must POST: files.slack.com answers a PUT with a 302 redirect and
        // the upload never lands (D-128).
        let calls = h.calls.borrow();
        let (_, bytes_leg) = calls
            .iter()
            .find(|(cmd, payload)| {
                cmd == "http.do"
                    && payload["url"]
                        .as_str()
                        .is_some_and(|u| u.contains("files.slack.test/up"))
            })
            .expect("pre-signed upload call recorded");
        assert_eq!(bytes_leg["method"], "POST");
    }

    #[test]
    fn file_download_stages_bytes_into_a_blob_byte_exact() {
        // Non-UTF-8 bytes prove the binary path round-trips without `from_utf8_lossy` corruption.
        let raw: Vec<u8> = vec![0x00, 0x9f, 0x92, 0x96, 0xff];
        let mut h = host()
            .with_http(
                "files.info",
                json!({ "ok": true, "file": { "id": "F1", "name": "report.bin", "url_private_download": "https://files.slack.test/dl/F1" } }),
            )
            .with_http_bytes("files.slack.test/dl", raw.clone());
        let out = plugin()
            .call("slack.file.download", json!({ "file_id": "F1" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["filename"], "report.bin");
        assert_eq!(out["size"], raw.len());
        let blob_ref = out["blob_ref"].as_str().unwrap();
        let blobs = h.blobs.borrow();
        let (_, stored) = blobs.get(blob_ref).expect("blob staged");
        assert_eq!(stored, &raw, "downloaded bytes must round-trip byte-exact");
    }

    #[test]
    fn download_alias_works() {
        let mut h = host()
            .with_http(
                "files.info",
                json!({ "ok": true, "file": { "id": "F2", "name": "a.txt", "url_private": "https://files.slack.test/p/F2" } }),
            )
            .with_http_bytes("files.slack.test/p", b"bytes".to_vec());
        let out = plugin()
            .call("slack.download", json!({ "file_id": "F2" }), &mut h)
            .unwrap();
        assert_eq!(out["file_id"], "F2");
    }

    #[test]
    fn file_info_reads() {
        let mut h = host().with_http("files.info", json!({ "ok": true, "file": { "id": "F1" } }));
        let out = plugin()
            .call("slack.file.info", json!({ "file_id": "F1" }), &mut h)
            .unwrap();
        assert_eq!(out["file"]["id"], "F1");
    }

    #[test]
    fn file_list_reads() {
        let mut h = host().with_http(
            "files.list",
            json!({ "ok": true, "files": [{ "id": "F1" }] }),
        );
        let out = plugin()
            .call("slack.file.list", json!({ "channel": "C1" }), &mut h)
            .unwrap();
        assert_eq!(out["files"][0]["id"], "F1");
    }

    #[test]
    fn file_delete_posts() {
        let mut h = host().with_http("files.delete", json!({ "ok": true }));
        let out = plugin()
            .call("slack.file.delete", json!({ "file_id": "F1" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn bookmark_add_posts() {
        let mut h = host().with_http(
            "bookmarks.add",
            json!({ "ok": true, "bookmark": { "id": "Bk1" } }),
        );
        let out = plugin()
            .call(
                "slack.bookmark.add",
                json!({ "channel": "C1", "title": "Docs", "link": "https://x", "emoji": ":book:" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["bookmark"]["id"], "Bk1");
    }

    #[test]
    fn bookmark_edit_posts() {
        let mut h = host().with_http(
            "bookmarks.edit",
            json!({ "ok": true, "bookmark": { "id": "Bk1" } }),
        );
        let out = plugin()
            .call(
                "slack.bookmark.edit",
                json!({ "channel": "C1", "bookmark_id": "Bk1", "title": "New" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["bookmark"]["id"], "Bk1");
    }

    #[test]
    fn bookmark_delete_posts() {
        let mut h = host().with_http("bookmarks.remove", json!({ "ok": true }));
        let out = plugin()
            .call(
                "slack.bookmark.delete",
                json!({ "channel": "C1", "bookmark_id": "Bk1" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn bookmark_list_reads() {
        let mut h = host().with_http(
            "bookmarks.list",
            json!({ "ok": true, "bookmarks": [{ "id": "Bk1" }] }),
        );
        let out = plugin()
            .call("slack.bookmark.list", json!({ "channel": "C1" }), &mut h)
            .unwrap();
        assert_eq!(out["bookmarks"][0]["id"], "Bk1");
    }

    #[test]
    fn user_list_contributes_records() {
        let mut h = host().with_http(
            "users.list",
            json!({
                "ok": true,
                "members": [{
                    "id": "U1",
                    "name": "alice",
                    "profile": { "real_name": "Alice A" },
                    "vendor_user_field": ["kept"]
                }],
                "response_metadata": { "next_cursor": "user-next" },
                "vendor_envelope_field": { "retained": true }
            }),
        );
        let out = plugin().call("slack.user.list", json!({}), &mut h).unwrap();
        assert_eq!(out["members"][0]["id"], "U1");
        assert_eq!(out["members"][0]["vendor_user_field"], json!(["kept"]));
        assert_eq!(out["response_metadata"]["next_cursor"], "user-next");
        assert_eq!(out["vendor_envelope_field"]["retained"], true);
        let recs = h.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "slack.user");
        assert_eq!(recs[0].body, "Alice A");
        drop(recs);
        let calls = h.calls.borrow();
        let (_, request) = calls
            .iter()
            .find(|(command, _)| command == "http.do")
            .expect("user-list HTTP request recorded");
        assert_eq!(request["auth_purpose"], "bot_token");
        assert_eq!(request["path"], "/users.list?limit=200");
    }

    #[test]
    fn presence_get_reads() {
        let mut h = host().with_http(
            "users.getPresence",
            json!({ "ok": true, "presence": "active" }),
        );
        let out = plugin()
            .call("slack.presence.get", json!({ "user": "U1" }), &mut h)
            .unwrap();
        assert_eq!(out["presence"], "active");
    }

    #[test]
    fn presence_set_posts() {
        let mut h = host().with_http("users.setPresence", json!({ "ok": true }));
        let out = plugin()
            .call("slack.presence.set", json!({ "presence": "away" }), &mut h)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn emoji_list_reads() {
        let mut h = host().with_http(
            "emoji.list",
            json!({ "ok": true, "emoji": { "party": "https://x" } }),
        );
        let out = plugin()
            .call("slack.emoji.list", json!({}), &mut h)
            .unwrap();
        assert_eq!(out["emoji"]["party"], "https://x");
    }

    #[test]
    fn index_build_contributes_channels_and_users() {
        let mut h = host()
            .with_http(
                "conversations.list",
                json!({ "ok": true, "channels": [{ "id": "C1", "name": "dev" }] }),
            )
            .with_http(
                "users.list",
                json!({ "ok": true, "members": [{ "id": "U1", "name": "alice" }] }),
            );
        let out = plugin()
            .call("slack.index.build", json!({}), &mut h)
            .unwrap();
        assert_eq!(out["indexed"], 2);
        let recs = h.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().any(|r| r.entity == "slack.channel"));
        assert!(recs.iter().any(|r| r.entity == "slack.user"));
    }

    #[test]
    fn falsey_ok_surfaces_the_error() {
        let mut h = host().with_http(
            "conversations.history",
            json!({ "ok": false, "error": "channel_not_found" }),
        );
        let err = plugin()
            .call("slack.message.list", json!({ "channel": "C9" }), &mut h)
            .unwrap_err();
        assert!(err.contains("channel_not_found"), "got: {err}");
    }

    #[test]
    fn message_send_blocks_requires_no_text_fails_without_fallback() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "1.0" }));
        let err = plugin()
            .call(
                "slack.message.send",
                json!({
                    "channel": "C1",
                    "blocks": [{ "type": "divider" }]
                }),
                &mut h,
            )
            .unwrap_err();
        assert!(err.contains("text fallback"), "got: {err}");
    }

    #[test]
    fn message_send_blocks_and_text_posts_blocks() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "1.0" }));
        let out = plugin()
            .call(
                "slack.message.send",
                json!({
                    "channel": "C1",
                    "text": "fallback",
                    "blocks": [{ "type": "divider" }],
                    "unfurl_links": false,
                    "unfurl_media": false,
                    "parse": "none"
                }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ts"], "1.0");
    }

    #[test]
    fn message_send_markdown_posts_mrkdwn_block() {
        let mut h = host().with_http("chat.postMessage", json!({ "ok": true, "ts": "1.0" }));
        let out = plugin()
            .call(
                "slack.message.send",
                json!({"channel": "C1", "markdown": "hello *world*" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ts"], "1.0");
    }

    #[test]
    fn message_edit_blocks_without_text_is_rejected() {
        let mut h = host().with_http("chat.update", json!({ "ok": true, "ts": "1.0" }));
        let err = plugin()
            .call(
                "slack.message.edit",
                json!({ "ref": "C1:1.0", "blocks": [{ "type": "divider" }] }),
                &mut h,
            )
            .unwrap_err();
        assert!(err.contains("text fallback"), "got: {err}");
    }

    #[test]
    fn message_list_text_format_mrkdwn_keeps_raw() {
        let mut h = host().with_http(
            "conversations.history",
            json!({ "ok": true, "messages": [{ "ts": "1.1", "text": "<https://x|link> plain" }] }),
        );
        let out = plugin()
            .call(
                "slack.message.list",
                json!({ "channel": "C1", "text_format": "mrkdwn" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"][0]["text"], "<https://x|link> plain");
        assert!(out["messages"][0]["text_mrkdwn"].is_null());
    }

    #[test]
    fn thread_text_format_default_renders_markdown() {
        let mut h = host().with_http(
            "conversations.replies",
            json!({ "ok": true, "messages": [{ "ts": "1.1", "text": "<https://x|link>" }] }),
        );
        let out = plugin()
            .call(
                "slack.thread",
                json!({ "channel": "C1", "ts": "1.0" }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["messages"][0]["text"], "[link](https://x)");
    }

    #[test]
    fn search_extracts_tickets() {
        let mut h = host().with_http(
            "search.messages",
            json!({
                "ok": true,
                "messages": {
                    "total": 1,
                    "matches": [{
                        "text": "PROJ-123 is fixed",
                        "permalink": "https://acme.slack.com/archives/C1/p1"
                    }]
                }
            }),
        );
        let out = plugin()
            .call(
                "slack.search",
                json!({ "query": "PROJ", "tickets": true, "ticket_keys": ["PROJ"] }),
                &mut h,
            )
            .unwrap();
        assert_eq!(
            out["messages"]["matches"][0]["tickets"],
            json!(["PROJ-123"])
        );
        let tickets = out["tickets"].as_array().unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0]["key"], "PROJ-123");
        assert_eq!(tickets[0]["mentions"], 1);
    }

    #[test]
    fn mentions_uses_bot_identity_when_requested() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "<@U_bot> look",
                    "permalink": "https://acme.slack.com/archives/C1/p1001000000",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_bot" }));
        let out = plugin()
            .call("slack.mentions", json!({ "bot": true }), &mut h)
            .unwrap();
        assert_eq!(out["target"], "U_bot");
        assert_eq!(out["count"], 1);
    }

    #[test]
    fn mentions_ticket_keys_are_strings() {
        let now = unix_now();
        let matched = format!("{now}.000001");
        let mut h = host()
            .with_http(
                "search.messages",
                json!({ "ok": true, "messages": { "total": 1, "matches": [{
                    "ts": matched,
                    "user": "U2",
                    "text": "dev-5 ABC-9",
                    "permalink": "https://x",
                    "channel": { "id": "" }
                }] } }),
            )
            .with_http("auth.test", json!({ "ok": true, "user_id": "U_me" }));
        let out = plugin()
            .call(
                "slack.mentions",
                json!({ "tickets": true, "ticket_keys": ["DEV", "abc"] }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["mentions"][0]["tickets"], json!(["ABC-9", "DEV-5"]));
    }

    #[test]
    fn file_upload_content_bytes_decodes_inline_base64() {
        let mut h = host()
            .with_http(
                "files.getUploadURLExternal",
                json!({ "ok": true, "upload_url": "https://files.slack.test/up", "file_id": "F1" }),
            )
            .with_http("files.slack.test/up", json!({ "ok": true }))
            .with_http(
                "files.completeUploadExternal",
                json!({ "ok": true, "files": [{ "id": "F1", "title": "hello.txt" }] }),
            );
        let content = b"hello bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let out = plugin()
            .call(
                "slack.file.upload",
                json!({
                    "channel": "C1",
                    "content_bytes": b64,
                    "filename": "hello.txt",
                    "alt_text": "chart"
                }),
                &mut h,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["filename"], "hello.txt");
        assert_eq!(out["size"], content.len());
    }

    #[test]
    fn file_upload_requires_exactly_one_content_source() {
        let mut h = host().with_http("files.getUploadURLExternal", json!({ "ok": false }));
        let err = plugin()
            .call(
                "slack.file.upload",
                json!({
                    "channel": "C1",
                    "blob_ref": "blob-1",
                    "content_bytes": "aGVsbG8="
                }),
                &mut h,
            )
            .unwrap_err();
        assert!(
            err.contains("exactly one of blob_ref or content_bytes"),
            "got: {err}"
        );
    }

    #[test]
    fn file_download_blob_ref_seed_returns_prefixed_ref() {
        let mut h = host()
            .with_http(
                "files.info",
                json!({ "ok": true, "file": { "id": "F1", "name": "a.txt", "url_private_download": "https://files.slack.test/dl" } }),
            )
            .with_http_bytes("files.slack.test/dl", b"data".to_vec());
        let out = plugin()
            .call(
                "slack.file.download",
                json!({ "file_id": "F1", "blob_ref": "myprefix" }),
                &mut h,
            )
            .unwrap();
        let blob_ref = out["blob_ref"].as_str().unwrap();
        assert!(blob_ref.starts_with("myprefix"), "got: {blob_ref}");
    }

    #[test]
    fn file_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "files.list",
            json!({
                "ok": true,
                "files": [
                    { "id": "F1", "name": "foo.txt" },
                    { "id": "F2", "name": "bar.txt" },
                    { "id": "F3", "name": "fooagain.txt" }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.file.list",
                json!({ "query": "foo", "limit": 2 }),
                &mut h,
            )
            .unwrap();
        let files = out["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .all(|f| f["name"].as_str().unwrap().contains("foo")));
    }

    #[test]
    fn channel_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "conversations.list",
            json!({
                "ok": true,
                "channels": [
                    { "id": "C1", "name": "team-alpha" },
                    { "id": "C2", "name": "team-beta" },
                    { "id": "C3", "name": "alpha-2" }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.channel.list",
                json!({ "query": "alpha", "limit": 1 }),
                &mut h,
            )
            .unwrap();
        let channels = out["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 1);
        assert!(channels[0]["name"].as_str().unwrap().contains("alpha"));
        assert_eq!(
            h.contributed.borrow().len(),
            3,
            "datasource contribution uses the unfiltered vendor response"
        );
    }

    #[test]
    fn user_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "users.list",
            json!({
                "ok": true,
                "members": [
                    { "id": "U1", "name": "alice", "profile": { "real_name": "Alice A" } },
                    { "id": "U2", "name": "bob", "profile": { "real_name": "Bob B" } },
                    { "id": "U3", "name": "alicia", "profile": { "real_name": "Alicia C" } }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.user.list",
                json!({ "query": "ali", "limit": 2 }),
                &mut h,
            )
            .unwrap();
        let members = out["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(
            h.contributed.borrow().len(),
            3,
            "datasource contribution uses the unfiltered vendor response"
        );
    }

    #[test]
    fn bookmark_list_filters_and_limits_client_side() {
        let mut h = host().with_http(
            "bookmarks.list",
            json!({
                "ok": true,
                "bookmarks": [
                    { "id": "B1", "title": "Alpha docs", "link": "https://a" },
                    { "id": "B2", "title": "Beta docs", "link": "https://b" },
                    { "id": "B3", "title": "Gamma runbook", "link": "https://g" }
                ]
            }),
        );
        let out = plugin()
            .call(
                "slack.bookmark.list",
                json!({ "channel": "C1", "query": "docs", "limit": 1 }),
                &mut h,
            )
            .unwrap();
        let bookmarks = out["bookmarks"].as_array().unwrap();
        assert_eq!(bookmarks.len(), 1);
        assert!(bookmarks[0]["title"].as_str().unwrap().contains("docs"));
    }

    #[test]
    fn emoji_list_custom_mode_and_query_and_limit() {
        let mut h = host().with_http(
            "emoji.list",
            json!({
                "ok": true,
                "emoji": {
                    "party": "https://x",
                    "partyparrot": "alias:party",
                    "work": "https://y"
                }
            }),
        );
        let out = plugin()
            .call(
                "slack.emoji.list",
                json!({ "query": "party", "limit": 1 }),
                &mut h,
            )
            .unwrap();
        let emoji = out["emoji"].as_array().unwrap();
        assert_eq!(emoji.len(), 1);
        assert_eq!(emoji[0]["name"], "party");
    }

    #[test]
    fn emoji_list_include_aliases_shows_alias_entry() {
        let mut h = host().with_http(
            "emoji.list",
            json!({
                "ok": true,
                "emoji": {
                    "party": "https://x",
                    "partyparrot": "alias:party"
                }
            }),
        );
        let out = plugin()
            .call(
                "slack.emoji.list",
                json!({ "include_aliases": true }),
                &mut h,
            )
            .unwrap();
        let emoji = out["emoji"].as_array().unwrap();
        assert!(emoji
            .iter()
            .any(|e| e["name"] == "partyparrot" && e["alias_for"] == "party"));
    }

    #[test]
    fn manifest_declares_ops_auth_and_datasources() {
        let m = plugin().manifest();
        assert_eq!(m.operations.iter().filter(|o| !o.internal).count(), 30);
        assert_eq!(m.auth[0].purpose, "bot_token");
        assert!(m.auth.iter().any(|a| a.purpose == "user_token"));
        assert!(m.capabilities.blob);
        assert!(m.datasources.iter().any(|d| d.entity == "slack.channel"));
        assert!(m.datasources.iter().any(|d| d.entity == "slack.user"));
        assert!(m
            .datasources
            .iter()
            .all(|d| d.capabilities.iter().any(|c| c == "index")));
    }

    #[test]
    fn bounded_read_families_publish_closed_inputs_and_typed_output_envelopes() {
        let manifest = plugin().manifest();
        let contract = |operation: &str| {
            manifest
                .operations
                .iter()
                .find(|spec| spec.name == operation)
                .unwrap_or_else(|| panic!("missing operation {operation}"))
        };
        let channel = contract("slack.channel.list");
        assert_eq!(channel.input_schema, op_input_schema::<ChannelListInput>());
        assert_eq!(
            channel.output_schema.as_ref(),
            Some(&op_output_schema::<ChannelListOutput>())
        );
        let user = contract("slack.user.list");
        assert_eq!(user.input_schema, op_input_schema::<UserListInput>());
        assert_eq!(
            user.output_schema.as_ref(),
            Some(&op_output_schema::<UserListOutput>())
        );
        let messages = contract("slack.message.list");
        assert_eq!(messages.input_schema, op_input_schema::<MessageListInput>());
        assert_eq!(
            messages.output_schema.as_ref(),
            Some(&op_output_schema::<MessageListOutput>())
        );
        let thread = contract("slack.thread");
        assert_eq!(thread.input_schema, op_input_schema::<ThreadInput>());
        assert_eq!(
            thread.output_schema.as_ref(),
            Some(&op_output_schema::<ThreadOutput>())
        );

        for (operation, object_schema, stable_field) in [
            ("slack.channel.list", "SlackChannelSchema", "id"),
            ("slack.user.list", "SlackUserSchema", "name"),
            ("slack.message.list", "SlackMessageSchema", "ts"),
            ("slack.thread", "SlackMessageSchema", "thread_ts"),
        ] {
            let output = contract(operation).output_schema.as_ref().unwrap();
            assert_eq!(
                output["additionalProperties"], true,
                "{operation}: top-level Slack metadata remains open"
            );
            let vendor_object = &output["$defs"][object_schema];
            assert_eq!(vendor_object["type"], "object");
            assert!(
                vendor_object["properties"][stable_field].is_object(),
                "{operation}: missing stable `{stable_field}` in {vendor_object}"
            );
            assert_eq!(
                vendor_object["additionalProperties"], true,
                "{operation}: Slack-owned object extensions remain open"
            );
        }

        for (operation, collection) in [
            ("slack.channel.list", "channels"),
            ("slack.user.list", "members"),
            ("slack.message.list", "messages"),
            ("slack.thread", "messages"),
        ] {
            let spec = contract(operation);
            assert_eq!(
                spec.input_schema["additionalProperties"],
                json!(false),
                "{operation}: typed input must reject contract drift"
            );
            let output = spec
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{operation}: missing generated output schema"));
            assert_eq!(output["type"], "object", "{operation}: output root");
            assert!(
                output["properties"].get(collection).is_some(),
                "{operation}: output must declare `{collection}`"
            );
            assert_eq!(
                output["properties"][collection]["type"], "array",
                "{operation}: `{collection}` remains an open vendor-object list"
            );
        }
    }

    #[test]
    fn bounded_read_input_drift_fails_before_http_with_field_context() {
        let mut h = host();
        let unknown = plugin()
            .call(
                "slack.channel.list",
                json!({ "unexpected_filter": true }),
                &mut h,
            )
            .unwrap_err();
        assert!(
            unknown.contains("unexpected_filter") || unknown.contains("unknown field"),
            "unexpected error: {unknown}"
        );

        let wrong_type = plugin()
            .call(
                "slack.message.list",
                json!({ "channel": "C1", "limit": "ten" }),
                &mut h,
            )
            .unwrap_err();
        assert!(
            wrong_type.contains("limit"),
            "unexpected error: {wrong_type}"
        );
        assert!(
            h.calls.borrow().is_empty(),
            "input drift must fail before a host capability call"
        );
    }

    /// C-52: the mark-read op was the only hyphenated op name in the whole plugin pack; every
    /// multi-word leaf segment now uses an underscore. Assert the renamed op is advertised and that
    /// no advertised op name carries a hyphen (guards the convention against reintroduction).
    #[test]
    fn op_names_use_underscores_not_hyphens() {
        let m = plugin().manifest();
        assert!(
            m.operations
                .iter()
                .any(|o| o.name == "slack.channel.mark_read"),
            "expected the renamed `slack.channel.mark_read` op"
        );
        assert!(
            !m.operations
                .iter()
                .any(|o| o.name == "slack.channel.mark-read"),
            "the hyphenated `slack.channel.mark-read` op must be gone"
        );
        let hyphenated: Vec<&str> = m
            .operations
            .iter()
            .map(|o| o.name.as_str())
            .filter(|name| name.contains('-'))
            .collect();
        assert!(
            hyphenated.is_empty(),
            "op names must use underscores, not hyphens: {hyphenated:?}"
        );
    }
}

// ===========================================================================
// D-36: schema-derivation contract test (slack).
// Each op's `input_schema` is schemars-derived (`read_op_typed::<T>` /
// `write_op_typed::<T>`) instead of an inline `json!({"type":"object",...})`
// literal. Asserts the derived schema's fields/required/base-types match the
// legacy inline contract (transcribed pre-migration). A change here is a real
// contract change.
// ===========================================================================
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        ArrayAny,
        ArrayStr,
    }
    #[derive(Clone)]
    struct Prop {
        name: &'static str,
        kind: Kind,
    }
    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }
    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }
    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }
    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            ("slack.test", c(vec![], vec![])),
            ("slack.info", c(vec![], vec![])),
            (
                "slack.message.send",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("text", Kind::Str),
                        p("markdown", Kind::Str),
                        p("blocks", Kind::ArrayAny),
                        p("thread_ts", Kind::Str),
                        p("reply_broadcast", Kind::Bool),
                        p("unfurl_links", Kind::Bool),
                        p("unfurl_media", Kind::Bool),
                        p("parse", Kind::Str),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.message.list",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("limit", Kind::Int),
                        p("cursor", Kind::Str),
                        p("oldest", Kind::Str),
                        p("latest", Kind::Str),
                        p("text_format", Kind::Str),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.message.edit",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("text", Kind::Str),
                        p("markdown", Kind::Str),
                        p("blocks", Kind::ArrayAny),
                        p("unfurl_links", Kind::Bool),
                        p("unfurl_media", Kind::Bool),
                        p("parse", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.message.delete",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.thread",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("limit", Kind::Int),
                        p("max_bytes", Kind::Int),
                        p("text_format", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.search",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                        p("tickets", Kind::Bool),
                        p("ticket_keys", Kind::ArrayStr),
                    ],
                    vec!["query"],
                ),
            ),
            (
                "slack.mentions",
                c(
                    vec![
                        p("user", Kind::Str),
                        p("bot", Kind::Bool),
                        p("since", Kind::Str),
                        p("limit", Kind::Int),
                        p("unhandled", Kind::Bool),
                        p("max_thread", Kind::Int),
                        p("tickets", Kind::Bool),
                        p("ticket_keys", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.unreads",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("since", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.reaction.add",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["emoji"],
                ),
            ),
            (
                "slack.reaction.remove",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["emoji"],
                ),
            ),
            (
                "slack.channel.list",
                c(vec![p("query", Kind::Str), p("limit", Kind::Int)], vec![]),
            ),
            (
                "slack.channel.join",
                c(vec![p("channel", Kind::Str)], vec!["channel"]),
            ),
            (
                "slack.channel.mark_read",
                c(
                    vec![
                        p("ref", Kind::Str),
                        p("channel", Kind::Str),
                        p("ts", Kind::Str),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.file.upload",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("blob_ref", Kind::Str),
                        p("content_bytes", Kind::Str),
                        p("filename", Kind::Str),
                        p("thread_ts", Kind::Str),
                        p("initial_comment", Kind::Str),
                        p("alt_text", Kind::Str),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.file.download",
                c(
                    vec![
                        p("file_id", Kind::Str),
                        p("blob_ref", Kind::Str),
                        p("filename", Kind::Str),
                    ],
                    vec!["file_id"],
                ),
            ),
            (
                "slack.download",
                c(
                    vec![
                        p("file_id", Kind::Str),
                        p("blob_ref", Kind::Str),
                        p("filename", Kind::Str),
                    ],
                    vec!["file_id"],
                ),
            ),
            (
                "slack.file.info",
                c(vec![p("file_id", Kind::Str)], vec!["file_id"]),
            ),
            (
                "slack.file.list",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("user", Kind::Str),
                        p("types", Kind::Str),
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "slack.file.delete",
                c(vec![p("file_id", Kind::Str)], vec!["file_id"]),
            ),
            (
                "slack.bookmark.add",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("title", Kind::Str),
                        p("link", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["channel", "title", "link"],
                ),
            ),
            (
                "slack.bookmark.edit",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("bookmark_id", Kind::Str),
                        p("title", Kind::Str),
                        p("link", Kind::Str),
                        p("emoji", Kind::Str),
                    ],
                    vec!["channel", "bookmark_id"],
                ),
            ),
            (
                "slack.bookmark.delete",
                c(
                    vec![p("channel", Kind::Str), p("bookmark_id", Kind::Str)],
                    vec!["channel", "bookmark_id"],
                ),
            ),
            (
                "slack.bookmark.list",
                c(
                    vec![
                        p("channel", Kind::Str),
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec!["channel"],
                ),
            ),
            (
                "slack.user.list",
                c(vec![p("query", Kind::Str), p("limit", Kind::Int)], vec![]),
            ),
            ("slack.presence.get", c(vec![p("user", Kind::Str)], vec![])),
            (
                "slack.presence.set",
                c(vec![p("presence", Kind::Str)], vec!["presence"]),
            ),
            (
                "slack.emoji.list",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                        p("mode", Kind::Str),
                        p("include_aliases", Kind::Bool),
                    ],
                    vec![],
                ),
            ),
            ("slack.index.build", c(vec![], vec![])),
        ]
    }
    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }
    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "array" => {
                if node
                    .get("items")
                    .and_then(|v| v.get("type"))
                    .and_then(|v| v.as_str())
                    == Some("string")
                {
                    Kind::ArrayStr
                } else {
                    Kind::ArrayAny
                }
            }
            "string" => Kind::Str,
            other => panic!("unsupported property type: {other}"),
        }
    }
    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        assert_eq!(schema["type"], "object", "{op_name}: root type");
        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                got.insert(k.as_str(), kind_of(v));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got
                .get(*name)
                .unwrap_or_else(|| panic!("{op_name}: missing property `{name}`"));
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }
        let req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut req_set: Vec<&str> = req.clone();
        req_set.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req_set, want_req, "{op_name}: required set");
    }
    #[test]
    fn derived_schemas_match_legacy_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .filter(|o| !o.internal)
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}
