//! File upload, download, listing, and deletion operations through host blobs.

use super::*;

// ---------------------------------------------------------------------------
// files (host blobs)
// ---------------------------------------------------------------------------

pub(crate) fn file_upload(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?.to_string();

    // Bytes come from either an inline base64 payload or a host blob_ref (exactly one).
    let has_blob_ref = opt_str(&input, "blob_ref").is_some();
    let has_content_bytes = opt_str(&input, "content_bytes").is_some();
    if has_blob_ref == has_content_bytes {
        return Err("provide exactly one of blob_ref or content_bytes".into());
    }
    let bytes = if let Some(b64) = opt_str(&input, "content_bytes") {
        base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| format!("content_bytes is not valid base64: {e}"))?
    } else {
        let blob_ref = req_str(&input, "blob_ref")?.to_string();
        host.blob_get(&blob_ref)?
    };

    let filename = opt_str(&input, "filename")
        .map(str::to_string)
        .unwrap_or_else(|| "upload.bin".into());
    if bytes.is_empty() {
        return Err("file content is empty".into());
    }

    // 1. Reserve an external upload URL. Alt text rides here as `alt_txt` — it is a
    //    getUploadURLExternal parameter; completeUploadExternal's `files` entries accept only
    //    `id`/`title` and answer anything else with `invalid_arguments` (D-128).
    let mut reserve_path = format!(
        "/files.getUploadURLExternal?filename={}&length={}",
        urlencode(&filename),
        bytes.len(),
    );
    if let Some(alt) = opt_str(&input, "alt_text") {
        reserve_path.push_str(&format!("&alt_txt={}", urlencode(alt)));
    }
    let reserved = check_ok(sl_get(host, &reserve_path, Some("bot_token"))?)?;
    let upload_url = reserved
        .get("upload_url")
        .and_then(|v| v.as_str())
        .ok_or("files.getUploadURLExternal returned no upload_url")?
        .to_string();
    let file_id = reserved
        .get("file_id")
        .and_then(|v| v.as_str())
        .ok_or("files.getUploadURLExternal returned no file_id")?
        .to_string();

    // 2. Send the bytes to the pre-signed URL byte-exact (no auth; the URL carries its own token).
    //    `http_bytes` ships the raw body so binary files round-trip without UTF-8 corruption.
    //    POST, per the files.getUploadURLExternal contract — files.slack.com answers a PUT with a
    //    302 redirect and the upload never lands (D-128).
    let resp = host.http_bytes("POST", &upload_url, None, &[], Some(&bytes), false)?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "slack file upload → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }

    // 3. Complete the upload, attaching the file to the channel/thread.
    let file_entry = json!({ "id": file_id, "title": filename });
    let mut complete = json!({
        "files": [file_entry],
        "channel_id": channel,
    });
    if let Some(ts) = opt_str(&input, "thread_ts") {
        complete["thread_ts"] = json!(ts);
    }
    if let Some(comment) = opt_str(&input, "initial_comment") {
        complete["initial_comment"] = json!(comment);
    }
    let done = check_ok(sl_send(
        host,
        "POST",
        "/files.completeUploadExternal",
        Some("bot_token"),
        &complete,
    )?)?;
    Ok(json!({
        "ok": true,
        "channel": channel,
        "file_id": file_id,
        "filename": filename,
        "size": bytes.len(),
        "files": done.get("files").cloned().unwrap_or_else(|| json!([])),
    }))
}

pub(crate) fn file_download(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?.to_string();
    let info_path = format!("/files.info?file={}", urlencode(&file_id));
    let info = check_ok(sl_get(host, &info_path, Some("bot_token"))?)?;
    let file = info.get("file").cloned().unwrap_or(Value::Null);
    let download_url = file
        .get("url_private_download")
        .or_else(|| file.get("url_private"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("file has no private download URL")?
        .to_string();
    // Fetch byte-exact: `binary_response = true` returns the raw bytes so non-UTF-8 files
    // round-trip without corruption. The download URL still needs the bot token as bearer auth.
    let resp = host.http_bytes("GET", &download_url, Some("bot_token"), &[], None, true)?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "slack download → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }
    let bytes = resp.bytes;
    let filename = opt_str(&input, "filename")
        .map(str::to_string)
        .or_else(|| {
            file.get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| file_id.clone());
    // If the caller provided a blob_ref seed, use it as the returned reference (the host's
    // blob store receives the content under that name). Mirrors fluxplane's BlobWrite.Ref.
    let blob_ref = if let Some(seed) = opt_str(&input, "blob_ref") {
        host.blob_put(seed, &bytes)?;
        seed.to_string()
    } else {
        host.blob_put(&filename, &bytes)?
    };
    Ok(json!({
        "ok": true,
        "file_id": file_id,
        "filename": filename,
        "size": bytes.len(),
        "blob_ref": blob_ref,
        "file": file,
    }))
}

pub(crate) fn file_info(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?;
    let path = format!("/files.info?file={}", urlencode(file_id));
    check_ok(sl_get(host, &path, Some("bot_token"))?)
}

pub(crate) fn file_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    let mut path = format!("/files.list?count={limit}&page=1");
    for key in ["channel", "user", "types"] {
        if let Some(val) = opt_str(&input, key) {
            path.push_str(&format!("&{key}={}", urlencode(val)));
        }
    }
    let query = opt_str(&input, "query")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut v = check_ok(sl_get(host, &path, Some("bot_token"))?)?;
    if let Some(files) = v.get_mut("files").and_then(|f| f.as_array_mut()) {
        if !query.is_empty() {
            files.retain(|f| file_matches_query(f, &query));
        }
        let cap = input
            .get("limit")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0);
        if let Some(n) = cap {
            if files.len() > n as usize {
                files.truncate(n as usize);
            }
        }
    }
    Ok(v)
}

pub(crate) fn file_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let file_id = req_str(&input, "file_id")?;
    check_ok(sl_send(
        host,
        "POST",
        "/files.delete",
        Some("bot_token"),
        &json!({ "file": file_id }),
    )?)
}
