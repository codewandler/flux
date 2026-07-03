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

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

    crate::add_descriptor(
        req.descriptors_dir,
        name,
        &crate::PluginDescriptor {
            program: dest_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            pinned: existing.and_then(|d| d.pinned),
            version: Some(entry.version.clone()),
            sha256: Some(binary_sha256.clone()),
            source: Some(tag.to_string()),
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
