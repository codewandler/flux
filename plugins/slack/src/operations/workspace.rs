//! Reactions, channels, bookmarks, users, presence, emoji, and indexing operations.

use super::*;

// ---------------------------------------------------------------------------
// reactions
// ---------------------------------------------------------------------------

pub(crate) fn reaction_add(input: Value, host: &mut Host) -> Result<Value, String> {
    reaction(input, host, "reactions.add")
}

pub(crate) fn reaction_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    reaction(input, host, "reactions.remove")
}

pub(crate) fn reaction(input: Value, host: &mut Host, method: &str) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    let emoji = req_str(&input, "emoji")?.trim_matches(':');
    let body = json!({ "channel": channel, "timestamp": ts, "name": emoji });
    check_ok(sl_send(
        host,
        "POST",
        &format!("/{method}"),
        Some("bot_token"),
        &body,
    )?)
}

// ---------------------------------------------------------------------------
// channels
// ---------------------------------------------------------------------------

pub(crate) fn channel_list(
    input: ChannelListInput,
    host: &mut Host,
) -> Result<ChannelListOutput, String> {
    let v = check_ok(sl_get(
        host,
        "/conversations.list?types=public_channel,private_channel,mpim,im&limit=200",
        Some("bot_token"),
    )?)?;
    // Index the complete vendor response before applying caller-local presentation filters.
    contribute_channels(host, &v);
    let query = input
        .query
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let limit = input.limit.filter(|n| *n > 0);
    let mut output: ChannelListOutput = decode_response("slack.channel.list", v)?;
    if !query.is_empty() {
        output
            .channels
            .retain(|channel| channel_matches_query(&channel.0, &query));
    }
    if let Some(n) = limit {
        if output.channels.len() > n as usize {
            output.channels.truncate(n as usize);
        }
    }
    Ok(output)
}

pub(crate) fn channel_join(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    check_ok(sl_send(
        host,
        "POST",
        "/conversations.join",
        Some("bot_token"),
        &json!({ "channel": channel }),
    )?)
}

pub(crate) fn channel_mark(input: Value, host: &mut Host) -> Result<Value, String> {
    let (channel, ts) = resolve_ref(&input)?;
    check_ok(sl_send(
        host,
        "POST",
        "/conversations.mark",
        Some("bot_token"),
        &json!({ "channel": channel, "ts": ts }),
    )?)
}

// ---------------------------------------------------------------------------
// bookmarks
// ---------------------------------------------------------------------------

pub(crate) fn bookmark_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let title = req_str(&input, "title")?;
    let link = req_str(&input, "link")?;
    let mut body = json!({ "channel_id": channel, "title": title, "type": "link", "link": link });
    if let Some(emoji) = opt_str(&input, "emoji") {
        body["emoji"] = json!(emoji.trim_matches(':'));
    }
    check_ok(sl_send(
        host,
        "POST",
        "/bookmarks.add",
        Some("bot_token"),
        &body,
    )?)
}

pub(crate) fn bookmark_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let bookmark_id = req_str(&input, "bookmark_id")?;
    let mut body = json!({ "channel_id": channel, "bookmark_id": bookmark_id });
    if let Some(title) = opt_str(&input, "title") {
        body["title"] = json!(title);
    }
    if let Some(link) = opt_str(&input, "link") {
        body["link"] = json!(link);
    }
    if let Some(emoji) = opt_str(&input, "emoji") {
        body["emoji"] = json!(emoji.trim_matches(':'));
    }
    check_ok(sl_send(
        host,
        "POST",
        "/bookmarks.edit",
        Some("bot_token"),
        &body,
    )?)
}

pub(crate) fn bookmark_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let bookmark_id = req_str(&input, "bookmark_id")?;
    let body = json!({ "channel_id": channel, "bookmark_id": bookmark_id });
    check_ok(sl_send(
        host,
        "POST",
        "/bookmarks.remove",
        Some("bot_token"),
        &body,
    )?)
}

pub(crate) fn bookmark_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let path = format!("/bookmarks.list?channel_id={}", urlencode(channel));
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0);
    let mut v = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    if let Some(bookmarks) = v.get_mut("bookmarks").and_then(|b| b.as_array_mut()) {
        if !query.is_empty() {
            bookmarks.retain(|b| bookmark_matches_query(b, &query));
        }
        if let Some(n) = limit {
            if bookmarks.len() > n as usize {
                bookmarks.truncate(n as usize);
            }
        }
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// users / presence / emoji
// ---------------------------------------------------------------------------

pub(crate) fn user_list(input: UserListInput, host: &mut Host) -> Result<UserListOutput, String> {
    let v = check_ok(sl_get(host, "/users.list?limit=200", Some("bot_token"))?)?;
    // Index the complete vendor response before applying caller-local presentation filters.
    contribute_users(host, &v);
    let query = input
        .query
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let limit = input.limit.filter(|n| *n > 0);
    let mut output: UserListOutput = decode_response("slack.user.list", v)?;
    if !query.is_empty() {
        output
            .members
            .retain(|user| user_matches_query(&user.0, &query));
    }
    if let Some(n) = limit {
        if output.members.len() > n as usize {
            output.members.truncate(n as usize);
        }
    }
    Ok(output)
}

pub(crate) fn presence_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut path = String::from("/users.getPresence");
    if let Some(user) = opt_str(&input, "user") {
        path.push_str(&format!("?user={}", urlencode(user)));
    }
    check_ok(sl_get(host, &path, Some("bot_token"))?)
}

pub(crate) fn presence_set(input: Value, host: &mut Host) -> Result<Value, String> {
    let presence = req_str(&input, "presence")?;
    check_ok(sl_send(
        host,
        "POST",
        "/users.setPresence",
        Some("user_token"),
        &json!({ "presence": presence }),
    )?)
}

pub(crate) fn emoji_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mode = opt_str(&input, "mode")
        .unwrap_or("custom")
        .trim()
        .to_ascii_lowercase();
    if mode != "custom" && mode != "builtin" && mode != "all" {
        return Err("mode must be custom, builtin, or all".into());
    }
    let include_aliases = input
        .get("include_aliases")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0);
    let unfiltered = query.is_empty() && limit.is_none() && !include_aliases && mode == "custom";

    let mut v = check_ok(sl_get(host, "/emoji.list", Some("bot_token"))?)?;
    if unfiltered {
        // No client-side filtering requested: keep Slack's native emoji map shape.
        return Ok(v);
    }

    let mut out: Vec<Value> = Vec::new();

    if mode == "custom" || mode == "all" {
        if let Some(emoji) = v.get("emoji").and_then(|e| e.as_object()) {
            let mut names: Vec<&String> = emoji.keys().collect();
            names.sort();
            for name in names {
                let value = emoji
                    .get(name)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let (is_alias, alias_for) = value
                    .strip_prefix("alias:")
                    .map(|rest| (true, rest.trim()))
                    .unwrap_or((false, ""));
                if is_alias && !include_aliases {
                    continue;
                }
                if name.to_ascii_lowercase().contains(&query) {
                    let mut entry = json!({ "name": name, "source": "custom" });
                    if is_alias {
                        entry["alias_for"] = json!(alias_for);
                    } else if !value.is_empty() {
                        entry["url"] = json!(value);
                    }
                    out.push(entry);
                }
            }
        }
    }

    if mode == "builtin" || mode == "all" {
        if let Some(categories) = v.get("categories").and_then(|c| c.as_array()) {
            for category in categories {
                let category_name = category
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if let Some(names) = category.get("emoji_names").and_then(|v| v.as_array()) {
                    for name in names {
                        let name = name
                            .as_str()
                            .map(str::trim)
                            .map(|s| s.trim_matches(':'))
                            .unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        if name.to_ascii_lowercase().contains(&query) {
                            out.push(json!({
                                "name": name,
                                "source": "builtin",
                                "category": category_name.clone(),
                            }));
                        }
                    }
                }
            }
            if mode == "all" {
                // Sort combined custom+builtin by name for stable output.
                out.sort_by(|a, b| {
                    let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    a_name.cmp(b_name)
                });
            }
        }
    }

    if let Some(n) = limit {
        if out.len() > n as usize {
            out.truncate(n as usize);
        }
    }

    v["emoji"] = json!(out);
    Ok(v)
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

pub(crate) fn index_build(_input: Value, host: &mut Host) -> Result<Value, String> {
    let mut total = 0usize;
    let channels = check_ok(sl_get(
        host,
        "/conversations.list?types=public_channel,private_channel,mpim,im&limit=200",
        Some("bot_token"),
    )?)?;
    total += contribute_channels(host, &channels);

    let users = check_ok(sl_get(host, "/users.list?limit=200", Some("bot_token"))?)?;
    total += contribute_users(host, &users);

    Ok(json!({ "indexed": total }))
}
