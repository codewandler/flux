//! Attachment upload, download, listing, and deletion through host blobs.

use super::*;

pub(crate) fn attachment_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let blob_ref = opt_str(&input, "blob_ref").trim();
    let content_bytes_b64 = opt_str(&input, "content_bytes").trim();
    let has_blob = !blob_ref.is_empty();
    let has_bytes = !content_bytes_b64.is_empty();
    if has_blob == has_bytes {
        return Err("provide exactly one of blob_ref or content_bytes".into());
    }
    let bytes = if has_blob {
        host.blob_get(blob_ref)?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(content_bytes_b64)
            .map_err(|e| format!("content_bytes is not valid base64: {e}"))?
    };
    let filename = {
        let f = opt_str(&input, "filename").trim();
        if f.is_empty() {
            "attachment"
        } else {
            f
        }
    };
    let content_type = {
        let c = opt_str(&input, "content_type").trim();
        if c.is_empty() {
            "application/octet-stream"
        } else {
            c
        }
    };
    // Assemble the multipart/form-data body as RAW BYTES (the reference uses mime/multipart), so binary
    // attachments round-trip byte-exact — no `from_utf8_lossy`. Upload via the byte-exact, ref-based
    // `http_bytes_ref` path with a non-binary response (we want the JSON attachment list back as
    // text) — the host resolves the base ref, so the plugin never holds a URL.
    let boundary = "----fluxjiraFormBoundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let mode = auth_mode(host)?;
    let content_type_header = format!("multipart/form-data; boundary={boundary}");
    let resp = host.http_bytes_ref(
        mode.base,
        "POST",
        &api_path(&format!("/issue/{}/attachments", urlencode(&key))),
        Some(mode.purpose),
        &[
            ("Accept", "application/json"),
            ("content-type", content_type_header.as_str()),
            ("X-Atlassian-Token", "no-check"),
        ],
        Some(&body),
        false,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "jira attachment upload → {} {}",
            resp.status,
            String::from_utf8_lossy(&resp.bytes)
        ));
    }
    let attachments: Value = serde_json::from_slice(&resp.bytes)
        .map_err(|e| format!("attachment upload response not JSON: {e}"))?;
    Ok(json!({"ok": true, "issue_key": key, "attachments": attachments}))
}

pub(crate) fn attachment_get(
    input: AttachmentGetInput,
    host: &mut Host,
) -> Result<AttachmentGetOutput, String> {
    let AttachmentGetInput {
        attachment_id,
        filename,
        mime_type,
        blob_ref: _,
    } = input;
    let id = attachment_id.trim().to_string();
    if id.is_empty() {
        return Err("`attachment_id` (string) required".into());
    }
    let mode = auth_mode(host)?;
    // Byte-exact, ref-based download: binary_response=true returns the raw bytes (no UTF-8
    // corruption), and the host resolves the base ref — the plugin never holds a URL.
    let resp = host.http_bytes_ref(
        mode.base,
        "GET",
        &api_path(&format!("/attachment/content/{}", urlencode(&id))),
        Some(mode.purpose),
        &[],
        None,
        true,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("jira attachment get → {}", resp.status));
    }
    let bytes = resp.bytes;
    let filename = filename.unwrap_or_default();
    let filename = match filename.trim() {
        "" => id.clone(),
        filename => filename.to_string(),
    };
    let blob_ref = host.blob_put(&filename, &bytes)?;
    Ok(AttachmentGetOutput {
        id,
        filename,
        mime_type: mime_type.unwrap_or_default(),
        size: bytes.len(),
        blob_ref,
    })
}

pub(crate) fn attachment_list(
    input: AttachmentListInput,
    host: &mut Host,
) -> Result<AttachmentListOutput, String> {
    let key = [
        Some(input.key.as_str()),
        input.id.as_deref(),
        input.issue_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|key| !key.is_empty())
    .unwrap_or_default();
    if key.is_empty() {
        return Err("`key` (issue key) required".into());
    }
    let issue = jget(
        host,
        &format!("/issue/{}?fields=attachment", urlencode(key)),
    )?;
    let attachments = issue
        .get("fields")
        .and_then(|f| f.get("attachment"))
        .cloned()
        .unwrap_or(json!([]));
    let count = attachments.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(AttachmentListOutput {
        issue_key: key.to_string(),
        count,
        attachments: attachments.as_array().cloned().unwrap_or_default(),
    })
}

pub(crate) fn attachment_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = opt_str(&input, "attachment_id").trim();
    if id.is_empty() {
        return Err("`attachment_id` (string) required".into());
    }
    jsend_noresp(
        host,
        "DELETE",
        &format!("/attachment/{}", urlencode(id)),
        None,
    )?;
    Ok(json!({"ok": true, "attachment_id": id}))
}
