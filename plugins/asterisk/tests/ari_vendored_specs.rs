use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SOURCE_TAG: &str = "22.10.1";
const SOURCE_TAG_OBJECT: &str = "4f85d05889cf9fb9c9e2ae44cc3f4a825a74545a";
const SOURCE_COMMIT: &str = "f0e408a7b0d829c85bf15fa4b487870a50cb3000";
const SOURCE_REPOSITORY: &str = "https://github.com/asterisk/asterisk";
const SPEC_DIRECTORY: &str = "ari-22.10.1";

const API_DOCUMENTS: [&str; 11] = [
    "applications.json",
    "asterisk.json",
    "bridges.json",
    "channels.json",
    "deviceStates.json",
    "endpoints.json",
    "events.json",
    "mailboxes.json",
    "playbacks.json",
    "recordings.json",
    "sounds.json",
];

fn spec_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("specs")
        .join(SPEC_DIRECTORY)
}

fn pinned_hashes() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("specs")
        .join(format!("{SPEC_DIRECTORY}.sha256"));
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    raw.lines()
        .filter_map(|raw_line| {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut fields = line.split_whitespace();
            let digest = fields.next().expect("pinned hash line needs a digest");
            let relative = fields.next().expect("pinned hash line needs a path");
            assert!(fields.next().is_none(), "unexpected pinned hash fields");
            assert_eq!(digest.len(), 64, "invalid SHA-256 for {relative}");
            assert!(
                digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid SHA-256 for {relative}"
            );
            assert!(!relative.starts_with('/'), "pinned path must be relative");
            assert!(
                !relative.split('/').any(|component| component == ".."),
                "pinned path must not traverse"
            );
            Some((relative.to_string(), digest.to_string()))
        })
        .collect()
}

fn verify_pinned_bytes(
    relative: &str,
    bytes: &[u8],
    pinned: &BTreeMap<String, String>,
) -> Result<(), String> {
    let expected = pinned
        .get(relative)
        .ok_or_else(|| format!("{relative} has no pinned SHA-256"))?;
    let actual = sha256(bytes);
    if &actual == expected {
        Ok(())
    } else {
        Err(format!(
            "pinned SHA-256 mismatch for {relative}: expected {expected}, got {actual}"
        ))
    }
}

fn read_json(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {} as JSON: {error}", path.display()))
}

fn declared_documents(resources: &Value) -> BTreeSet<String> {
    resources["apis"]
        .as_array()
        .expect("resources.json must declare an APIs array")
        .iter()
        .map(|api| {
            let path = api["path"]
                .as_str()
                .expect("each resources.json API must have a path");
            path.strip_prefix("/api-docs/")
                .and_then(|name| name.strip_suffix(".{format}"))
                .map(|name| format!("{name}.json"))
                .unwrap_or_else(|| panic!("unexpected resources.json API path {path:?}"))
        })
        .collect()
}

fn actual_documents(root: &Path) -> BTreeSet<String> {
    fs::read_dir(root.join("api-docs"))
        .unwrap_or_else(|error| panic!("read {}: {error}", root.join("api-docs").display()))
        .map(|entry| entry.expect("read api-docs entry"))
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .map(|entry| {
            entry
                .file_name()
                .into_string()
                .expect("API document names must be UTF-8")
        })
        .collect()
}

#[test]
fn vendored_contract_has_the_exact_official_inventory_and_census() {
    let root = spec_root();
    let resources = read_json(&root.join("resources.json"));
    let expected = API_DOCUMENTS
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();

    assert_eq!(resources["apiVersion"], "10.0.0");
    assert_eq!(resources["swaggerVersion"], "1.1");
    assert_eq!(resources["basePath"], "http://localhost:8088/ari");
    assert_eq!(declared_documents(&resources), expected);
    assert_eq!(actual_documents(&root), expected);

    let mut paths = 0;
    let mut operations = 0;
    let mut models = 0;
    let mut parameters = 0;
    let mut websocket_operations = 0;

    for name in API_DOCUMENTS {
        let document = read_json(&root.join("api-docs").join(name));
        assert!(
            matches!(document["swaggerVersion"].as_str(), Some("1.1" | "1.2")),
            "{name} declares an unsupported Swagger version"
        );
        assert_eq!(
            document["basePath"], "http://localhost:8088/ari",
            "{name} moved the ARI base path"
        );

        let apis = document["apis"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} must declare an APIs array"));
        paths += apis.len();
        models += document["models"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} must declare a models object"))
            .len();

        for api in apis {
            for operation in api["operations"]
                .as_array()
                .unwrap_or_else(|| panic!("{name} API must declare operations"))
            {
                operations += 1;
                parameters += operation["parameters"].as_array().map_or(0, Vec::len);
                if operation["upgrade"].as_str() == Some("websocket") {
                    websocket_operations += 1;
                    assert_eq!(name, "events.json");
                    assert_eq!(api["path"], "/events");
                    assert_eq!(operation["httpMethod"], "GET");
                    assert_eq!(operation["nickname"], "eventWebsocket");
                    assert_eq!(operation["websocketProtocol"], "ari");
                }
            }
        }
    }

    assert_eq!(paths, 76);
    assert_eq!(operations, 109);
    assert_eq!(models, 85);
    assert_eq!(parameters, 275);
    assert_eq!(websocket_operations, 1);
    assert_eq!(operations - websocket_operations, 108);
}

#[derive(Debug, Default)]
struct ProvenanceFile {
    path: String,
    sha256: String,
    bytes: usize,
}

fn toml_string(line: &str, key: &str) -> Option<String> {
    let value = line.strip_prefix(key)?.trim().strip_prefix('=')?.trim();
    Some(
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or_else(|| panic!("{key} must be a quoted TOML string"))
            .to_string(),
    )
}

fn parse_provenance(raw: &str) -> (BTreeMap<String, String>, Vec<ProvenanceFile>) {
    let mut header = BTreeMap::new();
    let mut files = Vec::new();
    let mut current: Option<ProvenanceFile> = None;

    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[files]]" {
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(ProvenanceFile::default());
            continue;
        }
        if let Some(file) = current.as_mut() {
            if let Some(value) = toml_string(line, "path") {
                file.path = value;
            } else if let Some(value) = toml_string(line, "sha256") {
                file.sha256 = value;
            } else if let Some(value) = line.strip_prefix("bytes = ") {
                file.bytes = value.parse().expect("bytes must be an integer");
            } else {
                panic!("unknown provenance file field {line:?}");
            }
        } else if let Some((key, _)) = line.split_once('=') {
            let key = key.trim();
            let value = toml_string(line, key)
                .unwrap_or_else(|| panic!("provenance header {key} must be a string"));
            header.insert(key.to_string(), value);
        } else {
            panic!("unknown provenance line {line:?}");
        }
    }
    if let Some(file) = current {
        files.push(file);
    }
    (header, files)
}

#[test]
fn provenance_pins_the_release_commit_and_every_vendored_byte() {
    let root = spec_root();
    let pinned = pinned_hashes();
    let raw =
        fs::read_to_string(root.join("provenance.toml")).expect("read vendored ARI provenance");
    let (header, files) = parse_provenance(&raw);

    assert_eq!(
        header.get("source_tag").map(String::as_str),
        Some(SOURCE_TAG)
    );
    assert_eq!(
        header.get("source_tag_object").map(String::as_str),
        Some(SOURCE_TAG_OBJECT)
    );
    assert_eq!(
        header.get("source_commit").map(String::as_str),
        Some(SOURCE_COMMIT)
    );
    assert_eq!(
        header.get("source_repository").map(String::as_str),
        Some(SOURCE_REPOSITORY)
    );
    assert_eq!(
        header.get("upstream_license").map(String::as_str),
        Some("GPL-2.0-only")
    );

    let expected_paths = std::iter::once("COPYING".to_string())
        .chain(std::iter::once("resources.json".to_string()))
        .chain(
            API_DOCUMENTS
                .into_iter()
                .map(|name| format!("api-docs/{name}")),
        )
        .collect::<BTreeSet<_>>();
    let recorded_paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(recorded_paths, expected_paths);
    assert_eq!(files.len(), expected_paths.len());
    assert_eq!(
        pinned.keys().cloned().collect::<BTreeSet<_>>(),
        expected_paths
    );

    for file in files {
        assert!(!file.path.starts_with('/'));
        assert!(!file.path.split('/').any(|component| component == ".."));
        let bytes = fs::read(root.join(&file.path))
            .unwrap_or_else(|error| panic!("read vendored {}: {error}", file.path));
        assert_eq!(bytes.len(), file.bytes, "size drift for {}", file.path);
        let expected = pinned
            .get(&file.path)
            .expect("pinned path was checked above");
        assert_eq!(&file.sha256, expected, "provenance drift for {}", file.path);
        verify_pinned_bytes(&file.path, &bytes, &pinned).unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn one_altered_source_byte_is_rejected_by_the_pinned_hashes() {
    let relative = "resources.json";
    let mut bytes = fs::read(spec_root().join(relative)).expect("read resources.json");
    bytes[0] ^= 1;
    let error = verify_pinned_bytes(relative, &bytes, &pinned_hashes())
        .expect_err("one altered byte must not pass the source pin");
    assert!(error.starts_with("pinned SHA-256 mismatch for resources.json:"));
}

#[test]
fn vendored_documents_contain_no_private_or_credential_shaped_examples() {
    let root = spec_root();
    for relative in std::iter::once("resources.json".to_string()).chain(
        API_DOCUMENTS
            .into_iter()
            .map(|name| format!("api-docs/{name}")),
    ) {
        let document = read_json(&root.join(&relative));
        inspect_value(&relative, None, &document);
    }
}

fn inspect_value(document: &str, key: Option<&str>, value: &Value) {
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                assert!(
                    !matches!(child_key.as_str(), "example" | "examples" | "exampleValue"),
                    "{document} contains an upstream example field {child_key:?}"
                );
                inspect_value(document, Some(child_key), child);
            }
        }
        Value::Array(items) => {
            for item in items {
                inspect_value(document, key, item);
            }
        }
        Value::String(text) => inspect_string(document, key, text),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn inspect_string(document: &str, key: Option<&str>, text: &str) {
    if key == Some("_author") {
        return;
    }
    if key == Some("basePath") {
        assert_eq!(
            text, "http://localhost:8088/ari",
            "{document} contains a noncanonical private base path"
        );
        return;
    }

    let lower = text.to_ascii_lowercase();
    for marker in [
        "authorization: basic ",
        "authorization: bearer ",
        "-----begin private key-----",
        "api_key=",
        "apikey=",
        "password=",
        "secret=",
        "token=",
    ] {
        assert!(
            !lower.contains(marker),
            "{document} contains credential-shaped text in {key:?}"
        );
    }
    assert!(
        !text.contains('@'),
        "{document} contains a non-attribution email-like value in {key:?}"
    );
    assert!(
        !contains_phone_number(text),
        "{document} contains a phone-number-shaped value in {key:?}"
    );

    for token in text.split(|character: char| {
        character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | '(' | ')' | ',')
    }) {
        let token = token.trim_end_matches(['.', ';', ':']);
        if token.starts_with("http://") || token.starts_with("https://") {
            assert!(
                token.starts_with("https://docs.asterisk.org/"),
                "{document} contains an unapproved endpoint-like value {token:?}"
            );
        }
        let candidate =
            token.trim_matches(|character: char| !character.is_ascii_digit() && character != '.');
        if let Ok(address) = candidate.parse::<Ipv4Addr>() {
            assert!(
                !(address.is_private() || address.is_loopback() || address.is_link_local()),
                "{document} contains a private IP example in {key:?}"
            );
        }
    }
}

fn contains_phone_number(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let candidate = word.trim_matches(|character: char| {
            !character.is_ascii_digit() && !matches!(character, '+' | '-' | '(' | ')')
        });
        candidate.starts_with('+') && candidate.chars().filter(char::is_ascii_digit).count() >= 7
    })
}

fn sha256(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_len = (input.len() as u64) * 8;
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for block in padded.chunks_exact(64) {
        let mut schedule = [0_u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let first = h
                .wrapping_add(sigma1)
                .wrapping_add(choose)
                .wrapping_add(ROUND[index])
                .wrapping_add(schedule[index]);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = sigma0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut output = String::with_capacity(64);
    for word in state {
        write!(output, "{word:08x}").expect("writing to a String cannot fail");
    }
    output
}

#[test]
fn sha256_test_vector_guards_the_integrity_test_itself() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
