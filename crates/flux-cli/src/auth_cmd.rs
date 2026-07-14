use super::*;

/// `flux auth status | login <provider>`.
/// Map a resolved `provider/model` spec to the `flux auth status` row it authenticates against, so
/// the status view can flag the active default provider. Returns `None` for specs that need no
/// listed credential (local `ollama*`, or `aws`, which isn't a listed row).
pub(super) fn auth_row_for_spec(spec: &str) -> Option<&'static str> {
    // The offline `mock` provider needs no credential (bare `mock` resolves to `anthropic` in
    // `flux_providers::spec::provider_prefix` for provider construction, but there is no key to
    // flag here).
    if spec == "mock" {
        return None;
    }
    match flux_providers::spec::provider_prefix(spec)? {
        "anthropic" => Some("anthropic"),
        "claude" => Some("claude"),
        "openai" => Some("openai"),
        "codex" => Some("codex"),
        "openrouter" | "openrouter-anthropic" => Some("openrouter"),
        // `aws` (not a listed status row) and local `ollama*` (keyless) have no row to mark active.
        _ => None,
    }
}

/// Render `flux auth status` grouped by state (Available / Not configured), with a summary line and
/// an active-default-provider marker. Pure (returns the block) so it is unit-testable.
pub(super) fn format_auth_status(
    rows: &[flux_credentials::ProviderAuth],
    default_spec: &str,
    active: Option<&str>,
) -> String {
    let total = rows.len();
    let avail = rows.iter().filter(|r| r.available).count();
    let mut out = String::new();
    out.push_str(&format!("Providers · {avail} of {total} configured\n"));

    // Default-model line: name the resolved default provider and whether its credential is present.
    match active {
        Some(p) => {
            let mark = match rows.iter().find(|r| r.provider == p).map(|r| r.available) {
                Some(true) => " ✓",
                Some(false) => " ·",
                None => "",
            };
            out.push_str(&format!("default model: {default_spec} → {p}{mark}\n"));
        }
        None => out.push_str(&format!("default model: {default_spec}\n")),
    }

    let w = rows.iter().map(|r| r.provider.len()).max().unwrap_or(0);
    let available: Vec<_> = rows.iter().filter(|r| r.available).collect();
    let missing: Vec<_> = rows.iter().filter(|r| !r.available).collect();

    if !available.is_empty() {
        out.push_str("\n  Available\n");
        let show_marker = available.iter().any(|r| active == Some(r.provider));
        for r in &available {
            if show_marker {
                let act = if active == Some(r.provider) {
                    "← active"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "    ✓ {:<w$}   {:<8}   {}\n",
                    r.provider, act, r.source
                ));
            } else {
                out.push_str(&format!("    ✓ {:<w$}   {}\n", r.provider, r.source));
            }
        }
    }
    if !missing.is_empty() {
        out.push_str("\n  Not configured\n");
        // Mark the active default here too if it's unconfigured — otherwise the `← active` tag would
        // vanish exactly when the user most needs to see which missing provider is the default.
        let show_marker = missing.iter().any(|r| active == Some(r.provider));
        for r in &missing {
            let hint = r.hint.as_deref().unwrap_or(r.source.as_str());
            if show_marker {
                let act = if active == Some(r.provider) {
                    "← active"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "    · {:<w$}   {:<8}   {}\n",
                    r.provider, act, hint
                ));
            } else {
                out.push_str(&format!("    · {:<w$}   {}\n", r.provider, hint));
            }
        }
    }
    out
}

pub(super) async fn run_auth(action: Option<AuthAction>) -> Result<()> {
    match action.unwrap_or(AuthAction::Status) {
        AuthAction::Status => {
            let cwd = std::env::current_dir().unwrap_or_default();
            // A malformed config must not silently report the wrong "default model" as configured.
            let cfg =
                flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
            let default_spec = resolve_model_spec(&None, &cfg);
            let active = auth_row_for_spec(&default_spec);
            let rows = flux_credentials::auth_status();
            print!("{}", format_auth_status(&rows, &default_spec, active));
            Ok(())
        }
        AuthAction::Login { provider, password } => match provider.as_str() {
            // The built-in providers only speak their PKCE flows — reject `--password` instead
            // of silently ignoring it (it is the plugin-OAuth password grant, D-82).
            name @ ("claude" | "codex") if password => {
                bail!("--password only applies to an installed OAuth2 plugin — `{name}` uses its browser PKCE flow")
            }
            "claude" => login_claude().await,
            "codex" => login_codex().await,
            // Any other name is treated as an installed OAuth2 plugin (plugin-oauth, D-82).
            name => login_plugin(name, password).await,
        },
        AuthAction::Set {
            plugin,
            purpose,
            clear,
        } => auth_set(&plugin, purpose.as_deref(), clear).await,
    }
}

/// Store (or `--clear`) a plain bearer for an installed plugin's auth purpose (D-126): validate the
/// plugin + purpose against the live manifest, prompt hidden for the token (read one stdin line
/// when piped, so `printf '%s' "$TOK" | flux auth set …` scripts), and persist it under
/// `plugin:<name>:<purpose>` — the same store key the host's purpose resolution consults before
/// falling back to the declared env keys. The token value is never echoed.
pub(super) async fn auth_set(name: &str, purpose: Option<&str>, clear: bool) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set — no plugin store"))?;
    let desc = flux_plugin::load_descriptor(&dir, name)
        .context("load plugin descriptor")?
        .ok_or_else(|| anyhow::anyhow!("no such plugin `{name}` — install it first"))?;
    let manifest = spawn_and_load_manifest(name, &desc).await?;
    let declared = || {
        manifest
            .auth
            .iter()
            .map(|a| a.purpose.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let method = match purpose {
        Some(p) => manifest
            .auth
            .iter()
            .find(|a| a.purpose == p)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "plugin `{name}` declares no auth purpose `{p}` (declared: {})",
                    declared()
                )
            })?,
        None => match manifest.auth.as_slice() {
            [] => bail!("plugin `{name}` declares no auth methods"),
            [only] => only,
            _ => bail!(
                "plugin `{name}` declares {} auth purposes — name one: {}",
                manifest.auth.len(),
                declared()
            ),
        },
    };
    let key = format!("plugin:{name}:{}", method.purpose);
    if clear {
        flux_credentials::delete_token(&key)?;
        println!(
            "\u{2713} cleared stored token for plugin `{name}` (purpose `{}`)",
            method.purpose
        );
        return Ok(());
    }
    let prompt = format!("{} for `{name}`: ", method.purpose);
    // The prompt blocks on user think-time — keep it off the runtime thread.
    let token = tokio::task::spawn_blocking(move || -> Result<String> {
        if std::io::stdin().is_terminal() {
            rpassword::prompt_password(&prompt).context("read token")
        } else {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("read token from stdin")?;
            Ok(line)
        }
    })
    .await
    .context("token prompt task")??;
    let token = token.trim();
    if token.is_empty() {
        bail!("empty token — nothing stored");
    }
    flux_credentials::save_token(
        &key,
        &flux_credentials::OAuthToken {
            access: token.to_string(),
            refresh: None,
            expires_at_ms: None,
            account_id: None,
        },
    )?;
    println!(
        "\u{2713} stored token for plugin `{name}` (purpose `{}`) in ~/.flux/credentials.toml",
        method.purpose
    );
    Ok(())
}

/// Interactive Anthropic (Claude subscription) PKCE login.
pub(super) async fn login_claude() -> Result<()> {
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::anthropic_authorize_url(&pkce, &state);
    println!(
        "Open this URL, approve access, then paste the code from the callback page:\n\n{url}\n"
    );
    // Off the runtime thread: the user can sit on this prompt indefinitely.
    let code = tokio::task::spawn_blocking(|| prompt_line("code: "))
        .await
        .context("code prompt task")??;
    flux_credentials::anthropic_exchange_and_store(code.trim(), &state, &pkce.verifier)
        .await
        .context("exchange authorization code")?;
    println!("\u{2713} stored Claude subscription credentials in ~/.flux/credentials.toml");
    Ok(())
}

/// Interactive Codex (ChatGPT subscription) PKCE login. Unlike claude's paste-the-code flow, the
/// codex client's registered redirect is `http://localhost:1455/auth/callback` (the upstream codex
/// CLI's pattern), so flux listens there and the code arrives without pasting.
pub(super) async fn login_codex() -> Result<()> {
    codex_login_flow(flux_credentials::CODEX_TOKEN_URL, |url, _state| async move {
        println!(
            "Open this URL and approve access — flux is listening on localhost:{} for the redirect:\n\n{url}\n",
            flux_credentials::CODEX_REDIRECT_PORT
        );
        wait_for_codex_callback().await
    })
    .await?;
    println!("\u{2713} stored Codex subscription credentials in ~/.flux/credentials.toml");
    Ok(())
}

/// Drive the codex PKCE login: generate the PKCE pair + CSRF state, hand the authorize URL (and
/// the state, for test injection) to `callback`, then exchange the returned `code#state` against
/// `token_url` and persist under the `codex` provider. The interactive path passes the real token
/// endpoint + the localhost:1455 listener; the hermetic test passes a loopback stub + a canned
/// callback (no browser, no network).
pub(super) async fn codex_login_flow<F, Fut>(token_url: &str, callback: F) -> Result<()>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::codex_authorize_url(&pkce, &state);
    let code = callback(url, state.clone()).await?;
    flux_credentials::codex_exchange_and_store_at(token_url, &code, &state, &pkce.verifier)
        .await
        .context("exchange authorization code")
}

/// Bind the codex client's registered redirect address (`localhost:1455`) and wait for the OAuth
/// redirect, answering the browser with a small confirmation page. Non-callback requests (e.g.
/// `/favicon.ico`) get a 404 and the wait continues. Bounded at 300s like its generic sibling
/// [`wait_for_oauth_callback`] — an abandoned browser flow must not hang the login forever.
/// Returns the callback as `code#state` — the shape `codex_exchange_and_store` binds against the
/// login's CSRF state.
pub(super) async fn wait_for_codex_callback() -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", flux_credentials::CODEX_REDIRECT_PORT))
            .await
            .with_context(|| {
                format!(
            "bind localhost:{} for the OAuth callback (is another login or the codex CLI running?)",
            flux_credentials::CODEX_REDIRECT_PORT
        )
            })?;
    let accept = async {
        loop {
            let (mut sock, _) = listener.accept().await.context("accept OAuth callback")?;
            // The callback is a small GET; one read is enough for the request line we parse.
            let mut buf = vec![0u8; 8192];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    // A failed read is this connection's problem, not the login's — say so and
                    // keep listening rather than silently 404-ing an empty request.
                    eprintln!("{}", style::dim(&format!("(callback read failed: {e})")));
                    continue;
                }
            };
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            // "GET <target> HTTP/1.1" — take the target.
            let target = req.split_whitespace().nth(1).unwrap_or("");
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            if path != flux_credentials::CODEX_REDIRECT_PATH {
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            }
            let result = parse_codex_callback(query);
            let page = match &result {
                Ok(_) => "Login complete — you can return to the terminal.",
                Err(_) => "Login failed — see the terminal for details.",
            };
            let body = format!("<!doctype html><html><body><p>{page}</p></body></html>");
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let (code, state) = result?;
            return Ok(format!("{code}#{state}"));
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(300), accept).await {
        Ok(r) => r,
        Err(_) => bail!(
            "timed out waiting for the OAuth callback on localhost:{}",
            flux_credentials::CODEX_REDIRECT_PORT
        ),
    }
}

/// Extract `code`/`state` (or the provider's `error`) from the OAuth callback query string.
pub(super) fn parse_codex_callback(query: &str) -> Result<(String, String)> {
    let (mut code, mut state, mut error) = (None, None, None);
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }
    if let Some(e) = error {
        bail!("authorization failed: {e}");
    }
    match (code, state) {
        (Some(c), Some(s)) if !c.is_empty() => Ok((c, s)),
        _ => bail!("OAuth callback did not include an authorization code and state"),
    }
}

/// Log in to an installed OAuth2 plugin (plugin-oauth, D-82): load its manifest, resolve its declared
/// OAuth2 endpoint, run the browser PKCE `authorization_code` flow (or the `--password` grant), and
/// store the tokens under `plugin:<name>:<purpose>` — the same key the host resolves at call time, so
/// a subsequent `flux plugin call` needs no env token.
pub(super) async fn login_plugin(name: &str, password: bool) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set — no plugin store"))?;
    let desc = flux_plugin::load_descriptor(&dir, name)
        .context("load plugin descriptor")?
        .ok_or_else(|| anyhow::anyhow!("no such plugin `{name}` — install it first"))?;
    let manifest = spawn_and_load_manifest(name, &desc).await?;
    let method = manifest
        .auth
        .iter()
        .find(|a| a.oauth2.is_some())
        .ok_or_else(|| anyhow::anyhow!("plugin `{name}` declares no OAuth2 auth method"))?;
    let oauth = method.oauth2.as_ref().expect("filtered to Some above");
    let base = resolve_manifest_endpoint(&manifest, &oauth.endpoint).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot resolve OAuth endpoint `{}` for plugin `{name}` — set its declared env or default",
            oauth.endpoint
        )
    })?;
    let token_url = join_endpoint_path(&base, &oauth.token_path);
    let key = format!("plugin:{name}:{}", method.purpose);
    let scope = oauth.scopes.join(" ");

    let token = if password {
        // Both prompts block on user think-time — keep them off the runtime thread.
        let (username, secret) = tokio::task::spawn_blocking(|| -> Result<(String, String)> {
            let username = prompt_line("username: ")?;
            let secret = rpassword::prompt_password("password: ").context("read password")?;
            Ok((username, secret))
        })
        .await
        .context("credential prompt task")??;
        flux_credentials::oauth_token_grant(
            &token_url,
            &[
                ("grant_type", "password"),
                ("username", username.trim()),
                ("password", &secret),
                ("client_id", &oauth.client_id),
                ("scope", &scope),
            ],
        )
        .await
        .context("password grant")?
    } else {
        let redirect = oauth.redirect.as_ref().ok_or_else(|| {
            anyhow::anyhow!("plugin `{name}` OAuth2 declares no loopback redirect; use --password")
        })?;
        let redirect_uri = format!("http://localhost:{}{}", redirect.port, redirect.path);
        let authorize_url = join_endpoint_path(&base, &oauth.authorize_path);
        let (port, path) = (redirect.port, redirect.path.clone());
        plugin_oauth_code_grant(
            &token_url,
            &authorize_url,
            &oauth.client_id,
            &scope,
            &redirect_uri,
            |url, _state| async move {
                println!(
                    "Open this URL and approve access — flux is listening on localhost:{port} for the redirect:\n\n{url}\n"
                );
                wait_for_oauth_callback(port, &path).await
            },
        )
        .await?
    };
    flux_credentials::save_token(&key, &token)?;
    println!(
        "\u{2713} stored OAuth credentials for plugin `{name}` (purpose `{}`) in ~/.flux/credentials.toml",
        method.purpose
    );
    Ok(())
}

/// The `authorization_code` + PKCE half of a plugin login (plugin-oauth, D-82): build the authorize
/// URL, run the browser callback (injected — the interactive path binds the loopback listener; the
/// test injects a canned callback), verify the CSRF state, and exchange the code against `token_url`.
pub(super) async fn plugin_oauth_code_grant<F, Fut>(
    token_url: &str,
    authorize_url: &str,
    client_id: &str,
    scope: &str,
    redirect_uri: &str,
    callback: F,
) -> Result<flux_credentials::OAuthToken>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::oauth_authorize_url(
        authorize_url,
        client_id,
        redirect_uri,
        scope,
        &pkce,
        &state,
    );
    let code_state = callback(url, state.clone()).await?;
    let (code, ret_state) = code_state
        .split_once('#')
        .unwrap_or((code_state.as_str(), ""));
    if ret_state != state {
        bail!("OAuth callback state mismatch — possible CSRF, aborting login");
    }
    flux_credentials::oauth_token_grant(
        token_url,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", &pkce.verifier),
        ],
    )
    .await
    .context("exchange authorization code")
}

/// Resolve a manifest endpoint's base URL for login (declared env keys → default). Templated
/// endpoints are resolved host-side at call time, not here.
pub(super) fn resolve_manifest_endpoint(
    m: &flux_plugin::PluginManifest,
    name: &str,
) -> Option<String> {
    let ep = m.endpoints.iter().find(|e| e.name == name)?;
    for k in &ep.env {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    ep.default.clone()
}

/// Join an endpoint base URL and a declared path (`https://host` + `/oauth/token`).
pub(super) fn join_endpoint_path(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Prompt on the terminal and read one trimmed line (visible echo — for a non-secret like a username).
pub(super) fn prompt_line(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

/// Bind `127.0.0.1:{port}` and wait for the OAuth redirect at `path`, answering the browser with a
/// small confirmation page (plugin-oauth, D-82 — the generic form of [`wait_for_codex_callback`],
/// with a bounded wait). Non-callback requests get a 404 and the wait continues. Returns `code#state`.
pub(super) async fn wait_for_oauth_callback(port: u16, path: &str) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| {
            format!("bind localhost:{port} for the OAuth callback (is another login running?)")
        })?;
    let accept = async {
        loop {
            let (mut sock, _) = listener.accept().await.context("accept OAuth callback")?;
            let mut buf = vec![0u8; 8192];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("{}", style::dim(&format!("(callback read failed: {e})")));
                    continue;
                }
            };
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let target = req.split_whitespace().nth(1).unwrap_or("");
            let (req_path, query) = target.split_once('?').unwrap_or((target, ""));
            if req_path != path {
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            }
            let result = parse_codex_callback(query);
            let page = if result.is_ok() {
                "Login complete — you can return to the terminal."
            } else {
                "Login failed — see the terminal for details."
            };
            let body = format!("<!doctype html><html><body><p>{page}</p></body></html>");
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let (code, state) = result?;
            return Ok(format!("{code}#{state}"));
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(300), accept).await {
        Ok(r) => r,
        Err(_) => bail!("timed out waiting for the OAuth callback on localhost:{port}"),
    }
}

/// Minimal percent-decoding for OAuth callback query values (`+` is left as-is — codes and states
/// are URL-safe base64, never space-bearing).
pub(super) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
