//! pack-index — generate the plugin-pack release index (D-46).
//!
//! Scans a directory of packaged per-plugin per-target archives
//! (`flux-plugin-<name>-<version>-<target>.tar.xz`, `.zip` on windows) and emits
//! `plugins-index.json` per the plugin-distribution design (`schema: 1`): `pack_version`,
//! `protocol`, `released_at`, and per-plugin per-target `asset`/`sha256`/`size` entries.
//!
//! Two hard rules from the design (docs/designs/plugin-distribution.md), both enforced here:
//! - **The index names assets, never URLs.** An `asset` value is a bare file name; the CLI
//!   constructs download URLs only from `(repo, tag, asset-name)`, so a compromised index cannot
//!   redirect a download to another host.
//! - **The index is data, never behavior.** No hooks, no commands — this tool only records file
//!   names, hashes, and sizes.
//!
//! Deterministic by construction: plugin/target maps are ordered, `released_at` is a required
//! input (no clock reads), and re-running over the same directory yields byte-identical output.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

const SCHEMA: u32 = 1;
const DEFAULT_PROTOCOL: &str = "flux.plugin.v1";

/// The five release targets — mirrors the core cargo-dist target set (`dist-workspace.toml`).
const KNOWN_TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
];

#[derive(Serialize)]
struct Index {
    schema: u32,
    pack_version: String,
    protocol: String,
    released_at: String,
    plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Serialize)]
struct PluginEntry {
    version: String,
    artifacts: BTreeMap<String, Artifact>,
}

#[derive(Serialize)]
struct Artifact {
    asset: String,
    sha256: String,
    size: u64,
}

/// Reject anything that is not a bare file name: URLs, absolute paths, path traversal, and
/// separator characters of either flavor. The index must never be able to name a location.
fn validate_asset_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("asset name is empty".into());
    }
    if name.contains("://") || name.contains('/') || name.contains('\\') || name.contains(':') {
        return Err(format!(
            "asset name `{name}` is not a bare file name (URL- or path-shaped values are forbidden)"
        ));
    }
    if name.starts_with('.') {
        return Err(format!("asset name `{name}` must not start with `.`"));
    }
    if !name.starts_with("flux-plugin-") {
        return Err(format!(
            "asset name `{name}` must start with `flux-plugin-`"
        ));
    }
    Ok(())
}

/// Parse `flux-plugin-<name>-<version>-<target>.tar.xz|.zip` into `(name, target)`.
/// `version` is the known pack version, so hyphenated plugin names parse unambiguously.
fn parse_asset(file_name: &str, version: &str) -> Result<(String, String), String> {
    let stem = file_name
        .strip_suffix(".tar.xz")
        .or_else(|| file_name.strip_suffix(".zip"))
        .ok_or_else(|| format!("asset `{file_name}` has neither a .tar.xz nor a .zip suffix"))?;
    let rest = stem
        .strip_prefix("flux-plugin-")
        .ok_or_else(|| format!("asset `{file_name}` does not start with `flux-plugin-`"))?;
    let marker = format!("-{version}-");
    let split = rest
        .find(&marker)
        .ok_or_else(|| format!("asset `{file_name}` does not embed pack version `{version}`"))?;
    let name = &rest[..split];
    let target = &rest[split + marker.len()..];
    if name.is_empty() {
        return Err(format!("asset `{file_name}` has an empty plugin name"));
    }
    if !KNOWN_TARGETS.contains(&target) {
        return Err(format!(
            "asset `{file_name}` names unknown target `{target}` (expected one of {KNOWN_TARGETS:?})"
        ));
    }
    Ok((name.to_string(), target.to_string()))
}

/// Scan `dir` for pack archives and build the index. Every `flux-plugin-*` file must parse (a
/// malformed archive name is a loud error, never a silent skip); other files are ignored so the
/// tool can run over a directory that already holds the index or signature from a prior step.
fn build_index(
    dir: &Path,
    version: &str,
    protocol: &str,
    released_at: &str,
) -> Result<Index, String> {
    let mut plugins: BTreeMap<String, PluginEntry> = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read_dir entry: {e}"))?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !entry.path().is_file() || !file_name.starts_with("flux-plugin-") {
            continue;
        }
        validate_asset_name(&file_name)?;
        let (name, target) = parse_asset(&file_name, version)?;
        let bytes = std::fs::read(entry.path())
            .map_err(|e| format!("read {}: {e}", entry.path().display()))?;
        let sha256 = hex(&Sha256::digest(&bytes));
        let artifact = Artifact {
            asset: file_name.to_string(),
            sha256,
            size: bytes.len() as u64,
        };
        let plugin = plugins.entry(name).or_insert_with(|| PluginEntry {
            version: version.to_string(),
            artifacts: BTreeMap::new(),
        });
        if plugin.artifacts.insert(target.clone(), artifact).is_some() {
            return Err(format!(
                "duplicate artifact for target `{target}` in `{file_name}`"
            ));
        }
    }
    if plugins.is_empty() {
        return Err(format!("no pack archives found in {}", dir.display()));
    }
    Ok(Index {
        schema: SCHEMA,
        pack_version: version.to_string(),
        protocol: protocol.to_string(),
        released_at: released_at.to_string(),
        plugins,
    })
}

/// The release sanity gate: exactly `expect_plugins` plugins, each with exactly `expect_targets`
/// artifacts (asset count = plugins × targets — a missing build leg fails the run here).
fn check_expectations(
    index: &Index,
    expect_plugins: usize,
    expect_targets: usize,
) -> Result<(), String> {
    if index.plugins.len() != expect_plugins {
        return Err(format!(
            "expected {expect_plugins} plugins, found {}: [{}]",
            index.plugins.len(),
            index.plugins.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    for (name, plugin) in &index.plugins {
        if plugin.artifacts.len() != expect_targets {
            return Err(format!(
                "plugin `{name}` has {} artifacts, expected {expect_targets} (targets: [{}])",
                plugin.artifacts.len(),
                plugin
                    .artifacts
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn usage() -> String {
    "usage: pack-index --version <pack_version> --dir <artifacts-dir> --out <file> \
     --released-at <rfc3339> [--protocol <id>] [--expect-plugins <n>] [--expect-targets <n>]"
        .into()
}

fn run() -> Result<(), String> {
    let mut version = None;
    let mut dir = None;
    let mut out = None;
    let mut released_at = None;
    let mut protocol = DEFAULT_PROTOCOL.to_string();
    let mut expect_plugins = None;
    let mut expect_targets = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("missing value for {flag}\n{}", usage()))
        };
        match flag.as_str() {
            "--version" => version = Some(value("--version")?),
            "--dir" => dir = Some(value("--dir")?),
            "--out" => out = Some(value("--out")?),
            "--released-at" => released_at = Some(value("--released-at")?),
            "--protocol" => protocol = value("--protocol")?,
            "--expect-plugins" => {
                expect_plugins = Some(
                    value("--expect-plugins")?
                        .parse::<usize>()
                        .map_err(|e| format!("--expect-plugins: {e}"))?,
                )
            }
            "--expect-targets" => {
                expect_targets = Some(
                    value("--expect-targets")?
                        .parse::<usize>()
                        .map_err(|e| format!("--expect-targets: {e}"))?,
                )
            }
            other => return Err(format!("unknown flag `{other}`\n{}", usage())),
        }
    }
    let version = version.ok_or_else(|| format!("--version is required\n{}", usage()))?;
    let dir = dir.ok_or_else(|| format!("--dir is required\n{}", usage()))?;
    let out = out.ok_or_else(|| format!("--out is required\n{}", usage()))?;
    let released_at =
        released_at.ok_or_else(|| format!("--released-at is required\n{}", usage()))?;

    let index = build_index(Path::new(&dir), &version, &protocol, &released_at)?;
    if let (Some(p), Some(t)) = (expect_plugins, expect_targets) {
        check_expectations(&index, p, t)?;
    }
    let json = serde_json::to_string_pretty(&index).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&out, format!("{json}\n")).map_err(|e| format!("write {out}: {e}"))?;
    eprintln!(
        "pack-index: wrote {out} ({} plugins × {} targets)",
        index.plugins.len(),
        index
            .plugins
            .values()
            .next()
            .map(|p| p.artifacts.len())
            .unwrap_or(0)
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pack-index: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_asterisk_is_absent_from_the_pack_workspace() {
        let plugins = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("pack-index lives in the plugins workspace");
        let workspace = std::fs::read_to_string(plugins.join("Cargo.toml")).unwrap();

        assert!(
            !workspace.lines().any(|line| line.trim() == "\"asterisk\","),
            "Asterisk must not remain a plugin-pack workspace member"
        );
        assert!(
            !plugins.join("asterisk").exists(),
            "the retired plugins/asterisk subtree must be deleted"
        );
    }

    /// A unique scratch dir per test (std-only; the plugins workspace carries no tempfile dep).
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pack-index-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// D-46 acceptance: fixture of two plugins × two targets — asserts the `schema: 1` shape,
    /// hash/size correctness against independently computed values, and that URL-shaped asset
    /// names are rejected.
    #[test]
    fn gen_index_matches_schema_and_hashes() {
        let dir = scratch("gen");
        let fixtures: &[(&str, &[u8])] = &[
            (
                "flux-plugin-alpha-0.1.0-x86_64-unknown-linux-gnu.tar.xz",
                b"alpha-linux-bytes",
            ),
            (
                "flux-plugin-alpha-0.1.0-x86_64-pc-windows-msvc.zip",
                b"alpha-windows-bytes",
            ),
            (
                "flux-plugin-beta-0.1.0-x86_64-unknown-linux-gnu.tar.xz",
                b"beta-linux-bytes",
            ),
            (
                "flux-plugin-beta-0.1.0-x86_64-pc-windows-msvc.zip",
                b"beta-windows",
            ),
        ];
        for (name, bytes) in fixtures {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        // A non-archive file in the dir (e.g. a prior index) is ignored, not an error.
        std::fs::write(dir.join("plugins-index.json"), b"{}").unwrap();

        let index = build_index(&dir, "0.1.0", DEFAULT_PROTOCOL, "2026-07-03T00:00:00Z").unwrap();

        // Schema shape.
        assert_eq!(index.schema, 1);
        assert_eq!(index.pack_version, "0.1.0");
        assert_eq!(index.protocol, "flux.plugin.v1");
        assert_eq!(index.released_at, "2026-07-03T00:00:00Z");
        assert_eq!(
            index.plugins.keys().cloned().collect::<Vec<_>>(),
            ["alpha", "beta"],
            "plugins are keyed by name, deterministically ordered"
        );

        // Hash + size correctness, per artifact, against independently computed values.
        for (file, bytes) in fixtures {
            let (name, target) = parse_asset(file, "0.1.0").unwrap();
            let artifact = &index.plugins[&name].artifacts[&target];
            assert_eq!(artifact.asset, *file, "asset is the bare file name");
            assert_eq!(
                artifact.sha256,
                hex(&Sha256::digest(bytes)),
                "sha256 of {file}"
            );
            assert_eq!(artifact.size, bytes.len() as u64, "size of {file}");
        }
        assert_eq!(index.plugins["alpha"].version, "0.1.0");

        // The serialized form nests exactly per the design's schema example.
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
        let entry = &json["plugins"]["alpha"]["artifacts"]["x86_64-unknown-linux-gnu"];
        assert!(
            entry["asset"].is_string() && entry["sha256"].is_string() && entry["size"].is_u64()
        );

        // URL- and path-shaped asset names are rejected — the index names assets, never URLs.
        for bad in [
            "https://evil.example/flux-plugin-alpha-0.1.0-x86_64-unknown-linux-gnu.tar.xz",
            "flux-plugin-alpha/../../evil-0.1.0-x86_64-unknown-linux-gnu.tar.xz",
            "..\\flux-plugin-alpha-0.1.0-x86_64-unknown-linux-gnu.tar.xz",
            "flux-plugin-a:b-0.1.0-x86_64-unknown-linux-gnu.tar.xz",
        ] {
            assert!(
                validate_asset_name(bad).is_err(),
                "`{bad}` must be rejected"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sanity gate fails the run unless asset count = plugins × targets.
    #[test]
    fn expectation_gate_catches_missing_leg() {
        let dir = scratch("expect");
        std::fs::write(
            dir.join("flux-plugin-alpha-0.1.0-x86_64-unknown-linux-gnu.tar.xz"),
            b"bytes",
        )
        .unwrap();
        let index = build_index(&dir, "0.1.0", DEFAULT_PROTOCOL, "2026-07-03T00:00:00Z").unwrap();
        assert!(check_expectations(&index, 1, 1).is_ok());
        assert!(
            check_expectations(&index, 2, 1).is_err(),
            "missing plugin must fail"
        );
        assert!(
            check_expectations(&index, 1, 5).is_err(),
            "missing build leg must fail"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Malformed pack archives fail loudly; hyphenated plugin names parse unambiguously.
    #[test]
    fn asset_parsing_is_strict() {
        // Hyphenated plugin name: the known version disambiguates.
        let (name, target) = parse_asset(
            "flux-plugin-my-thing-0.1.0-aarch64-apple-darwin.tar.xz",
            "0.1.0",
        )
        .unwrap();
        assert_eq!(
            (name.as_str(), target.as_str()),
            ("my-thing", "aarch64-apple-darwin")
        );
        // Unknown target, wrong version, bad suffix: all loud errors.
        assert!(parse_asset("flux-plugin-a-0.1.0-riscv64-unknown-linux.tar.xz", "0.1.0").is_err());
        assert!(parse_asset("flux-plugin-a-0.2.0-x86_64-apple-darwin.tar.xz", "0.1.0").is_err());
        assert!(parse_asset("flux-plugin-a-0.1.0-x86_64-apple-darwin.tar.gz", "0.1.0").is_err());
    }
}
