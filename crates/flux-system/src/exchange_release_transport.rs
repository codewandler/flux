//! Closed HTTP transport for Exchange release inputs.
//!
//! This module deliberately stops at bounded opaque bytes. It neither parses provider documents
//! nor claims that transport authenticated them: the C-510 verifier must still apply the provider
//! signatures, signed origin/tag/basename, digests and per-document schema bounds.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{ACCEPT, LOCATION};
use url::Url;

use crate::net::{self, PrivateNetAllow};

const GITHUB_ORIGIN: &str = "https://github.com";
const REPOSITORY_DOWNLOAD: &str = "https://github.com/codewandler/flux-exchange/releases/download";
const CDN_HOST: &str = "release-assets.githubusercontent.com";
const TRUST_TAG: &str = "exchange-trust-v1";
const TRUST_BASENAME: &str = "flux-exchange-release-trust.json";
const CHANNEL_TAG: &str = "exchange-stable-v1";
const CHANNEL_BASENAME: &str = "flux-exchange-release-channel.json";
const MANIFEST_BASENAME: &str = "flux-exchange-release-manifest.json";

const LOCATION_BYTES: usize = 8 * 1024;
const QUERY_BYTES: usize = 6 * 1024;
const QUERY_VALUE_BYTES: usize = 2 * 1024;
const TRUST_BYTES: usize = 64 * 1024;
const METADATA_BYTES: usize = 256 * 1024;
const SIGNATURE_BYTES: usize = 4 * 1024;
const ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
const FETCH_DEADLINE: Duration = Duration::from_secs(120);
// Exchange does not yet define response media types. Request opaque bytes and keep reqwest's
// optional content decoders disabled (the workspace dependency enables no gzip/brotli/deflate/zstd
// feature), so the verifier receives the exact bounded response stream rather than locally
// invented JSON semantics or transparently transformed bytes.
const OPAQUE_MEDIA: &str = "application/octet-stream";

const CDN_QUERY_NAMES: &[&str] = &[
    "jwt",
    "response-content-disposition",
    "response-content-type",
    "rscd",
    "rsct",
    "se",
    "sig",
    "ske",
    "skoid",
    "sks",
    "skt",
    "sktid",
    "skv",
    "sp",
    "spr",
    "sr",
    "sv",
];

/// A closed, value-free refusal from the Exchange release transport.
///
/// Display strings are stable diagnostic codes. Network, URL-parser and operating-system error
/// text is intentionally discarded so lifecycle output cannot accidentally disclose values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRefusal {
    InvalidVersion,
    InvalidKeyId,
    InvalidBasename,
    Network,
    InitialStatus,
    RedirectMissing,
    RedirectInvalid,
    FinalStatus,
    BodyTooLarge,
    Deadline,
}

impl TransportRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidVersion => "exchange_transport_invalid_version",
            Self::InvalidKeyId => "exchange_transport_invalid_key_id",
            Self::InvalidBasename => "exchange_transport_invalid_basename",
            Self::Network => "exchange_transport_network_refused",
            Self::InitialStatus => "exchange_transport_initial_status_refused",
            Self::RedirectMissing => "exchange_transport_redirect_missing",
            Self::RedirectInvalid => "exchange_transport_redirect_refused",
            Self::FinalStatus => "exchange_transport_final_status_refused",
            Self::BodyTooLarge => "exchange_transport_body_too_large",
            Self::Deadline => "exchange_transport_deadline_exceeded",
        }
    }
}

impl fmt::Display for TransportRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for TransportRefusal {}

/// A provider-grammar signing key id used only to derive an exact metadata signature basename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningKeyId(String);

impl SigningKeyId {
    pub fn parse(value: &str) -> Result<Self, TransportRefusal> {
        let bytes = value.as_bytes();
        let valid_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        if bytes.is_empty()
            || bytes.len() > 64
            || !value.is_ascii()
            || !valid_edge(bytes[0])
            || !valid_edge(bytes[bytes.len() - 1])
            || bytes.iter().any(|byte| !valid_edge(*byte) && *byte != b'-')
            || value.contains("--")
        {
            return Err(TransportRefusal::InvalidKeyId);
        }
        Ok(Self(value.to_owned()))
    }
}

/// A provider-grammar stable version obtained from authenticated channel data.
///
/// This is deliberately not an exact-version selector. It only prevents unsafe path construction
/// after the later verifier has selected a signed channel entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StableVersion(String);

impl StableVersion {
    pub fn parse(value: &str) -> Result<Self, TransportRefusal> {
        let mut parts = value.split('.');
        for _ in 0..3 {
            let part = parts.next().ok_or(TransportRefusal::InvalidVersion)?;
            if part.is_empty()
                || part.len() > 9
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(TransportRefusal::InvalidVersion);
            }
        }
        if parts.next().is_some() {
            return Err(TransportRefusal::InvalidVersion);
        }
        Ok(Self(value.to_owned()))
    }

    fn release_tag(&self) -> String {
        format!("v{}", self.0)
    }
}

/// A provider-grammar basename obtained from authenticated metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedBasename(String);

impl DerivedBasename {
    pub fn parse(value: &str) -> Result<Self, TransportRefusal> {
        let bytes = value.as_bytes();
        let valid_edge = |byte: u8| byte.is_ascii_alphanumeric();
        if bytes.is_empty()
            || bytes.len() > 128
            || !value.is_ascii()
            || !valid_edge(bytes[0])
            || !valid_edge(bytes[bytes.len() - 1])
            || bytes
                .iter()
                .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'_' | b'-'))
            || value.contains("..")
        {
            return Err(TransportRefusal::InvalidBasename);
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyPolicy {
    Trust,
    Metadata,
    Signature,
    Archive,
}

impl BodyPolicy {
    const fn max_bytes(self) -> usize {
        match self {
            Self::Trust => TRUST_BYTES,
            Self::Metadata => METADATA_BYTES,
            Self::Signature => SIGNATURE_BYTES,
            Self::Archive => ARCHIVE_BYTES,
        }
    }
}

/// One provider-authorized logical Exchange release input.
///
/// Constructors own the body ceiling. There is no arbitrary URL, header, proxy, timeout or byte
/// limit in this request surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRequest {
    url: Url,
    body_policy: BodyPolicy,
}

impl ReleaseRequest {
    pub fn trust() -> Self {
        Self::fixed(TRUST_TAG, TRUST_BASENAME, BodyPolicy::Trust)
    }

    pub fn trust_signature(key_id: SigningKeyId) -> Self {
        Self::fixed(
            TRUST_TAG,
            &signature_basename(TRUST_BASENAME, &key_id),
            BodyPolicy::Signature,
        )
    }

    pub fn channel() -> Self {
        Self::fixed(CHANNEL_TAG, CHANNEL_BASENAME, BodyPolicy::Metadata)
    }

    pub fn channel_signature(key_id: SigningKeyId) -> Self {
        Self::fixed(
            CHANNEL_TAG,
            &signature_basename(CHANNEL_BASENAME, &key_id),
            BodyPolicy::Signature,
        )
    }

    pub fn manifest(version: StableVersion) -> Self {
        Self::fixed(
            &version.release_tag(),
            MANIFEST_BASENAME,
            BodyPolicy::Metadata,
        )
    }

    pub fn release_signature(version: StableVersion, key_id: SigningKeyId) -> Self {
        Self::fixed(
            &version.release_tag(),
            &signature_basename(MANIFEST_BASENAME, &key_id),
            BodyPolicy::Signature,
        )
    }

    pub fn archive(version: StableVersion, basename: DerivedBasename) -> Self {
        Self::immutable(version, basename, BodyPolicy::Archive)
    }

    pub fn initial_url(&self) -> &Url {
        &self.url
    }

    fn fixed(tag: &str, basename: &str, body_policy: BodyPolicy) -> Self {
        let url = Url::parse(&format!("{REPOSITORY_DOWNLOAD}/{tag}/{basename}"))
            .expect("fixed Exchange release URL is valid");
        debug_assert_eq!(url.origin().ascii_serialization(), GITHUB_ORIGIN);
        Self { url, body_policy }
    }

    fn immutable(
        version: StableVersion,
        basename: DerivedBasename,
        body_policy: BodyPolicy,
    ) -> Self {
        Self::fixed(&version.release_tag(), &basename.0, body_policy)
    }
}

fn signature_basename(metadata_basename: &str, key_id: &SigningKeyId) -> String {
    format!("{metadata_basename}.{}.minisig", key_id.0)
}

/// Bytes fetched under the fixed transport ceiling, still awaiting provider authentication.
#[derive(Debug, PartialEq, Eq)]
pub struct FetchedReleaseBytes(Vec<u8>);

impl FetchedReleaseBytes {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug)]
struct PreparedGet {
    url: Url,
    accept: &'static str,
}

impl PreparedGet {
    fn new(url: Url) -> Self {
        Self {
            url,
            accept: OPAQUE_MEDIA,
        }
    }
}

struct HopResponse<B> {
    status: u16,
    locations: Vec<Vec<u8>>,
    content_length: Option<u64>,
    body: B,
}

type BodyChunkFuture<'a> = Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, ()>> + Send + 'a>>;
type HopFuture<'a, B> =
    Pin<Box<dyn Future<Output = Result<HopResponse<B>, TransportRefusal>> + Send + 'a>>;

trait HopBody: Send {
    fn next_chunk(&mut self) -> BodyChunkFuture<'_>;
}

trait HopClient {
    type Body: HopBody;

    fn get(&mut self, request: PreparedGet, timeout: Duration) -> HopFuture<'_, Self::Body>;
}

struct ReqwestBody(reqwest::Response);

impl HopBody for ReqwestBody {
    fn next_chunk(&mut self) -> BodyChunkFuture<'_> {
        Box::pin(async move {
            self.0
                .chunk()
                .await
                .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                .map_err(|_| ())
        })
    }
}

struct ReqwestHop;

impl HopClient for ReqwestHop {
    type Body = ReqwestBody;

    fn get(&mut self, request: PreparedGet, timeout: Duration) -> HopFuture<'_, Self::Body> {
        Box::pin(async move {
            let hop_deadline = tokio::time::Instant::now() + timeout;
            let raw_url = request.url.to_string();
            let guarded = tokio::task::spawn_blocking(move || {
                net::guard_url_scoped_pinned(&raw_url, &PrivateNetAllow::None)
            });
            let (url, pinned) = tokio::time::timeout(timeout, guarded)
                .await
                .map_err(|_| TransportRefusal::Deadline)?
                .map_err(|_| TransportRefusal::Network)?
                .map_err(|_| TransportRefusal::Network)?;
            let host = url.host_str().ok_or(TransportRefusal::Network)?;
            if pinned.is_empty() {
                return Err(TransportRefusal::Network);
            }

            // flux-allow-direct-io: this is flux-system's closed Exchange release HTTP broker. Each
            // hop gets a fresh credential-free, proxy-free client pinned to guard-vetted addresses;
            // reqwest cannot follow a redirect behind the policy state machine.
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .https_only(true)
                .resolve_to_addrs(host, &pinned)
                .build()
                .map_err(|_| TransportRefusal::Network)?;
            let response = client
                .get(url)
                .header(ACCEPT, request.accept)
                .timeout(remaining(hop_deadline)?)
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() {
                        TransportRefusal::Deadline
                    } else {
                        TransportRefusal::Network
                    }
                })?;
            let status = response.status().as_u16();
            let locations = response
                .headers()
                .get_all(LOCATION)
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect();
            let content_length = response.content_length();
            Ok(HopResponse {
                status,
                locations,
                content_length,
                body: ReqwestBody(response),
            })
        })
    }
}

/// Fetch one logical release input through exactly one provider-authorized 302 hop.
pub async fn fetch(request: ReleaseRequest) -> Result<FetchedReleaseBytes, TransportRefusal> {
    fetch_with(&mut ReqwestHop, request).await
}

async fn fetch_with<H: HopClient>(
    hop: &mut H,
    request: ReleaseRequest,
) -> Result<FetchedReleaseBytes, TransportRefusal> {
    let deadline = tokio::time::Instant::now() + FETCH_DEADLINE;
    let first = hop
        .get(PreparedGet::new(request.url.clone()), remaining(deadline)?)
        .await?;
    if first.status != 302 {
        return Err(TransportRefusal::InitialStatus);
    }
    if first.locations.is_empty() {
        return Err(TransportRefusal::RedirectMissing);
    }
    if first.locations.len() != 1 {
        return Err(TransportRefusal::RedirectInvalid);
    }
    let cdn_url = validate_redirect_location(&first.locations[0])?;

    // The follow-up is constructed from only the validated URL and the fixed opaque Accept value.
    // No request builder, header map, cookie jar or client survives the first hop.
    let second = hop
        .get(PreparedGet::new(cdn_url), remaining(deadline)?)
        .await?;
    if second.status != 200 {
        return Err(TransportRefusal::FinalStatus);
    }
    read_bounded(second, request.body_policy.max_bytes(), deadline).await
}

fn remaining(deadline: tokio::time::Instant) -> Result<Duration, TransportRefusal> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        Err(TransportRefusal::Deadline)
    } else {
        Ok(remaining)
    }
}

async fn read_bounded<B: HopBody>(
    mut response: HopResponse<B>,
    max_bytes: usize,
    deadline: tokio::time::Instant,
) -> Result<FetchedReleaseBytes, TransportRefusal> {
    if response
        .content_length
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(TransportRefusal::BodyTooLarge);
    }
    let capacity = response.content_length.unwrap_or(0).min(max_bytes as u64) as usize;
    let mut bytes = Vec::new();
    bytes
        .try_reserve(capacity)
        .map_err(|_| TransportRefusal::BodyTooLarge)?;
    loop {
        let chunk = tokio::time::timeout(remaining(deadline)?, response.body.next_chunk())
            .await
            .map_err(|_| TransportRefusal::Deadline)?
            .map_err(|_| TransportRefusal::Network)?;
        let Some(chunk) = chunk else {
            return Ok(FetchedReleaseBytes(bytes));
        };
        if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
            return Err(TransportRefusal::BodyTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
}

fn validate_redirect_location(raw: &[u8]) -> Result<Url, TransportRefusal> {
    if raw.is_empty()
        || raw.len() > LOCATION_BYTES
        || !raw.is_ascii()
        || raw.iter().any(u8::is_ascii_control)
    {
        return Err(TransportRefusal::RedirectInvalid);
    }
    let raw = std::str::from_utf8(raw).map_err(|_| TransportRefusal::RedirectInvalid)?;
    let Some(after_scheme) = raw.strip_prefix("https://") else {
        return Err(TransportRefusal::RedirectInvalid);
    };
    let Some(path_start) = after_scheme.find('/') else {
        return Err(TransportRefusal::RedirectInvalid);
    };
    let authority = &after_scheme[..path_start];
    if authority != CDN_HOST && authority != format!("{CDN_HOST}:443") {
        return Err(TransportRefusal::RedirectInvalid);
    }
    let target = &after_scheme[path_start..];
    if target.contains('#') {
        return Err(TransportRefusal::RedirectInvalid);
    }
    let (raw_path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };
    if !valid_cdn_path(raw_path) || !valid_cdn_query(raw_query) {
        return Err(TransportRefusal::RedirectInvalid);
    }

    let url = Url::parse(raw).map_err(|_| TransportRefusal::RedirectInvalid)?;
    // `url` follows the URL Standard and therefore normalizes such inputs as raw spaces, dot
    // segments and backslashes. Provider policy is a grammar over the received ASCII bytes, so the
    // raw path/query above must agree with the parsed path/query. The sole admitted serialization
    // difference is an explicit default `:443`, which the URL parser removes.
    if url.path() != raw_path
        || url.query() != raw_query
        || url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str() != Some(CDN_HOST)
    {
        return Err(TransportRefusal::RedirectInvalid);
    }
    Ok(url)
}

fn valid_cdn_path(path: &str) -> bool {
    const PREFIX: &str = "/github-production-release-asset/";
    let Some(rest) = path.strip_prefix(PREFIX) else {
        return false;
    };
    let Some((asset_id, identifier)) = rest.split_once('/') else {
        return false;
    };
    if asset_id.is_empty()
        || asset_id.len() > 20
        || asset_id.starts_with('0')
        || !asset_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let lengths = [8, 4, 4, 4, 12];
    let mut pieces = identifier.split('-');
    for length in lengths {
        let Some(piece) = pieces.next() else {
            return false;
        };
        if piece.len() != length
            || !piece
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return false;
        }
    }
    pieces.next().is_none()
}

fn valid_cdn_query(query: Option<&str>) -> bool {
    let Some(query) = query else {
        return true;
    };
    if query.len() > QUERY_BYTES || !query.is_ascii() {
        return false;
    }
    if query.is_empty() {
        return true;
    }
    let mut names = HashSet::new();
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            return false;
        };
        if name.is_empty()
            || name.contains('%')
            || !CDN_QUERY_NAMES.contains(&name)
            || !names.insert(name)
            || value.is_empty()
            || value.len() > QUERY_VALUE_BYTES
            || !valid_percent_encoded_value(value)
        {
            return false;
        }
    }
    true
}

fn valid_percent_encoded_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let decoded = if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return false;
            }
            let Some(high) = hex_value(bytes[index + 1]) else {
                return false;
            };
            let Some(low) = hex_value(bytes[index + 2]) else {
                return false;
            };
            index += 3;
            high * 16 + low
        } else {
            let decoded = bytes[index];
            index += 1;
            decoded
        };
        if decoded.is_ascii_control() {
            return false;
        }
    }
    true
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    struct FakeBody(VecDeque<Result<Vec<u8>, ()>>);

    impl HopBody for FakeBody {
        fn next_chunk(&mut self) -> BodyChunkFuture<'_> {
            Box::pin(async move { self.0.pop_front().transpose() })
        }
    }

    struct FakeResponse {
        status: u16,
        locations: Vec<Vec<u8>>,
        content_length: Option<u64>,
        chunks: Vec<Result<Vec<u8>, ()>>,
    }

    impl FakeResponse {
        fn redirect(location: &str) -> Self {
            Self {
                status: 302,
                locations: vec![location.as_bytes().to_vec()],
                content_length: Some(0),
                chunks: Vec::new(),
            }
        }

        fn ok(body: &[u8]) -> Self {
            Self {
                status: 200,
                locations: Vec::new(),
                content_length: Some(body.len() as u64),
                chunks: vec![Ok(body.to_vec())],
            }
        }
    }

    #[derive(Default)]
    struct FakeHop {
        requests: Vec<(String, &'static str)>,
        responses: VecDeque<Result<FakeResponse, TransportRefusal>>,
    }

    impl FakeHop {
        fn with(responses: impl IntoIterator<Item = FakeResponse>) -> Self {
            Self {
                responses: responses.into_iter().map(Ok).collect(),
                ..Self::default()
            }
        }
    }

    impl HopClient for FakeHop {
        type Body = FakeBody;

        fn get(&mut self, request: PreparedGet, _timeout: Duration) -> HopFuture<'_, Self::Body> {
            self.requests
                .push((request.url.to_string(), request.accept));
            let response = self.responses.pop_front().expect("scripted response");
            Box::pin(async move {
                let response = response?;
                Ok(HopResponse {
                    status: response.status,
                    locations: response.locations,
                    content_length: response.content_length,
                    body: FakeBody(response.chunks.into()),
                })
            })
        }
    }

    const VALID_CDN: &str = "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?jwt=abc%2Fdef&sig=value";

    #[test]
    fn provider_request_origins_are_fixed_and_version_is_not_a_url() {
        assert_eq!(
            ReleaseRequest::trust().initial_url().as_str(),
            "https://github.com/codewandler/flux-exchange/releases/download/exchange-trust-v1/flux-exchange-release-trust.json"
        );
        assert_eq!(
            ReleaseRequest::channel().initial_url().as_str(),
            "https://github.com/codewandler/flux-exchange/releases/download/exchange-stable-v1/flux-exchange-release-channel.json"
        );

        let version = StableVersion::parse("12.3.40").unwrap();
        assert_eq!(
            ReleaseRequest::manifest(version.clone())
                .initial_url()
                .as_str(),
            "https://github.com/codewandler/flux-exchange/releases/download/v12.3.40/flux-exchange-release-manifest.json"
        );
        let key_id = SigningKeyId::parse("release-2026-01").unwrap();
        assert_eq!(
            ReleaseRequest::release_signature(version, key_id)
                .initial_url()
                .as_str(),
            "https://github.com/codewandler/flux-exchange/releases/download/v12.3.40/flux-exchange-release-manifest.json.release-2026-01.minisig"
        );
    }

    #[test]
    fn provider_grammars_refuse_path_or_version_widening() {
        for version in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.-3",
            "1.2.3-rc1",
            "1000000000.2.3",
        ] {
            assert_eq!(
                StableVersion::parse(version),
                Err(TransportRefusal::InvalidVersion),
                "version {version:?}"
            );
        }
        for basename in [
            "",
            ".hidden",
            "trailing.",
            "../archive",
            "a..b",
            "a/b",
            "a\\b",
            "a%2fb",
            "a b",
        ] {
            assert_eq!(
                DerivedBasename::parse(basename),
                Err(TransportRefusal::InvalidBasename),
                "basename {basename:?}"
            );
        }
        for key_id in ["", "-a", "a-", "A", "a--b", "a.b", "a_b", "a/b"] {
            assert_eq!(
                SigningKeyId::parse(key_id),
                Err(TransportRefusal::InvalidKeyId),
                "key id {key_id:?}"
            );
        }
    }

    #[tokio::test]
    async fn one_validated_redirect_returns_only_bounded_opaque_bytes() {
        let mut hop = FakeHop::with([
            FakeResponse::redirect(VALID_CDN),
            FakeResponse::ok(b"unverified bytes"),
        ]);
        let bytes = fetch_with(&mut hop, ReleaseRequest::channel())
            .await
            .unwrap();
        assert_eq!(bytes.as_slice(), b"unverified bytes");
        assert_eq!(
            hop.requests,
            vec![
                (
                    ReleaseRequest::channel().initial_url().to_string(),
                    OPAQUE_MEDIA
                ),
                (VALID_CDN.to_owned(), OPAQUE_MEDIA),
            ]
        );
    }

    #[test]
    fn redirect_path_and_query_grammar_is_closed() {
        assert!(validate_redirect_location(VALID_CDN.as_bytes()).is_ok());
        assert!(validate_redirect_location(
            VALID_CDN
                .replacen(CDN_HOST, &format!("{CDN_HOST}:443"), 1)
                .as_bytes()
        )
        .is_ok());
        let refused = [
            "http://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://github.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://user@release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com:444/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com/other/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/0/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89AB-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?unknown=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x&sig=y",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?s%69g=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=%0A",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=%xx",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x#fragment",
            "https://RELEASE-ASSETS.GITHUBUSERCONTENT.COM/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/9/../1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com\\github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig=raw space",
        ];
        for location in refused {
            assert_eq!(
                validate_redirect_location(location.as_bytes()),
                Err(TransportRefusal::RedirectInvalid),
                "location {location:?}"
            );
        }
        assert_eq!(
            validate_redirect_location(&vec![b'x'; LOCATION_BYTES + 1]),
            Err(TransportRefusal::RedirectInvalid)
        );
        let oversized_query = format!(
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/01234567-89ab-cdef-0123-456789abcdef?sig={}",
            "x".repeat(QUERY_VALUE_BYTES + 1)
        );
        assert_eq!(
            validate_redirect_location(oversized_query.as_bytes()),
            Err(TransportRefusal::RedirectInvalid)
        );
    }

    #[tokio::test]
    async fn initial_and_final_statuses_admit_only_302_then_200() {
        let mut initial_200 = FakeHop::with([FakeResponse::ok(b"unsigned latest")]);
        assert_eq!(
            fetch_with(&mut initial_200, ReleaseRequest::trust()).await,
            Err(TransportRefusal::InitialStatus)
        );

        let mut missing = FakeHop::with([FakeResponse {
            status: 302,
            locations: Vec::new(),
            content_length: Some(0),
            chunks: Vec::new(),
        }]);
        assert_eq!(
            fetch_with(&mut missing, ReleaseRequest::trust()).await,
            Err(TransportRefusal::RedirectMissing)
        );

        let mut second_redirect = FakeHop::with([
            FakeResponse::redirect(VALID_CDN),
            FakeResponse::redirect(VALID_CDN),
        ]);
        assert_eq!(
            fetch_with(&mut second_redirect, ReleaseRequest::trust()).await,
            Err(TransportRefusal::FinalStatus)
        );
    }

    #[tokio::test]
    async fn body_caps_are_fixed_by_request_kind_and_checked_while_streaming() {
        let key_id = SigningKeyId::parse("channel-2026-01").unwrap();
        let mut declared = FakeHop::with([
            FakeResponse::redirect(VALID_CDN),
            FakeResponse {
                status: 200,
                locations: Vec::new(),
                content_length: Some((SIGNATURE_BYTES + 1) as u64),
                chunks: Vec::new(),
            },
        ]);
        assert_eq!(
            fetch_with(
                &mut declared,
                ReleaseRequest::channel_signature(key_id.clone())
            )
            .await,
            Err(TransportRefusal::BodyTooLarge)
        );

        let mut streamed = FakeHop::with([
            FakeResponse::redirect(VALID_CDN),
            FakeResponse {
                status: 200,
                locations: Vec::new(),
                content_length: None,
                chunks: vec![Ok(vec![0; SIGNATURE_BYTES]), Ok(vec![1])],
            },
        ]);
        assert_eq!(
            fetch_with(&mut streamed, ReleaseRequest::channel_signature(key_id)).await,
            Err(TransportRefusal::BodyTooLarge)
        );
        assert_eq!(ReleaseRequest::trust().body_policy.max_bytes(), TRUST_BYTES);
        assert_eq!(
            ReleaseRequest::channel().body_policy.max_bytes(),
            METADATA_BYTES
        );
        assert_eq!(
            ReleaseRequest::archive(
                StableVersion::parse("1.0.0").unwrap(),
                DerivedBasename::parse("archive.tar.zst").unwrap()
            )
            .body_policy
            .max_bytes(),
            ARCHIVE_BYTES
        );
        assert_eq!(FETCH_DEADLINE, Duration::from_secs(120));
    }

    #[test]
    fn diagnostics_are_closed_and_value_free() {
        let cases = [
            TransportRefusal::InvalidVersion,
            TransportRefusal::InvalidKeyId,
            TransportRefusal::InvalidBasename,
            TransportRefusal::Network,
            TransportRefusal::InitialStatus,
            TransportRefusal::RedirectMissing,
            TransportRefusal::RedirectInvalid,
            TransportRefusal::FinalStatus,
            TransportRefusal::BodyTooLarge,
            TransportRefusal::Deadline,
        ];
        for refusal in cases {
            let rendered = refusal.to_string();
            assert_eq!(rendered, refusal.code());
            assert!(rendered
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'));
        }
    }
}
