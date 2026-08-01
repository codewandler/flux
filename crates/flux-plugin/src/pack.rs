//! Plugin pack distribution — the demand side (D-47, `docs/designs/plugin-distribution.md`).
//!
//! A flux user without the source tree runs `flux plugin install gitlab slack` and gets working,
//! verified plugins. This module resolves the newest (or an explicit) `plugins-v<version>` GitHub
//! release from the pack channel D-46 publishes, verifies the release's signed index, checksums
//! every archive before it is unpacked, and registers the result in the existing descriptor store.
//!
//! The trust ladder (design doc, "Security model & supply chain", steps 1-4):
//! 1. **Fixed origin.** Every download URL is built only from `(repo, tag, asset-name)` against
//!    `github.com` — an index can never redirect a download elsewhere ([`require_bare_asset_name`]).
//! 2. **Signed index.** `plugins-index.json` + `.minisig` are verified against [`PUBLIC_KEY`]
//!    (`minisign-verify`) before a single byte of it is trusted. Verification is fail-closed: there
//!    is no bypass flag ([`verify_index_signature`]).
//! 3. **Checksum before executable.** Every archive's sha256 is checked against the (now-trusted)
//!    index entry *before* it is unpacked into place ([`install_one`]).
//! 4. **Install-time recording.** The written [`crate::PluginDescriptor`] carries `version`,
//!    `sha256` (of the installed binary), and `source` (the release tag) — the anchor D-48's
//!    spawn-time re-hash enforcement will check against.
//!
//! Everything that touches the network goes through the injectable [`Fetcher`] seam, so every test
//! in this module is hermetic (fixture indexes/archives, no network in the gate).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use flux_core::{Error, Result};

/// The GitHub repo the pack channel lives in (`releases/download/<tag>/<asset>`).
pub const DEFAULT_REPO: &str = "codewandler/flux";

/// The production minisign public key for the `codewandler/flux` plugin pack channel. Generated
/// once via the `generate_pack_keypair` helper test (below); the matching secret key lives only in
/// the `MINISIGN_SECRET_KEY` GitHub Actions secret consumed by `.github/workflows/release-plugins.yml`
/// — never in this repo. Key rotation = embed a new key here and ship a flux release (the design
/// doc's residual-risk note: embedding multiple accepted keys is a future hardening step, not
/// implemented here).
pub const PUBLIC_KEY: &str = "RWSd30xfPYIFZc6x0bb9KukLrw2ax49cKMbP6bKpj5wpACesSqZE1qcp";

/// The target triple this flux binary was built for, restricted to the five targets D-46 releases
/// for. A build for any other target has no prebuilt pack artifacts — empty means "unsupported";
/// callers should refuse with a message pointing at the documented source fallback
/// (`--dir`/`git clone … && cargo build --release`).
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
pub const CURRENT_TARGET: &str = "aarch64-apple-darwin";
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
pub const CURRENT_TARGET: &str = "aarch64-unknown-linux-gnu";
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
pub const CURRENT_TARGET: &str = "x86_64-apple-darwin";
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const CURRENT_TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
pub const CURRENT_TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(not(any(
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux"),
    all(target_arch = "x86_64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "x86_64", target_os = "windows"),
)))]
pub const CURRENT_TARGET: &str = "";

// ---------------------------------------------------------------------------
// Index schema (`schema: 1`) — must match `plugins/pack-index`'s generator output exactly
// (D-46, `plugins/pack-index/src/main.rs`).
// ---------------------------------------------------------------------------

/// `plugins-index.json`, deserialized. `description` is absent from the generator's current output
/// (D-46 doesn't emit it), so it's serde-defaulted — either form of the index reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    pub schema: u32,
    pub pack_version: String,
    pub protocol: String,
    pub released_at: String,
    pub plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginEntry {
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub artifacts: BTreeMap<String, Artifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub asset: String,
    pub sha256: String,
    pub size: u64,
}

/// A bare download-asset filename: no path separators, no `://`, no `..`/absolute components — the
/// same guard D-35's `descriptor_path` uses for plugin names, reused here so a compromised or
/// malformed index can never steer a download URL anywhere but
/// `github.com/<repo>/releases/download/<tag>/<name>` (the asset-name invariant).
fn require_bare_asset_name(name: &str) -> Result<()> {
    crate::invalid_plugin_name(name).map_err(|_| {
        Error::Other(format!(
            "asset name `{name}` is not a bare file name (URL- or path-shaped values are forbidden)"
        ))
    })
}

/// The stronger rule for a *plugin archive* asset (an index `artifacts.*.asset` value): bare, per
/// [`require_bare_asset_name`], and it must also start with `flux-plugin-` — mirrors
/// `pack-index::validate_asset_name` (the supply-side rule, `plugins/pack-index/src/main.rs`) on
/// the demand side.
fn validate_plugin_asset_name(name: &str) -> Result<()> {
    require_bare_asset_name(name)?;
    if !name.starts_with("flux-plugin-") {
        return Err(Error::Other(format!(
            "asset name `{name}` must start with `flux-plugin-`"
        )));
    }
    Ok(())
}

fn is_windows_target(target: &str) -> bool {
    target.ends_with("windows-msvc")
}

fn exe_name(plugin_name: &str, target: &str) -> String {
    if is_windows_target(target) {
        format!("flux-plugin-{plugin_name}.exe")
    } else {
        format!("flux-plugin-{plugin_name}")
    }
}

/// Hex-encoded SHA-256 — the one hash spelled everywhere in the pack ladder (index entries,
/// descriptors, store sidecars, spawn-time verification).
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The versioned-store location of one plugin version's binary:
/// `<store_root>/<name>/<version>/flux-plugin-<name>[.exe]`.
pub fn stored_binary_path(store_root: &Path, name: &str, version: &str, target: &str) -> PathBuf {
    store_root
        .join(name)
        .join(version)
        .join(exe_name(name, target))
}

/// The sidecar file recording a stored binary's sha256, written at (verified) unpack time —
/// what lets `pin` repoint to an already-stored version and `rollback` flip to `previous`
/// **offline** without re-fetching the signed index and without blessing unverified bytes.
fn sidecar_path(binary: &Path) -> PathBuf {
    let name = binary
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    binary.with_file_name(format!("{name}.sha256"))
}

// ---------------------------------------------------------------------------
// Signature verification
// ---------------------------------------------------------------------------

/// Verify `index_bytes` against the detached minisign signature `sig_text`, using
/// `public_key_base64`. Fail-closed: any decoding or verification failure is a hard `Err` — there
/// is no bypass (the no-fallbacks rule; design doc "Security model & supply chain" step 2).
fn verify_index_signature(
    index_bytes: &[u8],
    sig_text: &str,
    public_key_base64: &str,
) -> Result<()> {
    let pk = minisign_verify::PublicKey::from_base64(public_key_base64)
        .map_err(|e| Error::Other(format!("embedded minisign public key is invalid: {e}")))?;
    let sig = minisign_verify::Signature::decode(sig_text).map_err(|e| {
        Error::Other(format!(
            "plugins-index.json.minisig is not a valid minisign signature: {e}"
        ))
    })?;
    pk.verify(index_bytes, &sig, false).map_err(|e| {
        Error::Other(format!(
            "plugins-index.json signature verification FAILED ({e}) — refusing to trust a pack \
             whose index does not verify against the embedded flux public key (possible \
             tampering, a stale mirror, or a corrupted download)"
        ))
    })
}

// ---------------------------------------------------------------------------
// Fetcher seam — every network access goes through here, so tests inject a hermetic fixture.
// ---------------------------------------------------------------------------

/// The network boundary of the pack channel. Two operations, both scoped to `(repo, tag, asset)` —
/// never a caller-supplied URL — so the asset-name invariant holds structurally, not just by
/// convention.
#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// List every release tag in `repo` (used only for latest-release resolution — an explicit
    /// `@<version>` needs no API call, per the design).
    async fn list_release_tags(&self, repo: &str) -> Result<Vec<String>>;

    /// Fetch one release asset's raw bytes (`releases/download/<tag>/<asset>`).
    async fn fetch_release_asset(&self, repo: &str, tag: &str, asset: &str) -> Result<Vec<u8>>;
}

/// The real [`Fetcher`]: unauthenticated GitHub API for release listing, direct release-download
/// URLs for assets. Unauthenticated rate limits are acceptable (design doc, "Resolution").
pub struct GithubFetcher {
    client: reqwest::Client,
}

impl Default for GithubFetcher {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl Fetcher for GithubFetcher {
    async fn list_release_tags(&self, repo: &str) -> Result<Vec<String>> {
        let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
        let resp = self
            .client
            .get(&url)
            .header("User-Agent", "flux-cli")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| Error::Http(format!("list releases for {repo}: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Http(format!(
                "list releases for {repo}: HTTP {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Http(format!("parse releases response for {repo}: {e}")))?;
        Ok(body
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("tag_name").and_then(|v| v.as_str()).map(String::from))
            .collect())
    }

    async fn fetch_release_asset(&self, repo: &str, tag: &str, asset: &str) -> Result<Vec<u8>> {
        // The asset-name invariant, enforced at the one place a URL is ever built.
        require_bare_asset_name(asset)?;
        let url = format!("https://github.com/{repo}/releases/download/{tag}/{asset}");
        let resp = self
            .client
            .get(&url)
            .header("User-Agent", "flux-cli")
            .send()
            .await
            .map_err(|e| Error::Http(format!("download {asset} ({tag}): {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Http(format!(
                "download {asset} ({tag}): HTTP {}",
                resp.status()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Http(format!("read {asset} body: {e}")))?;
        Ok(bytes.to_vec())
    }
}

/// Resolve a `plugins-v*` release tag: an explicit `version` needs no API call (design doc,
/// "Resolution"); otherwise list releases, keep the `plugins-v` prefixed ones, and take the
/// highest dotted-numeric version.
async fn resolve_release_tag(
    fetcher: &dyn Fetcher,
    repo: &str,
    version: Option<&str>,
) -> Result<String> {
    if let Some(v) = version {
        return Ok(format!("plugins-v{v}"));
    }
    let tags = fetcher.list_release_tags(repo).await?;
    let mut versions: Vec<(Vec<u64>, String)> = tags
        .into_iter()
        .filter_map(|t| {
            let v = t.strip_prefix("plugins-v")?;
            let parsed: Option<Vec<u64>> = v.split('.').map(|p| p.parse::<u64>().ok()).collect();
            parsed.map(|p| (p, t))
        })
        .collect();
    versions.sort();
    versions
        .into_iter()
        .next_back()
        .map(|(_, t)| t)
        .ok_or_else(|| {
            Error::Other(format!(
                "no `plugins-v*` release found in `{repo}` — has the plugin pack ever been released?"
            ))
        })
}

/// Split `<name>[@<version>]` into its parts.
fn split_name_version(spec: &str) -> (String, Option<String>) {
    match spec.split_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (spec.to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// Unpack: `.tar.xz` (unix/macOS targets) or `.zip` (the windows target), single binary entry.
// ---------------------------------------------------------------------------

/// Extract the single `exe` entry from `archive_bytes` and write it to `dest_dir/exe`. Returns the
/// extracted bytes (the caller hashes them for the descriptor's `sha256`). Nothing is written
/// before the entry is fully read into memory — a truncated/corrupt archive fails before any file
/// touches disk.
fn unpack_single_binary(
    archive_bytes: &[u8],
    windows: bool,
    exe: &str,
    dest_dir: &Path,
) -> Result<Vec<u8>> {
    let bytes = if windows {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive_bytes))
            .map_err(|e| Error::Other(format!("open archive as zip: {e}")))?;
        let mut file = zip
            .by_name(exe)
            .map_err(|e| Error::Other(format!("archive has no `{exe}` entry: {e}")))?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf).map_err(Error::Io)?;
        buf
    } else {
        let mut decompressed = Vec::new();
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(archive_bytes), &mut decompressed)
            .map_err(|e| Error::Other(format!("xz-decompress archive: {e}")))?;
        let mut tar_archive = tar::Archive::new(std::io::Cursor::new(&decompressed));
        let mut found = None;
        for entry in tar_archive
            .entries()
            .map_err(|e| Error::Other(format!("read tar entries: {e}")))?
        {
            let mut entry = entry.map_err(|e| Error::Other(format!("read tar entry: {e}")))?;
            let path = entry
                .path()
                .map_err(|e| Error::Other(format!("tar entry path: {e}")))?
                .to_string_lossy()
                .into_owned();
            if path == exe {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf).map_err(Error::Io)?;
                found = Some(buf);
                break;
            }
        }
        found.ok_or_else(|| Error::Other(format!("archive has no `{exe}` entry")))?
    };

    std::fs::create_dir_all(dest_dir).map_err(Error::Io)?;
    let dest_path = dest_dir.join(exe);
    std::fs::write(&dest_path, &bytes).map_err(Error::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(0o755))
            .map_err(Error::Io)?;
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Install orchestration — the whole resolve → verify → download → checksum → unpack → register
// pipeline. `flux-cli` calls only this; it owns no protocol/verification logic of its own (design
// doc, "Code placement").
// ---------------------------------------------------------------------------

pub struct InstallRequest<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub repo: &'a str,
    pub public_key: &'a str,
    /// The descriptor store (`~/.flux/plugins`).
    pub descriptors_dir: &'a Path,
    /// The versioned binary store (`~/.flux/plugins/bin`).
    pub store_root: &'a Path,
    /// The current target triple (one of [`CURRENT_TARGET`]'s five values).
    pub target: &'a str,
}

#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub source: String,
    pub program: PathBuf,
    /// `true` when this version was already installed and nothing was downloaded (idempotent
    /// re-install — a note, not an error).
    pub already_installed: bool,
}

/// Resolve → verify → download → checksum → unpack → register every requested plugin from one
/// `plugins-v<version>` release. One fetched, one signature-verified index covers the whole batch
/// (the terraform "sign the aggregate" pattern) — archives are then fetched and checksummed
/// per plugin. `names` are `<name>[@<version>]`; a version suffix, if given, must agree across
/// every name in one call (the pack is released lockstep — `gitlab@0.2.0` means "pack release
/// plugins-v0.2.0", not an independent per-plugin version). `all` installs every plugin the
/// index names and cannot be combined with explicit names.
pub async fn install_many(
    req: &InstallRequest<'_>,
    names: &[String],
    all: bool,
) -> Result<Vec<InstalledPlugin>> {
    if names.is_empty() && !all {
        return Err(Error::Other(
            "no plugin names given and `--all` not set".into(),
        ));
    }
    if all && !names.is_empty() {
        return Err(Error::Other(
            "cannot combine explicit plugin names with `--all`".into(),
        ));
    }

    let mut requested_version: Option<String> = None;
    let mut requested_names: Vec<String> = Vec::new();
    for spec in names {
        let (name, version) = split_name_version(spec);
        if let Some(v) = version {
            match &requested_version {
                Some(existing) if existing != &v => {
                    return Err(Error::Other(format!(
                        "conflicting `@version` in one install call: `{existing}` vs `{v}` — install them separately"
                    )));
                }
                _ => requested_version = Some(v),
            }
        }
        requested_names.push(name);
    }

    let tag = resolve_release_tag(req.fetcher, req.repo, requested_version.as_deref()).await?;

    let index_bytes = req
        .fetcher
        .fetch_release_asset(req.repo, &tag, "plugins-index.json")
        .await?;
    let sig_bytes = req
        .fetcher
        .fetch_release_asset(req.repo, &tag, "plugins-index.json.minisig")
        .await?;
    let sig_text = String::from_utf8(sig_bytes)
        .map_err(|e| Error::Other(format!("plugins-index.json.minisig is not UTF-8: {e}")))?;
    verify_index_signature(&index_bytes, &sig_text, req.public_key)?;

    let index: Index = serde_json::from_slice(&index_bytes)
        .map_err(|e| Error::Other(format!("parse plugins-index.json: {e}")))?;
    if index.protocol != crate::PROTOCOL {
        return Err(Error::Other(format!(
            "plugin pack `{tag}` speaks protocol `{}`, this flux speaks `{}` — upgrade flux, or \
             install a pack release built for `{}`",
            index.protocol,
            crate::PROTOCOL,
            crate::PROTOCOL
        )));
    }

    let plugin_names: Vec<String> = if all {
        index.plugins.keys().cloned().collect()
    } else {
        requested_names
    };

    let mut out = Vec::new();
    for name in plugin_names {
        out.push(install_one(req, &tag, &index, &name).await?);
    }
    Ok(out)
}

async fn install_one(
    req: &InstallRequest<'_>,
    tag: &str,
    index: &Index,
    name: &str,
) -> Result<InstalledPlugin> {
    crate::invalid_plugin_name(name)
        .map_err(|_| Error::Other(format!("invalid plugin name `{name}`")))?;
    let entry = index.plugins.get(name).ok_or_else(|| {
        Error::Other(format!(
            "plugin `{name}` is not in this pack (available: {})",
            index.plugins.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
    })?;
    let artifact = entry.artifacts.get(req.target).ok_or_else(|| {
        Error::Other(format!(
            "plugin `{name}` has no prebuilt artifact for target `{}` (available: {})",
            req.target,
            entry
                .artifacts
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    validate_plugin_asset_name(&artifact.asset)?;

    let exe = exe_name(name, req.target);
    let dest_dir = req.store_root.join(name).join(&entry.version);
    let dest_path = dest_dir.join(&exe);

    // Idempotent re-install: the same version is already in the store — no-op with a note, no
    // fetch, nothing re-verified.
    let existing = crate::load_descriptor(req.descriptors_dir, name)?;
    if let Some(existing) = &existing {
        if existing.version.as_deref() == Some(entry.version.as_str()) && dest_path.is_file() {
            return Ok(InstalledPlugin {
                name: name.to_string(),
                version: entry.version.clone(),
                sha256: existing.sha256.clone().unwrap_or_default(),
                source: existing.source.clone().unwrap_or_else(|| tag.to_string()),
                program: dest_path,
                already_installed: true,
            });
        }
    }

    let archive_bytes = req
        .fetcher
        .fetch_release_asset(req.repo, tag, &artifact.asset)
        .await?;
    let archive_sha256 = sha256_hex(&archive_bytes);
    if archive_sha256 != artifact.sha256 {
        return Err(Error::Other(format!(
            "checksum mismatch for `{}`: index names sha256 {} but the downloaded archive hashes \
             to {} — refusing to unpack a tampered or corrupted download",
            artifact.asset, artifact.sha256, archive_sha256
        )));
    }

    let windows = is_windows_target(req.target);
    let binary_bytes = unpack_single_binary(&archive_bytes, windows, &exe, &dest_dir)?;
    let binary_sha256 = sha256_hex(&binary_bytes);
    // Record the verified binary's hash beside it (D-48): the store sidecar is what makes a later
    // `pin` to this version, or a `rollback` onto it, an offline repoint instead of a re-fetch.
    std::fs::write(sidecar_path(&dest_path), &binary_sha256).map_err(Error::Io)?;

    // A version *switch* remembers what it replaced (D-48): `rollback` flips back to `previous`
    // offline. A fresh install (or one replacing an unversioned local descriptor) has nothing in
    // the store to roll back to, so any earlier `previous` is carried forward unchanged.
    let (prior_pinned, prior_version, prior_previous) = existing
        .map(|d| (d.pinned, d.version, d.previous))
        .unwrap_or_default();
    let previous = match prior_version {
        Some(v) if v != entry.version => Some(v),
        _ => prior_previous,
    };

    crate::add_descriptor(
        req.descriptors_dir,
        name,
        &crate::PluginDescriptor {
            program: dest_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            pinned: prior_pinned,
            version: Some(entry.version.clone()),
            sha256: Some(binary_sha256.clone()),
            source: Some(tag.to_string()),
            previous,
            git_url: None,
            git_commit: None,
            // A version switch is not a re-grant (C-411): `add_descriptor` carries the grant of
            // record forward, so `None` here means "leave what the operator granted alone".
            capabilities: None,
            origin: None,
        },
    )?;

    Ok(InstalledPlugin {
        name: name.to_string(),
        version: entry.version.clone(),
        sha256: binary_sha256,
        source: tag.to_string(),
        program: dest_path,
        already_installed: false,
    })
}

// ---------------------------------------------------------------------------
// Pin / rollback / purge — the D-48 verified version switches over the store
// ---------------------------------------------------------------------------

/// The result of a [`pin`]: what the descriptor now points at, and how it got there.
#[derive(Debug, Clone)]
pub struct PinOutcome {
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// The version this pin replaced (now the descriptor's `previous`), if any.
    pub previous: Option<String>,
    /// `false` when the version was already in the versioned store with a recorded hash —
    /// the pin was an offline repoint, nothing downloaded.
    pub fetched: bool,
}

/// `flux plugin pin <name> <version>` (D-48): a **verified version switch**, not an advisory
/// label. Ensures the version is present in the versioned store — an already-stored version with
/// a hash sidecar is repointed **offline**; anything else goes through the same signed-index +
/// checksum path as `install` (`install_many`), so a version the index does not offer fails
/// cleanly there. The descriptor is repointed at the stored binary with its `sha256` + `version`
/// recorded (enforced at every spawn by [`crate::PluginHost::spawn_verified`]) and the replaced
/// version remembered in `previous` for [`rollback`]. Operator-set `args` survive the switch.
///
/// The offline path stamps `source` as `plugins-v<version>` — faithful because the pack is
/// released lockstep (a stored `<name>/<version>` can only have come from that release tag).
pub async fn pin(req: &InstallRequest<'_>, name: &str, version: &str) -> Result<PinOutcome> {
    crate::invalid_plugin_name(name)?;
    let prior = crate::load_descriptor(req.descriptors_dir, name)?;
    let (prior_args, prior_version, prior_previous) = prior
        .map(|d| (d.args, d.version, d.previous))
        .unwrap_or_default();

    let stored = stored_binary_path(req.store_root, name, version, req.target);
    let sidecar = sidecar_path(&stored);
    let (program, actual_version, sha256, source, fetched) =
        if stored.is_file() && sidecar.is_file() {
            let sha = std::fs::read_to_string(&sidecar)
                .map_err(Error::Io)?
                .trim()
                .to_string();
            (
                stored,
                version.to_string(),
                sha,
                format!("plugins-v{version}"),
                false,
            )
        } else {
            let installed = install_many(req, &[format!("{name}@{version}")], false).await?;
            let p = installed.into_iter().next().ok_or_else(|| {
                Error::Other(format!("install returned no result for `{name}@{version}`"))
            })?;
            (p.program, p.version, p.sha256, p.source, true)
        };

    let previous = match &prior_version {
        Some(v) if v != &actual_version => Some(v.clone()),
        _ => prior_previous,
    };
    crate::add_descriptor(
        req.descriptors_dir,
        name,
        &crate::PluginDescriptor {
            program: program.to_string_lossy().into_owned(),
            args: prior_args,
            pinned: Some(actual_version.clone()),
            version: Some(actual_version.clone()),
            sha256: Some(sha256.clone()),
            source: Some(source),
            previous: previous.clone(),
            git_url: None,
            git_commit: None,
            // A version switch is not a re-grant (C-411): `add_descriptor` carries the grant of
            // record forward, so `None` here means "leave what the operator granted alone".
            capabilities: None,
            origin: None,
        },
    )?;
    Ok(PinOutcome {
        name: name.to_string(),
        version: actual_version,
        sha256,
        previous,
        fetched,
    })
}

/// The result of a [`rollback`]: the flip that happened.
#[derive(Debug, Clone)]
pub struct RollbackOutcome {
    pub name: String,
    /// The version rolled back *from* (now the descriptor's `previous`, so a second rollback
    /// flips forward again).
    pub from: Option<String>,
    pub to: String,
    pub sha256: String,
}

/// `flux plugin rollback <name>` (D-48): repoint the descriptor at its `previous` version —
/// **offline and instant** by construction (this function has no fetcher: the side-by-side
/// versioned store plus the hash sidecar recorded at install time are all it reads). The current
/// and previous versions swap, so `rollback` twice is a round trip. A missing sidecar (a store
/// entry from before D-48) is a clean refusal, not a re-hash — recording whatever bytes happen to
/// be on disk would bless a tampered binary.
///
/// Clean cutover: `rollback` used to *clear the advisory pin*; it now flips versions. The
/// no-`previous` error says so explicitly.
pub fn rollback(
    descriptors_dir: &Path,
    store_root: &Path,
    target: &str,
    name: &str,
) -> Result<RollbackOutcome> {
    let d = crate::load_descriptor(descriptors_dir, name)?
        .ok_or_else(|| Error::Other(format!("no such plugin: {name}")))?;
    let Some(prev) = d.previous.clone() else {
        return Err(Error::Other(format!(
            "plugin `{name}` has no previous version recorded — `rollback` flips to the version \
             in place before the last version switch (it no longer clears the advisory pin); \
             switch versions explicitly with `flux plugin pin {name} <version>`"
        )));
    };
    let stored = stored_binary_path(store_root, name, &prev, target);
    if !stored.is_file() {
        return Err(Error::Other(format!(
            "previous version {prev} of `{name}` is not in the versioned store ({}) — nothing to \
             flip to offline; re-fetch it with `flux plugin pin {name} {prev}`",
            stored.display()
        )));
    }
    let sha256 = std::fs::read_to_string(sidecar_path(&stored))
        .map(|s| s.trim().to_string())
        .map_err(|_| {
            Error::Other(format!(
                "the versioned store has no recorded hash for `{name}` {prev} (installed before \
                 D-48's sidecars) — refusing to bless unverified bytes offline; re-record it with \
                 `flux plugin pin {name} {prev}` (verified fetch)"
            ))
        })?;

    let from = d.version.clone();
    crate::add_descriptor(
        descriptors_dir,
        name,
        &crate::PluginDescriptor {
            program: stored.to_string_lossy().into_owned(),
            args: d.args,
            pinned: Some(prev.clone()),
            version: Some(prev.clone()),
            sha256: Some(sha256.clone()),
            source: Some(format!("plugins-v{prev}")),
            previous: match &from {
                Some(v) if v != &prev => Some(v.clone()),
                _ => None,
            },
            git_url: None,
            git_commit: None,
            // A version switch is not a re-grant (C-411): `add_descriptor` carries the grant of
            // record forward, so `None` here means "leave what the operator granted alone".
            capabilities: None,
            origin: None,
        },
    )?;
    Ok(RollbackOutcome {
        name: name.to_string(),
        from,
        to: prev,
        sha256,
    })
}

/// Remove a plugin's entire versioned-store directory (`flux plugin uninstall --purge`).
/// Returns whether anything existed. The name is sanitized like every descriptor path (D-35) —
/// this is a destructive `remove_dir_all`, so a traversal name must be rejected before any
/// filesystem op.
pub fn purge_store(store_root: &Path, name: &str) -> Result<bool> {
    crate::invalid_plugin_name(name)?;
    let dir = store_root.join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(Error::Io)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// The THIRD install source (D-87): `install --git <url>` — clone + build from source.
//
// A signed pack cannot serve an out-of-tree plugin (every URL is hardcoded to `github.com/<repo>`,
// signed by a key only flux maintainers hold). The `--git` source is the `cargo install --git`
// model: clone a repo at a ref, detect a `flux-plugin-*` crate, `cargo build --release --locked`,
// register the built binary. Trust is source-transparent, not signed — the descriptor is labelled
// `from-source (unverified)` ([`crate::Verification::UnverifiedFromSource`]), gated behind explicit
// consent and a commit disclosure BEFORE any code is built.
//
// The heavy real work — `git clone` + `cargo build`, both through the guarded `flux_system::System`
// (argv-only, no shell) — sits behind the injectable [`SourceBuilder`] seam, exactly as [`Fetcher`]
// hides the network for the signed pack, so every test here is hermetic (no network, no toolchain).
// ---------------------------------------------------------------------------

/// A git reference to clone at — the `--tag`/`--rev`/`--branch` flags (mutually exclusive), or the
/// remote's default branch when none is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    /// `--tag <t>`: an annotated/lightweight tag.
    Tag(String),
    /// `--rev <r>`: an exact commit sha (or anything `git checkout` resolves).
    Rev(String),
    /// `--branch <b>`: a branch head.
    Branch(String),
    /// No ref flag: the remote's default branch (`HEAD` after a plain clone).
    Default,
}

impl GitRef {
    /// A human-readable one-liner for messages ("branch `main`", "commit `abc…`", "default branch").
    pub fn describe(&self) -> String {
        match self {
            GitRef::Tag(t) => format!("tag `{t}`"),
            GitRef::Rev(r) => format!("commit `{r}`"),
            GitRef::Branch(b) => format!("branch `{b}`"),
            GitRef::Default => "the default branch".to_string(),
        }
    }
}

/// A built flux-plugin binary handed back by [`SourceBuilder::build`].
#[derive(Debug, Clone)]
pub struct BuiltPlugin {
    /// The cargo bin target that was built (`flux-plugin-<name>`).
    pub bin_name: String,
    /// The freshly built release binary on disk (inside the clone's `target/release`).
    pub binary: PathBuf,
}

/// The clone-then-build boundary for `--git` source installs — the one seam that touches the
/// network + the Rust toolchain, injected so unit tests supply a fake "clone + build" without
/// either (mirrors [`Fetcher`] for the signed pack). The production implementation drives `git`
/// and `cargo` through the guarded [`flux_system::System`] (argv-only, cleared env) rooted at the
/// clone directory — never a raw `std::process::Command`, never a shell string.
#[async_trait::async_trait]
pub trait SourceBuilder: Send + Sync {
    /// Clone `url` at `git_ref` into `clone_dir` (creating/updating it in place — the clone is a
    /// cache reused across installs), check the ref out, and return the **resolved commit sha**
    /// (`git rev-parse HEAD`). The commit is what the trust gate discloses before any build.
    async fn clone_and_resolve(
        &self,
        url: &str,
        git_ref: &GitRef,
        clone_dir: &Path,
    ) -> Result<String>;

    /// Detect the flux-plugin binary target in an already-checked-out `clone_dir` (a `flux-plugin-*`
    /// bin; `requested_bin` disambiguates when a repo has several) and `cargo build --release
    /// --locked` it. A repo that is not a flux plugin is a clean, actionable `Err` — never a raw
    /// cargo dump.
    async fn build(&self, clone_dir: &Path, requested_bin: Option<&str>) -> Result<BuiltPlugin>;
}

/// The static inputs of a `--git` source install — the descriptor store, the clone cache root
/// (`~/.flux/plugins/src`), the versioned binary store (`~/.flux/plugins/bin`), and the injected
/// [`SourceBuilder`].
pub struct GitInstallRequest<'a> {
    pub builder: &'a dyn SourceBuilder,
    /// The descriptor store (`~/.flux/plugins`).
    pub descriptors_dir: &'a Path,
    /// The clone cache root (`~/.flux/plugins/src`); a repo is cloned into `<src_root>/<repo-slug>`.
    pub src_root: &'a Path,
    /// The versioned binary store (`~/.flux/plugins/bin`); the built binary is copied to
    /// `<store_root>/<name>/git-<short-commit>/flux-plugin-<name>` so the descriptor's `program`
    /// is a stable path independent of the mutable clone `target/`.
    pub store_root: &'a Path,
}

/// The outcome of a [`install_from_git`] call.
#[derive(Debug, Clone)]
pub struct GitInstalled {
    pub name: String,
    pub git_url: String,
    pub git_commit: String,
    pub program: PathBuf,
    /// `true` when the same git URL was already installed at this exact resolved commit and its
    /// binary is present — no consent asked, nothing rebuilt (idempotent re-install).
    pub already_installed: bool,
}

/// Derive a bare, filesystem-safe clone-cache directory name from a git URL: the last path segment
/// with any `.git` suffix stripped, and every character outside `[A-Za-z0-9._-]` mapped to `-`.
/// This is only the *cache* directory (`<src_root>/<slug>`); the **registered plugin name** comes
/// from the built binary (`flux-plugin-<name>`), so two bins in one repo share the clone but
/// register under their own names.
fn repo_slug(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(trimmed);
    let last = last.strip_suffix(".git").unwrap_or(last);
    let slug: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        "plugin-src".to_string()
    } else {
        slug
    }
}

/// The first 12 hex chars of a commit sha — the versioned-store sub-directory for a from-source
/// build (`<store_root>/<name>/git-<short>/…`), so builds at different commits sit side by side.
fn short_commit(commit: &str) -> String {
    commit.chars().take(12).collect()
}

/// `flux plugin install --git <url> [--tag|--rev|--branch] [--bin <name>] [--force]` (D-87): the
/// third install source. Clone → resolve the commit → (idempotency short-circuit) → **consent
/// gate** → build → register.
///
/// Trust: building arbitrary source is code execution, so `consent` is called with the resolved
/// commit BEFORE anything is built and must return `Ok(true)` to proceed (a non-interactive
/// consent env flag or an interactive confirm — the CLI owns that). An idempotent no-op (same URL +
/// commit already installed, binary present, `!force`) skips the gate entirely — re-confirming to
/// do nothing is noise. The registered descriptor carries `git_url` + `git_commit` and **no**
/// `sha256`, so [`crate::verify_descriptor`] labels it [`crate::Verification::UnverifiedFromSource`].
pub async fn install_from_git<F, Fut>(
    req: &GitInstallRequest<'_>,
    url: &str,
    git_ref: &GitRef,
    requested_bin: Option<&str>,
    force: bool,
    consent: F,
) -> Result<GitInstalled>
where
    F: FnOnce(&str) -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let clone_dir = req.src_root.join(repo_slug(url));
    let commit = req
        .builder
        .clone_and_resolve(url, git_ref, &clone_dir)
        .await?;

    // Idempotency (D-87 acceptance): re-installing the same resolved commit is a no-op. Match by
    // provenance — any installed descriptor from this same `git_url` at this same commit whose
    // binary is still on disk — so it holds regardless of which name the bin registered under, and
    // never rebuilds or re-prompts. `--force` skips the short-circuit and rebuilds.
    if !force {
        for d in crate::discover(req.descriptors_dir) {
            if d.descriptor.git_url.as_deref() == Some(url)
                && d.descriptor.git_commit.as_deref() == Some(commit.as_str())
                && Path::new(&d.descriptor.program).exists()
            {
                return Ok(GitInstalled {
                    name: d.name,
                    git_url: url.to_string(),
                    git_commit: commit,
                    program: PathBuf::from(d.descriptor.program),
                    already_installed: true,
                });
            }
        }
    }

    // Trust gate: disclose the commit and get explicit consent before building unverified source.
    if !consent(&commit).await? {
        return Err(Error::Other(format!(
            "declined: building `{url}` at commit {commit} was not confirmed — nothing built or \
             registered (re-run and confirm, or set the non-interactive consent flag)"
        )));
    }

    let built = req.builder.build(&clone_dir, requested_bin).await?;
    let name = built
        .bin_name
        .strip_prefix("flux-plugin-")
        .unwrap_or(&built.bin_name)
        .to_string();
    crate::invalid_plugin_name(&name).map_err(|_| {
        Error::Other(format!(
            "built binary `{}` yields an invalid plugin name `{name}`",
            built.bin_name
        ))
    })?;

    // Cache the built binary in the versioned store so the descriptor's `program` is a stable path
    // (the clone's `target/release` is overwritten by the next build at a different ref). Keyed by
    // short commit → builds at different commits sit side by side.
    let dest_dir = req
        .store_root
        .join(&name)
        .join(format!("git-{}", short_commit(&commit)));
    std::fs::create_dir_all(&dest_dir).map_err(Error::Io)?;
    let dest_path = dest_dir.join(&built.bin_name);
    std::fs::copy(&built.binary, &dest_path).map_err(Error::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(0o755))
            .map_err(Error::Io)?;
    }

    // Preserve an operator's `args` across a rebuild of the same plugin name.
    let prior_args = crate::load_descriptor(req.descriptors_dir, &name)?
        .map(|d| d.args)
        .unwrap_or_default();
    crate::add_descriptor(
        req.descriptors_dir,
        &name,
        &crate::PluginDescriptor {
            program: dest_path.to_string_lossy().into_owned(),
            args: prior_args,
            git_url: Some(url.to_string()),
            git_commit: Some(commit.clone()),
            ..Default::default()
        },
    )?;

    Ok(GitInstalled {
        name,
        git_url: url.to_string(),
        git_commit: commit,
        program: dest_path,
        already_installed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const TEST_TARGET: &str = "x86_64-unknown-linux-gnu";
    const TEST_TAG: &str = "plugins-v0.9.0";

    /// A hermetic in-memory [`Fetcher`]: serves fixed release tags + asset bytes, and records
    /// every fetch call so tests can prove "nothing was downloaded twice" (idempotent re-install)
    /// without any network access.
    struct MockFetcher {
        tags: Vec<String>,
        assets: HashMap<(String, String), Vec<u8>>,
        fetch_log: Mutex<Vec<(String, String)>>,
    }

    impl MockFetcher {
        fn new(tags: Vec<&str>) -> Self {
            Self {
                tags: tags.into_iter().map(String::from).collect(),
                assets: HashMap::new(),
                fetch_log: Mutex::new(Vec::new()),
            }
        }

        fn with_asset(mut self, tag: &str, asset: &str, bytes: Vec<u8>) -> Self {
            self.assets
                .insert((tag.to_string(), asset.to_string()), bytes);
            self
        }

        fn fetch_count(&self, tag: &str, asset: &str) -> usize {
            self.fetch_log
                .lock()
                .unwrap()
                .iter()
                .filter(|(t, a)| t == tag && a == asset)
                .count()
        }
    }

    #[async_trait::async_trait]
    impl Fetcher for MockFetcher {
        async fn list_release_tags(&self, _repo: &str) -> Result<Vec<String>> {
            Ok(self.tags.clone())
        }

        async fn fetch_release_asset(
            &self,
            _repo: &str,
            tag: &str,
            asset: &str,
        ) -> Result<Vec<u8>> {
            self.fetch_log
                .lock()
                .unwrap()
                .push((tag.to_string(), asset.to_string()));
            self.assets
                .get(&(tag.to_string(), asset.to_string()))
                .cloned()
                .ok_or_else(|| Error::Other(format!("mock: no asset `{asset}` for tag `{tag}`")))
        }
    }

    fn test_keypair() -> minisign::KeyPair {
        minisign::KeyPair::generate_unencrypted_keypair().unwrap()
    }

    fn sign(kp: &minisign::KeyPair, bytes: &[u8]) -> String {
        minisign::sign(
            Some(&kp.pk),
            &kp.sk,
            std::io::Cursor::new(bytes),
            None,
            None,
        )
        .unwrap()
        .into_string()
    }

    /// Build a `.tar.xz` fixture containing exactly one entry — the same shape D-46's release
    /// workflow packages (`tar -cJf … -C <bindir> flux-plugin-<name>`), just produced in pure Rust
    /// so tests never shell out.
    fn tar_xz_fixture(exe: &str, contents: &[u8]) -> Vec<u8> {
        let mut tarbuf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tarbuf);
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, exe, contents).unwrap();
            builder.finish().unwrap();
        }
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(&tarbuf), &mut xz).unwrap();
        xz
    }

    fn zip_fixture(exe: &str, contents: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file(exe, opts).unwrap();
            std::io::Write::write_all(&mut zw, contents).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    /// A unique scratch dir per test — never `~/.flux` (D-19/D-35 test convention).
    fn scratch(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "flux-plugin-pack-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let descriptors_dir = base.join("plugins");
        let store_root = base.join("plugins-bin");
        std::fs::create_dir_all(&descriptors_dir).unwrap();
        (descriptors_dir, store_root)
    }

    /// A valid, correctly signed one-plugin index + matching archive fixture. Individual tests
    /// tamper with one piece of this bundle to exercise a specific failure mode.
    struct Fixture {
        kp: minisign::KeyPair,
        index_bytes: Vec<u8>,
        sig_text: String,
        archive: Vec<u8>,
        contents: Vec<u8>,
        asset_name: String,
    }

    fn build_fixture() -> Fixture {
        let kp = test_keypair();
        let contents = b"pretend-flux-plugin-alpha-elf-bytes".to_vec();
        let archive = tar_xz_fixture("flux-plugin-alpha", &contents);
        let archive_sha = sha256_hex(&archive);
        let asset_name = "flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz".to_string();
        let index_json = serde_json::json!({
            "schema": 1,
            "pack_version": "0.9.0",
            "protocol": crate::PROTOCOL,
            "released_at": "2026-07-03T00:00:00Z",
            "plugins": {
                "alpha": {
                    "version": "0.9.0",
                    "artifacts": {
                        TEST_TARGET: {
                            "asset": asset_name,
                            "sha256": archive_sha,
                            "size": archive.len(),
                        }
                    }
                }
            }
        });
        let index_bytes = serde_json::to_vec_pretty(&index_json).unwrap();
        let sig_text = sign(&kp, &index_bytes);
        Fixture {
            kp,
            index_bytes,
            sig_text,
            archive,
            contents,
            asset_name,
        }
    }

    fn fetcher_for(f: &Fixture) -> MockFetcher {
        MockFetcher::new(vec![TEST_TAG])
            .with_asset(TEST_TAG, "plugins-index.json", f.index_bytes.clone())
            .with_asset(
                TEST_TAG,
                "plugins-index.json.minisig",
                f.sig_text.clone().into_bytes(),
            )
            .with_asset(TEST_TAG, &f.asset_name, f.archive.clone())
    }

    fn req<'a>(
        fetcher: &'a dyn Fetcher,
        descriptors_dir: &'a Path,
        store_root: &'a Path,
        public_key: &'a str,
    ) -> InstallRequest<'a> {
        InstallRequest {
            fetcher,
            repo: DEFAULT_REPO,
            public_key,
            descriptors_dir,
            store_root,
            target: TEST_TARGET,
        }
    }

    /// A second signed release (same keypair — one `InstallRequest.public_key` verifies both) so
    /// pin/rollback tests can switch between side-by-side store versions.
    fn versioned_release(
        kp: &minisign::KeyPair,
        version: &str,
        contents: &[u8],
    ) -> (Vec<u8>, String, Vec<u8>, String) {
        let archive = tar_xz_fixture("flux-plugin-alpha", contents);
        let asset_name = format!("flux-plugin-alpha-{version}-x86_64-unknown-linux-gnu.tar.xz");
        let index_json = serde_json::json!({
            "schema": 1,
            "pack_version": version,
            "protocol": crate::PROTOCOL,
            "released_at": "2026-07-03T00:00:00Z",
            "plugins": {
                "alpha": {
                    "version": version,
                    "artifacts": {
                        TEST_TARGET: {
                            "asset": asset_name,
                            "sha256": sha256_hex(&archive),
                            "size": archive.len(),
                        }
                    }
                }
            }
        });
        let index_bytes = serde_json::to_vec_pretty(&index_json).unwrap();
        let sig = sign(kp, &index_bytes);
        (index_bytes, sig, archive, asset_name)
    }

    /// D-48 acceptance: `pin` is a verified version switch — an already-stored version is
    /// repointed OFFLINE (no re-fetch), the hash + `previous` are recorded, and a version the
    /// index does not offer fails cleanly.
    #[tokio::test]
    async fn pin_switches_descriptor_to_stored_version() {
        let kp = test_keypair();
        let (idx8, sig8, arc8, asset8) = versioned_release(&kp, "0.8.0", b"alpha-bytes-0.8.0");
        let (idx9, sig9, arc9, asset9) = versioned_release(&kp, "0.9.0", b"alpha-bytes-0.9.0");
        let fetcher = MockFetcher::new(vec!["plugins-v0.8.0", "plugins-v0.9.0"])
            .with_asset("plugins-v0.8.0", "plugins-index.json", idx8)
            .with_asset(
                "plugins-v0.8.0",
                "plugins-index.json.minisig",
                sig8.into_bytes(),
            )
            .with_asset("plugins-v0.8.0", &asset8, arc8)
            .with_asset("plugins-v0.9.0", "plugins-index.json", idx9)
            .with_asset(
                "plugins-v0.9.0",
                "plugins-index.json.minisig",
                sig9.into_bytes(),
            )
            .with_asset("plugins-v0.9.0", &asset9, arc9);
        let (descriptors_dir, store_root) = scratch("pin");
        let pk = kp.pk.to_base64();
        let r = req(&fetcher, &descriptors_dir, &store_root, &pk);

        // Plain install resolves the newest release (0.9.0) — and records the hash sidecar.
        install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap();
        let stored9 = stored_binary_path(&store_root, "alpha", "0.9.0", TEST_TARGET);
        assert_eq!(
            std::fs::read_to_string(sidecar_path(&stored9)).unwrap(),
            sha256_hex(b"alpha-bytes-0.9.0"),
            "install records the hash sidecar beside the stored binary"
        );

        // Pin to 0.8.0 — not in the store yet, so it goes through the verified fetch path.
        let out = pin(&r, "alpha", "0.8.0").await.unwrap();
        assert!(out.fetched, "0.8.0 was not in the store — fetched");
        assert_eq!(out.previous.as_deref(), Some("0.9.0"));
        let d = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        let stored8 = stored_binary_path(&store_root, "alpha", "0.8.0", TEST_TARGET);
        assert_eq!(d.program, stored8.to_string_lossy());
        assert_eq!(d.version.as_deref(), Some("0.8.0"));
        assert_eq!(d.pinned.as_deref(), Some("0.8.0"));
        assert_eq!(
            d.sha256.as_deref(),
            Some(sha256_hex(b"alpha-bytes-0.8.0").as_str())
        );
        assert_eq!(d.previous.as_deref(), Some("0.9.0"));

        // Pin back to 0.9.0 — already stored side-by-side: an OFFLINE repoint, nothing re-fetched.
        let before = fetcher.fetch_count("plugins-v0.9.0", &asset9);
        let out = pin(&r, "alpha", "0.9.0").await.unwrap();
        assert!(!out.fetched, "0.9.0 was in the store — offline repoint");
        assert_eq!(
            fetcher.fetch_count("plugins-v0.9.0", &asset9),
            before,
            "the archive was not re-downloaded for an offline pin"
        );
        let d = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        assert_eq!(d.program, stored9.to_string_lossy());
        assert_eq!(d.version.as_deref(), Some("0.9.0"));
        assert_eq!(d.pinned.as_deref(), Some("0.9.0"));
        assert_eq!(
            d.sha256.as_deref(),
            Some(sha256_hex(b"alpha-bytes-0.9.0").as_str())
        );
        assert_eq!(d.previous.as_deref(), Some("0.8.0"));

        // A version no release offers fails cleanly (nothing repointed).
        let err = pin(&r, "alpha", "0.7.0").await.unwrap_err().to_string();
        assert!(
            err.contains("0.7.0"),
            "clean error names the version: {err}"
        );
        let d = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        assert_eq!(d.version.as_deref(), Some("0.9.0"), "descriptor untouched");

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    /// D-48 acceptance: `rollback` flips to `previous` offline — by construction (no fetcher in
    /// scope), reading only the side-by-side store + hash sidecars. Current/previous swap, so a
    /// second rollback is the round trip; no `previous` and a sidecar-less store entry are clean,
    /// explanatory refusals.
    #[test]
    fn rollback_flips_to_previous_version_offline() {
        let (descriptors_dir, store_root) = scratch("rollback");
        let seed = |version: &str, bytes: &[u8]| {
            let p = stored_binary_path(&store_root, "alpha", version, TEST_TARGET);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes).unwrap();
            std::fs::write(sidecar_path(&p), sha256_hex(bytes)).unwrap();
            p
        };
        let p8 = seed("0.8.0", b"alpha-bytes-0.8.0");
        let p9 = seed("0.9.0", b"alpha-bytes-0.9.0");
        crate::add_descriptor(
            &descriptors_dir,
            "alpha",
            &crate::PluginDescriptor {
                program: p9.to_string_lossy().into_owned(),
                args: vec!["--flag".into()],
                pinned: Some("0.9.0".into()),
                version: Some("0.9.0".into()),
                sha256: Some(sha256_hex(b"alpha-bytes-0.9.0")),
                source: Some("plugins-v0.9.0".into()),
                previous: Some("0.8.0".into()),
                git_url: None,
                git_commit: None,
                capabilities: None,
                origin: None,
            },
        )
        .unwrap();

        let out = rollback(&descriptors_dir, &store_root, TEST_TARGET, "alpha").unwrap();
        assert_eq!(out.from.as_deref(), Some("0.9.0"));
        assert_eq!(out.to, "0.8.0");
        let d = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        assert_eq!(d.program, p8.to_string_lossy());
        assert_eq!(d.version.as_deref(), Some("0.8.0"));
        assert_eq!(d.pinned.as_deref(), Some("0.8.0"));
        assert_eq!(
            d.sha256.as_deref(),
            Some(sha256_hex(b"alpha-bytes-0.8.0").as_str())
        );
        assert_eq!(d.previous.as_deref(), Some("0.9.0"), "versions swapped");
        assert_eq!(d.args, vec!["--flag"], "operator args survive the flip");

        // The swap makes a second rollback the round trip.
        let out = rollback(&descriptors_dir, &store_root, TEST_TARGET, "alpha").unwrap();
        assert_eq!(out.to, "0.9.0");
        assert_eq!(out.from.as_deref(), Some("0.8.0"));

        // No `previous` → a clean error explaining the new semantics.
        let mut d = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        d.previous = None;
        crate::add_descriptor(&descriptors_dir, "alpha", &d).unwrap();
        let err = rollback(&descriptors_dir, &store_root, TEST_TARGET, "alpha")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no previous version"), "{err}");
        assert!(
            err.contains("pin"),
            "the error names the explicit alternative: {err}"
        );

        // A pre-sidecar store entry refuses rather than blessing unverified bytes.
        d.previous = Some("0.8.0".into());
        crate::add_descriptor(&descriptors_dir, "alpha", &d).unwrap();
        std::fs::remove_file(sidecar_path(&p8)).unwrap();
        let err = rollback(&descriptors_dir, &store_root, TEST_TARGET, "alpha")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no recorded hash"), "{err}");

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    /// D-48: `purge_store` removes exactly the plugin's own store directory (and reports a
    /// missing one as `false`, not an error).
    #[test]
    fn purge_store_removes_the_plugin_dir_only() {
        let (descriptors_dir, store_root) = scratch("purge");
        let p = stored_binary_path(&store_root, "alpha", "0.9.0", TEST_TARGET);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"bytes").unwrap();
        let other = stored_binary_path(&store_root, "beta", "0.9.0", TEST_TARGET);
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"bytes").unwrap();

        assert!(purge_store(&store_root, "alpha").unwrap());
        assert!(!store_root.join("alpha").exists());
        assert!(other.is_file(), "another plugin's store is untouched");
        assert!(
            !purge_store(&store_root, "alpha").unwrap(),
            "already gone → false"
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn remote_install_writes_versioned_store_and_descriptor() {
        let f = build_fixture();
        let fetcher = fetcher_for(&f);
        let (descriptors_dir, store_root) = scratch("happy");

        let pk = f.kp.pk.to_base64();
        let r = req(&fetcher, &descriptors_dir, &store_root, &pk);
        let installed = install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap();
        assert_eq!(installed.len(), 1);
        let alpha = &installed[0];
        assert_eq!(alpha.name, "alpha");
        assert_eq!(alpha.version, "0.9.0");
        assert_eq!(alpha.source, TEST_TAG);
        assert!(!alpha.already_installed);
        let expected_bin_sha = sha256_hex(&f.contents);
        assert_eq!(alpha.sha256, expected_bin_sha);

        // The versioned store layout: ~/.flux/plugins/bin/<name>/<version>/flux-plugin-<name>.
        let expected_path = store_root
            .join("alpha")
            .join("0.9.0")
            .join("flux-plugin-alpha");
        assert_eq!(alpha.program, expected_path);
        let on_disk = std::fs::read(&expected_path).unwrap();
        assert_eq!(on_disk, f.contents, "the unpacked binary is byte-exact");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&expected_path)
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "the installed binary is executable");
        }

        // The descriptor: version/sha256/source populated, program points at the store.
        let desc = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        assert_eq!(desc.program, expected_path.to_string_lossy());
        assert_eq!(desc.version.as_deref(), Some("0.9.0"));
        assert_eq!(desc.sha256.as_deref(), Some(expected_bin_sha.as_str()));
        assert_eq!(desc.source.as_deref(), Some(TEST_TAG));

        // Re-installing the same version is an idempotent no-op: no second archive fetch.
        assert_eq!(fetcher.fetch_count(TEST_TAG, &f.asset_name), 1);
        let installed_again = install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap();
        assert!(installed_again[0].already_installed);
        assert_eq!(
            fetcher.fetch_count(TEST_TAG, &f.asset_name),
            1,
            "the archive was not re-downloaded on an idempotent re-install"
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn remote_install_refuses_bad_index_signature() {
        let f = build_fixture();
        // Tamper the index AFTER signing: the signature on file no longer matches.
        let mut tampered_index = f.index_bytes.clone();
        tampered_index.extend_from_slice(b" ");
        let fetcher = MockFetcher::new(vec![TEST_TAG])
            .with_asset(TEST_TAG, "plugins-index.json", tampered_index)
            .with_asset(
                TEST_TAG,
                "plugins-index.json.minisig",
                f.sig_text.clone().into_bytes(),
            )
            .with_asset(TEST_TAG, &f.asset_name, f.archive.clone());
        let (descriptors_dir, store_root) = scratch("bad-sig");

        let pk = f.kp.pk.to_base64();
        let r = req(&fetcher, &descriptors_dir, &store_root, &pk);
        let err = install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("signature"),
            "error names the signature failure: {err}"
        );

        // Fail-closed: nothing written at all.
        assert!(crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .is_none());
        assert!(!store_root.exists(), "no store directory was created");
        assert_eq!(
            fetcher.fetch_count(TEST_TAG, &f.asset_name),
            0,
            "the archive was never even fetched — the index never became trusted"
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn remote_install_refuses_checksum_mismatch() {
        let f = build_fixture();
        let mut tampered_archive = f.archive.clone();
        tampered_archive.extend_from_slice(b"\0\0\0\0tampered");
        let fetcher = MockFetcher::new(vec![TEST_TAG])
            .with_asset(TEST_TAG, "plugins-index.json", f.index_bytes.clone())
            .with_asset(
                TEST_TAG,
                "plugins-index.json.minisig",
                f.sig_text.clone().into_bytes(),
            )
            .with_asset(TEST_TAG, &f.asset_name, tampered_archive);
        let (descriptors_dir, store_root) = scratch("bad-checksum");

        let pk = f.kp.pk.to_base64();
        let r = req(&fetcher, &descriptors_dir, &store_root, &pk);
        let err = install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("checksum"),
            "error names the checksum failure: {err}"
        );

        // Fail-closed: nothing was made executable, no descriptor exists.
        assert!(crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .is_none());
        let expected_path = store_root
            .join("alpha")
            .join("0.9.0")
            .join("flux-plugin-alpha");
        assert!(
            !expected_path.exists(),
            "the tampered archive was never unpacked"
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn install_refuses_protocol_mismatch() {
        let kp = test_keypair();
        let index_json = serde_json::json!({
            "schema": 1,
            "pack_version": "0.9.0",
            "protocol": "flux.plugin.v2",
            "released_at": "2026-07-03T00:00:00Z",
            "plugins": {
                "alpha": {
                    "version": "0.9.0",
                    "artifacts": {
                        TEST_TARGET: {
                            "asset": "flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz",
                            "sha256": "deadbeef",
                            "size": 4,
                        }
                    }
                }
            }
        });
        let index_bytes = serde_json::to_vec_pretty(&index_json).unwrap();
        let sig_text = sign(&kp, &index_bytes);
        let fetcher = MockFetcher::new(vec![TEST_TAG])
            .with_asset(TEST_TAG, "plugins-index.json", index_bytes)
            .with_asset(
                TEST_TAG,
                "plugins-index.json.minisig",
                sig_text.into_bytes(),
            );
        let (descriptors_dir, store_root) = scratch("protocol-mismatch");

        let pk = kp.pk.to_base64();
        let r = req(&fetcher, &descriptors_dir, &store_root, &pk);
        let err = install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("flux.plugin.v2"), "{msg}");
        assert!(msg.contains(crate::PROTOCOL), "{msg}");

        // The mismatch is caught before any archive is even requested.
        assert_eq!(
            fetcher.fetch_count(
                TEST_TAG,
                "flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz"
            ),
            0
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    /// The embedded production key must actually parse — a typo here would silently make every
    /// real install fail closed (safe, but not what a release should ship).
    #[test]
    fn embedded_public_key_parses() {
        minisign_verify::PublicKey::from_base64(PUBLIC_KEY)
            .expect("PUBLIC_KEY must be a valid minisign public key");
    }

    #[test]
    fn index_assets_are_bare_names_never_urls() {
        for bad in [
            "https://evil.example/flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz",
            "flux-plugin-alpha/../../evil-0.9.0-x86_64-unknown-linux-gnu.tar.xz",
            "../flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz",
            "/etc/passwd",
            "not-flux-plugin-prefixed-0.9.0-x86_64-unknown-linux-gnu.tar.xz",
        ] {
            assert!(
                validate_plugin_asset_name(bad).is_err(),
                "`{bad}` must be rejected"
            );
        }
        assert!(validate_plugin_asset_name(
            "flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz"
        )
        .is_ok());
    }

    #[tokio::test]
    async fn remote_install_refuses_url_shaped_asset_end_to_end() {
        // A validly-signed index whose content is malicious (a compromised generation step, not a
        // forged signature) — the URL-shaped asset must still be rejected before any download.
        let kp = test_keypair();
        let index_json = serde_json::json!({
            "schema": 1,
            "pack_version": "0.9.0",
            "protocol": crate::PROTOCOL,
            "released_at": "2026-07-03T00:00:00Z",
            "plugins": {
                "alpha": {
                    "version": "0.9.0",
                    "artifacts": {
                        TEST_TARGET: {
                            "asset": "https://evil.example/flux-plugin-alpha-0.9.0-x86_64-unknown-linux-gnu.tar.xz",
                            "sha256": "deadbeef",
                            "size": 4,
                        }
                    }
                }
            }
        });
        let index_bytes = serde_json::to_vec_pretty(&index_json).unwrap();
        let sig_text = sign(&kp, &index_bytes);
        let fetcher = MockFetcher::new(vec![TEST_TAG])
            .with_asset(TEST_TAG, "plugins-index.json", index_bytes)
            .with_asset(
                TEST_TAG,
                "plugins-index.json.minisig",
                sig_text.into_bytes(),
            );
        let (descriptors_dir, store_root) = scratch("url-asset");

        let pk = kp.pk.to_base64();
        let r = req(&fetcher, &descriptors_dir, &store_root, &pk);
        let err = install_many(&r, &["alpha".to_string()], false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bare file name"), "{err}");
        assert!(crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .is_none());

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    #[test]
    fn unpack_single_binary_reads_windows_zip_archives() {
        let contents = b"pretend-windows-plugin-bytes";
        let archive = zip_fixture("flux-plugin-demo.exe", contents);
        let dest =
            std::env::temp_dir().join(format!("flux-plugin-pack-zip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let bytes = unpack_single_binary(&archive, true, "flux-plugin-demo.exe", &dest).unwrap();
        assert_eq!(bytes, contents);
        assert_eq!(
            std::fs::read(dest.join("flux-plugin-demo.exe")).unwrap(),
            contents
        );
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn generated_pack_archives_are_total_and_write_only_after_complete_extraction() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                let mut value = self.0;
                value ^= value >> 12;
                value ^= value << 25;
                value ^= value >> 27;
                self.0 = value;
                value.wrapping_mul(0x2545_F491_4F6C_DD1D)
            }
        }

        let cases = std::env::var("FLUX_ADVERSARIAL_CASES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(64usize)
            .clamp(1, 512);
        let unix_exe = "flux-plugin-corpus";
        let windows_exe = "flux-plugin-corpus.exe";
        let contents = b"committed-secret-free-pack-corpus";
        let archives = [
            (false, unix_exe, tar_xz_fixture(unix_exe, contents)),
            (true, windows_exe, zip_fixture(windows_exe, contents)),
        ];

        let root = std::env::temp_dir().join(format!(
            "flux-plugin-pack-adversarial-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        // A temporary known-bad fixture proves the extraction oracle itself is live: corrupt bytes
        // must fail without creating the destination directory.
        let known_bad_dest = root.join("known-bad");
        assert!(unpack_single_binary(&[1, 2, 3], false, unix_exe, &known_bad_dest).is_err());
        assert!(!known_bad_dest.exists());

        let mut rng = Rng(0xC264_A2C4_1E00_0001);
        for case in 0..cases {
            let (windows, exe, seed) = &archives[case % archives.len()];
            let mut candidate = seed.clone();
            let recipe = if case % 3 == 0 {
                let cut = rng.next() as usize % candidate.len();
                candidate.truncate(cut);
                format!("truncate:{cut}")
            } else {
                let junk_len = 1 + rng.next() as usize % 8;
                candidate.extend((0..junk_len).map(|_| rng.next() as u8));
                format!("append:{junk_len}")
            };

            let dest = root.join(format!("case-{case}"));
            let outcome =
                std::panic::catch_unwind(|| unpack_single_binary(&candidate, *windows, exe, &dest));
            let result = outcome.unwrap_or_else(|_| {
                panic!(
                    "pack decoder panicked; reproduce with case={case}, format={}, recipe={recipe}",
                    if *windows { "zip" } else { "tar.xz" }
                )
            });
            match result {
                Ok(bytes) => {
                    // Appended bytes may leave a valid archive. A successful decode must remain
                    // bounded and materialize exactly the returned single entry.
                    assert!(
                        bytes.len() <= 4096,
                        "case {case}: extraction exceeded corpus bound"
                    );
                    assert_eq!(std::fs::read(dest.join(exe)).unwrap(), bytes, "case {case}");
                    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1, "case {case}");
                }
                Err(_) => assert!(
                    !dest.exists(),
                    "case {case}: failed extraction left filesystem output ({recipe})"
                ),
            }
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolve_release_tag_picks_highest_semver_from_plugins_v_tags() {
        struct StaticTags(Vec<&'static str>);
        #[async_trait::async_trait]
        impl Fetcher for StaticTags {
            async fn list_release_tags(&self, _repo: &str) -> Result<Vec<String>> {
                Ok(self.0.iter().map(|s| s.to_string()).collect())
            }
            async fn fetch_release_asset(
                &self,
                _repo: &str,
                _tag: &str,
                _asset: &str,
            ) -> Result<Vec<u8>> {
                unreachable!("not exercised by this test")
            }
        }
        let fetcher = StaticTags(vec![
            "plugins-v0.1.0",
            "v0.9.9",          // core release tag — must be ignored
            "plugins-v0.10.0", // numerically > 0.9.0, lexicographically < it
            "plugins-v0.9.0",
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tag = rt
            .block_on(resolve_release_tag(&fetcher, DEFAULT_REPO, None))
            .unwrap();
        assert_eq!(tag, "plugins-v0.10.0");

        let tag = rt
            .block_on(resolve_release_tag(&fetcher, DEFAULT_REPO, Some("0.3.0")))
            .unwrap();
        assert_eq!(
            tag, "plugins-v0.3.0",
            "an explicit version needs no listing call"
        );
    }

    // -----------------------------------------------------------------------
    // D-87 `--git` source install — hermetic (a fake clone+build, no network, no cargo)
    // -----------------------------------------------------------------------

    /// A fake [`SourceBuilder`]: "clones" by creating the dir + returning a fixed commit, and
    /// "builds" by writing a stub `flux-plugin-alpha` into `target/release`. Records call counts so
    /// a test can prove an idempotent re-install rebuilt NOTHING and `--force` rebuilt. Optionally
    /// fails the build to model a repo that is not a flux plugin.
    struct FakeSourceBuilder {
        commit: String,
        build_error: Option<String>,
        clone_calls: Mutex<usize>,
        build_calls: Mutex<usize>,
    }

    impl FakeSourceBuilder {
        fn new(commit: &str) -> Self {
            Self {
                commit: commit.to_string(),
                build_error: None,
                clone_calls: Mutex::new(0),
                build_calls: Mutex::new(0),
            }
        }

        fn failing(commit: &str, err: &str) -> Self {
            Self {
                build_error: Some(err.to_string()),
                ..Self::new(commit)
            }
        }
    }

    #[async_trait::async_trait]
    impl SourceBuilder for FakeSourceBuilder {
        async fn clone_and_resolve(
            &self,
            _url: &str,
            _git_ref: &GitRef,
            clone_dir: &Path,
        ) -> Result<String> {
            *self.clone_calls.lock().unwrap() += 1;
            std::fs::create_dir_all(clone_dir).map_err(Error::Io)?;
            Ok(self.commit.clone())
        }

        async fn build(
            &self,
            clone_dir: &Path,
            _requested_bin: Option<&str>,
        ) -> Result<BuiltPlugin> {
            *self.build_calls.lock().unwrap() += 1;
            if let Some(err) = &self.build_error {
                return Err(Error::Other(err.clone()));
            }
            let rel = clone_dir.join("target").join("release");
            std::fs::create_dir_all(&rel).map_err(Error::Io)?;
            let bin = rel.join("flux-plugin-alpha");
            std::fs::write(&bin, b"pretend-built-plugin-bytes").map_err(Error::Io)?;
            Ok(BuiltPlugin {
                bin_name: "flux-plugin-alpha".to_string(),
                binary: bin,
            })
        }
    }

    fn git_scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base =
            std::env::temp_dir().join(format!("flux-plugin-git-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let descriptors_dir = base.join("plugins");
        let src_root = base.join("src");
        let store_root = base.join("bin");
        std::fs::create_dir_all(&descriptors_dir).unwrap();
        (descriptors_dir, src_root, store_root)
    }

    /// D-87 acceptance: a `--git` install registers a descriptor with git provenance and **no**
    /// signed-pack hash, so `verify_descriptor` labels it `UnverifiedFromSource`; re-installing the
    /// same resolved commit is an idempotent no-op (nothing rebuilt); `--force` rebuilds.
    #[tokio::test]
    async fn git_install_registers_from_source_descriptor_and_is_idempotent() {
        let (descriptors_dir, src_root, store_root) = git_scratch("happy");
        let builder = FakeSourceBuilder::new("abc123def4567890commit");
        let req = GitInstallRequest {
            builder: &builder,
            descriptors_dir: &descriptors_dir,
            src_root: &src_root,
            store_root: &store_root,
        };
        let url = "https://gitlab.example/group/flux-plugin-alpha.git";

        let out = install_from_git(
            &req,
            url,
            &GitRef::Tag("v1".into()),
            None,
            false,
            |_c| async { Ok(true) },
        )
        .await
        .unwrap();
        assert_eq!(out.name, "alpha");
        assert_eq!(out.git_url, url);
        assert_eq!(out.git_commit, "abc123def4567890commit");
        assert!(!out.already_installed);
        assert!(
            out.program.is_file(),
            "the built binary was cached in the store"
        );
        assert_eq!(*builder.build_calls.lock().unwrap(), 1);

        // The descriptor: git provenance recorded, NO sha256 → UnverifiedFromSource (not a signed
        // pack, not a `--dir` local scan). This is the label spawn_verified admits (not HashDrift).
        let desc = crate::load_descriptor(&descriptors_dir, "alpha")
            .unwrap()
            .unwrap();
        assert_eq!(desc.git_url.as_deref(), Some(url));
        assert_eq!(desc.git_commit.as_deref(), Some("abc123def4567890commit"));
        assert!(
            desc.sha256.is_none(),
            "a from-source install records no signed-pack hash"
        );
        assert_eq!(
            crate::verify_descriptor(&desc),
            crate::Verification::UnverifiedFromSource
        );

        // Re-install of the same resolved commit → idempotent no-op, NOTHING rebuilt.
        let again = install_from_git(
            &req,
            url,
            &GitRef::Tag("v1".into()),
            None,
            false,
            |_c| async {
                panic!("consent must NOT be asked for an idempotent no-op");
            },
        )
        .await
        .unwrap();
        assert!(again.already_installed);
        assert_eq!(
            *builder.build_calls.lock().unwrap(),
            1,
            "an idempotent re-install rebuilds nothing"
        );

        // `--force` rebuilds even at the same commit.
        let forced = install_from_git(
            &req,
            url,
            &GitRef::Tag("v1".into()),
            None,
            true,
            |_c| async { Ok(true) },
        )
        .await
        .unwrap();
        assert!(!forced.already_installed);
        assert_eq!(
            *builder.build_calls.lock().unwrap(),
            2,
            "`--force` rebuilds"
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    /// D-87 acceptance: a repo that is not a flux plugin fails with the builder's clear, actionable
    /// error — and nothing is registered.
    #[tokio::test]
    async fn git_install_non_plugin_repo_errors_cleanly() {
        let (descriptors_dir, src_root, store_root) = git_scratch("non-plugin");
        let builder = FakeSourceBuilder::failing(
            "deadbeefcafe",
            "repo is not a flux plugin: no `flux-plugin-*` binary target found",
        );
        let req = GitInstallRequest {
            builder: &builder,
            descriptors_dir: &descriptors_dir,
            src_root: &src_root,
            store_root: &store_root,
        };
        let err = install_from_git(
            &req,
            "https://example/repo.git",
            &GitRef::Default,
            None,
            false,
            |_c| async { Ok(true) },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a flux plugin"), "actionable error: {err}");
        assert!(
            flux_core::Result::unwrap(crate::load_descriptor(&descriptors_dir, "repo")).is_none()
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    /// D-87 trust gate: a declined consent aborts before any build/registration.
    #[tokio::test]
    async fn git_install_declined_consent_aborts_before_build() {
        let (descriptors_dir, src_root, store_root) = git_scratch("declined");
        let builder = FakeSourceBuilder::new("c0ffee123456");
        let req = GitInstallRequest {
            builder: &builder,
            descriptors_dir: &descriptors_dir,
            src_root: &src_root,
            store_root: &store_root,
        };
        let err = install_from_git(
            &req,
            "https://example/flux-plugin-alpha.git",
            &GitRef::Default,
            None,
            false,
            |_c| async { Ok(false) },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("declined"), "{err}");
        assert_eq!(
            *builder.build_calls.lock().unwrap(),
            0,
            "consent declined → nothing built"
        );
        assert!(
            crate::discover(&descriptors_dir).is_empty(),
            "nothing registered"
        );

        std::fs::remove_dir_all(descriptors_dir.parent().unwrap()).ok();
    }

    #[test]
    fn repo_slug_is_a_bare_safe_dir_name() {
        assert_eq!(
            repo_slug("https://gitlab.example/group/flux-plugin-acme-manager.git"),
            "flux-plugin-acme-manager"
        );
        assert_eq!(repo_slug("git@github.com:codewandler/flux.git"), "flux");
        assert_eq!(repo_slug("https://example/x/"), "x");
        // No path separators survive → the cache dir can never escape src_root.
        assert!(!repo_slug("https://example/a/b/../c").contains('/'));
    }

    /// Not a real test — a one-shot operator tool. Run explicitly once:
    /// `cargo test -p flux-plugin --lib pack::tests::generate_pack_keypair -- --ignored --nocapture`
    /// to mint the production keypair: prints the PUBLIC key to embed as [`PUBLIC_KEY`], and writes
    /// the SECRET key to `~/.flux/minisign-pack.key` (mode 0600) — never into the repo. That file's
    /// contents become the `MINISIGN_SECRET_KEY` GitHub Actions secret D-46's
    /// `release-plugins.yml` reads to sign `plugins-index.json` with the real `minisign` CLI.
    #[test]
    #[ignore]
    fn generate_pack_keypair() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().expect("generate keypair");
        let pk_b64 = kp.pk.to_base64();
        let sk_text = kp
            .sk
            .to_box(Some("flux plugin pack signing key (D-46/D-47)"))
            .expect("box secret key")
            .into_string();

        let home = std::env::var_os("HOME").expect("HOME must be set to write the secret key");
        let path = PathBuf::from(home).join(".flux").join("minisign-pack.key");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create ~/.flux");
        std::fs::write(&path, &sk_text).expect("write secret key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 0600");
        }

        eprintln!("PUBLIC KEY (embed as pack::PUBLIC_KEY):\n{pk_b64}");
        eprintln!("secret key written to {} (mode 0600)", path.display());
        eprintln!(
            "add this file's contents as the MINISIGN_SECRET_KEY GitHub Actions secret, then \
             remove any local copies you don't need to keep"
        );
    }
}
