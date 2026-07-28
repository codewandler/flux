//! The Deterministic Agent Test Kit (feature `test-kit`, default off).
//!
//! Record a run once against a live [`Client`], commit the result as a redacted golden fixture
//! (`tests/scenarios/<name>/`), and re-run the **real** agent offline in `cargo test` for $0 —
//! asserting on the canonical Flux-Lang plan (not a transcript). Built entirely on shipped
//! primitives: [`flux_flow::replay::replay_session`], [`Session::fork`]/[`Fork::inject`], and
//! [`flux_events::run_diff`].
//!
//! Four doors on [`Scenario`]: [`record`](Scenario::record) (live, once),
//! [`replay`](Scenario::replay) (Engine 1 — hermetic, the CI guard — proven safe under a
//! deny-all approver and a never-called provider, since it dispatches nothing at all),
//! [`inject_at`](Scenario::inject_at) (the fault-injection door, returning a
//! [`crate::whatif::Counterfactual`]), and (D-176) [`check`](Scenario::check) (Engine 2 —
//! world-pinned re-drive: `Frozen(golden, Halt)` pins every OP, a [`ServingProvider`] pins the
//! MODEL too, so a clean run costs $0 and any drift — a different plan, or a different world —
//! surfaces as a classified [`Report`]). [`Outcome`] carries `replay`'s assertions.
//!
//! (D-195) A fifth, complementary axis: [`Scenario::judge`]/[`Scenario::assert_judge`] grade a
//! TEXT output against a natural-language [`Rubric`] with an LLM judge, for outputs that don't
//! have one canonical answer the way a plan does. The judge's own model call flows through the
//! SAME kind of cassette as an agent model call — see its doc comment for the record/replay
//! contract.
//!
//! ## Fixture format
//!
//! `tests/scenarios/<name>/` is a plain [`Storage::dir`] (`events.db` + an empty `flow.db` — also
//! openable by `flux replay`/`flux sessions`/`flux diff`), plus:
//! - `model.jsonl` — one JSON line per recorded model call (see [`ModelCallRecord`]), redacted
//!   before it is ever written to disk.
//! - `plan.flux.snap` — the canonical Flux-Lang text of every accepted plan in the recorded turn
//!   (joined by a `---` marker; v1 scenarios are single-turn, so ordinarily just one).
//! - `judge.jsonl` (D-195, only present once a scenario uses [`Scenario::judge`]) — one JSON line
//!   per committed judge verdict, same [`ModelCallRecord`] shape as `model.jsonl`; accumulates
//!   additively rather than being rewritten wholesale, since many distinct judge assertions across
//!   many tests can share one fixture.
//! - `scenario.toml` — a manifest (see [`Manifest`]): what was recorded, when, and with what
//!   cassette settings — drift diagnostics, and (from D-176) `check()`'s re-drive input.
//!
//! Every piece is redacted through the client's own [`Redactor`] before it touches disk, so a
//! fixture is safe to `git commit` with no extra work.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use flux_core::{Chunk, ContentBlock, PricingTable, Result};
use flux_events::{DiffLineKind, DiffRow, EventContext, EventStore, RunDiff};
use flux_flow::cassette::{CassetteScope, FrozenTape, RecordScope, ReplayTape};
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_lang::ast::RunEvent;
use flux_provider::{ChunkStream, Provider, Request};
use flux_secret::Redactor;

use crate::assembly::VariantOverrides;
use crate::whatif::Counterfactual;
use crate::{Client, Session};

/// The redaction placeholder text a fixture's `model.jsonl`/`plan.flux.snap` may contain —
/// recorded into `scenario.toml` as a drift diagnostic (the same marker `flux_secret::Redactor`
/// substitutes; kept as a local literal since the constant itself is private to `flux-secret`).
const REDACTION_MARKER: &str = "[redacted]";

/// A no-op [`AgentSink`] — used where a driver requires a sink but the caller doesn't observe the
/// turn live (every Test Kit door reads back the recorded/replayed result from the event store
/// instead).
struct NullSink;
impl AgentSink for NullSink {}

// --- fixture manifest + model cassette -------------------------------------

/// `scenario.toml` — what was recorded, when, and under what settings. Drift diagnostics (a
/// fixture whose `model.jsonl`/`events.db` no longer matches its own manifest is a loud bug, not a
/// silent stale pass), and — from D-176 — `Scenario::check`'s re-drive input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Manifest {
    /// The fixture format version (currently `1`).
    pub schema: u32,
    /// The scenario's name (the fixture directory's file name).
    pub name: String,
    /// The `codewandler-flux-sdk` version that recorded this fixture.
    pub flux_version: String,
    /// When the fixture was recorded (unix ms).
    pub recorded_at_ms: i64,
    /// The recorded session id, inside `events.db`.
    pub session: String,
    /// The input this scenario re-drives (the turn's user input).
    pub input: String,
    /// The model that was recorded.
    pub model: String,
    /// `FLUX_CASSETTE_MAX_BYTES` in effect when this fixture was recorded — an oversized cell
    /// truncates at capture time and is never hermetically replayable past that point; a
    /// diagnostic re-record hint stays actionable without re-deriving this from the tape.
    pub cassette_max_bytes: usize,
    /// The redaction placeholder text this fixture's cells/model calls may contain.
    pub redaction_marker: String,
}

/// One recorded model call, one line of `model.jsonl`. Redacted-by-construction: `request` and
/// `chunks` are redacted through the client's [`Redactor`] before this is ever written to disk.
///
/// `pub(crate)` (not public): D-176's `ServingProvider` reuses this shape, [`canonical_request`],
/// and [`hash_request`] to serve a matching call deterministically at $0 — but the fixture's
/// on-disk JSON is the only stable contract, not this Rust type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelCallRecord {
    /// The record format version (currently `1`).
    pub(crate) v: u32,
    /// `sha256_hex` of the canonical redacted request — the match key a serving provider (D-176)
    /// looks up a re-drive's request against.
    pub(crate) hash: String,
    /// The model id this call was made against.
    pub(crate) model: String,
    /// The canonical (redacted) request, for human inspection and the hash's input.
    pub(crate) request: serde_json::Value,
    /// The exact (redacted) chunk stream the provider returned.
    pub(crate) chunks: Vec<Chunk>,
}

/// Build the canonical JSON shape of a model request that identity/hashing is computed over.
/// Deliberately excludes `req.trace`/`req.metadata` — host-owned correlation, not request
/// identity, and often contains a run-specific id that would make an identical request hash
/// differently every recording. `pub(crate)`: D-176's `ServingProvider` recomputes this same shape
/// over a re-drive's request to look up a match.
pub(crate) fn canonical_request(req: &Request) -> serde_json::Value {
    serde_json::json!({
        "model": req.model,
        "system": req.system,
        "system_segments": req.system_segments.iter().map(|s| serde_json::json!({
            "text": s.text,
            "cache": s.cache,
        })).collect::<Vec<_>>(),
        "messages": req.messages,
        "tools": req.tools.iter().map(|t| serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        })).collect::<Vec<_>>(),
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
        "top_p": req.top_p,
        "stop_sequences": req.stop_sequences,
        "thinking": req.thinking,
        "effort": req.effort,
    })
}

/// Redact then hash a canonical request — redaction happens BEFORE hashing, so two requests that
/// differ only in a secret value that redacts to the same placeholder still match. Returns the
/// redacted value (safe to persist) and its hash. `pub(crate)`: shared with D-176's
/// `ServingProvider`.
pub(crate) fn redact_and_hash_request(
    req: &Request,
    redactor: &Redactor,
) -> Result<(serde_json::Value, String)> {
    let canonical = canonical_request(req);
    let canonical_str = serde_json::to_string(&canonical)?;
    let redacted_str = redactor.redact(&canonical_str);
    let hash = flux_lang::runtime::sha256_hex(&redacted_str);
    let redacted_value: serde_json::Value =
        serde_json::from_str(&redacted_str).unwrap_or(canonical);
    Ok((redacted_value, hash))
}

/// Redact a single response [`Chunk`] by round-tripping it through JSON — reuses the same
/// [`Redactor`] the request side uses, so a chunk that echoes secret-shaped text (a model quoting
/// back part of a tool result) is scrubbed the same way.
fn redact_chunk(chunk: &Chunk, redactor: &Redactor) -> Result<Chunk> {
    let json = serde_json::to_string(chunk)?;
    let redacted = redactor.redact(&json);
    Ok(serde_json::from_str(&redacted)?)
}

/// Tees a real [`Provider`]'s responses into a shared buffer of [`ModelCallRecord`]s while still
/// returning them to the caller — `Scenario::record`'s recording door. The buffered records are
/// what `Scenario::record` writes to `model.jsonl`.
struct RecordingProvider {
    inner: Arc<dyn Provider>,
    redactor: Redactor,
    records: Arc<Mutex<Vec<ModelCallRecord>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        use futures::stream::{self, TryStreamExt};

        let (request, hash) = redact_and_hash_request(&req, &self.redactor)?;
        let model = req.model.clone();
        let raw_chunks: Vec<Chunk> = self.inner.stream(req).await?.try_collect().await?;
        let chunks = raw_chunks
            .iter()
            .map(|c| redact_chunk(c, &self.redactor))
            .collect::<Result<Vec<_>>>()?;

        self.records.lock().unwrap().push(ModelCallRecord {
            v: 1,
            hash,
            model,
            request,
            chunks: chunks.clone(),
        });

        Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// D-176's `Scenario::check` model cassette: serve a recorded [`ModelCallRecord`] on a
/// canonical-request hash hit ($0, deterministic), falling through to `inner` on a miss (a real
/// model call — counted, never silently swallowed). Records with the SAME hash (an identical
/// request made more than once in a turn) serve in recorded order, one per hit, via a `VecDeque` —
/// only once a hash's queue is exhausted does a repeat of that request fall through to `inner`.
struct ServingProvider {
    inner: Arc<dyn Provider>,
    redactor: Redactor,
    by_hash: Mutex<HashMap<String, VecDeque<ModelCallRecord>>>,
    served: AtomicUsize,
    live: AtomicUsize,
}

impl ServingProvider {
    fn new(inner: Arc<dyn Provider>, redactor: Redactor, records: Vec<ModelCallRecord>) -> Self {
        let mut by_hash: HashMap<String, VecDeque<ModelCallRecord>> = HashMap::new();
        for record in records {
            by_hash
                .entry(record.hash.clone())
                .or_default()
                .push_back(record);
        }
        Self {
            inner,
            redactor,
            by_hash: Mutex::new(by_hash),
            served: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
        }
    }

    /// How many calls were served from the recorded `model.jsonl` — $0, deterministic.
    fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }

    /// How many calls fell through to the real provider (a request hash `check()` didn't record) —
    /// the "the recorded golden no longer covers this re-drive" honesty signal.
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for ServingProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        use futures::stream;

        let (_, hash) = redact_and_hash_request(&req, &self.redactor)?;
        let recorded = self
            .by_hash
            .lock()
            .unwrap()
            .get_mut(&hash)
            .and_then(VecDeque::pop_front);
        if let Some(record) = recorded {
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(Box::pin(stream::iter(record.chunks.into_iter().map(Ok))));
        }
        self.live.fetch_add(1, Ordering::SeqCst);
        self.inner.stream(req).await
    }
}

// --- fixture IO --------------------------------------------------------------

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(dir.join("scenario.toml")).map_err(|e| {
        flux_core::Error::Other(format!(
            "scenario fixture {}: read scenario.toml: {e}",
            dir.display()
        ))
    })?;
    toml::from_str(&text)
        .map_err(|e| flux_core::Error::Other(format!("scenario.toml: invalid manifest: {e}")))
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<()> {
    let text = toml::to_string_pretty(manifest)
        .map_err(|e| flux_core::Error::Other(format!("scenario.toml: serialize: {e}")))?;
    std::fs::write(dir.join("scenario.toml"), text)?;
    Ok(())
}

/// Read back a fixture's `model.jsonl` — D-176's `Scenario::check` loads a fixture's recorded model
/// calls into its [`ServingProvider`].
fn read_model_calls(dir: &Path) -> Result<Vec<ModelCallRecord>> {
    let path = dir.join("model.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .map_err(|e| flux_core::Error::Other(format!("model.jsonl: invalid record: {e}")))
        })
        .collect()
}

fn write_model_calls(dir: &Path, records: &[ModelCallRecord]) -> Result<()> {
    let mut text = String::new();
    for r in records {
        text.push_str(&serde_json::to_string(r)?);
        text.push('\n');
    }
    std::fs::write(dir.join("model.jsonl"), text)?;
    Ok(())
}

/// Read back a fixture's `judge.jsonl` (D-195) — every judge verdict ever committed for this
/// fixture, keyed by the canonical request hash [`Scenario::judge`] looks a call up against.
/// Reuses [`ModelCallRecord`] wholesale: an LLM-judge call IS a model call, recorded the same way.
fn read_judge_calls(dir: &Path) -> Result<Vec<ModelCallRecord>> {
    let path = dir.join("judge.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .map_err(|e| flux_core::Error::Other(format!("judge.jsonl: invalid record: {e}")))
        })
        .collect()
}

/// Append one freshly-recorded judge call to `judge.jsonl` — additive by design: unlike
/// `model.jsonl` (rewritten wholesale by one `Scenario::record`), a fixture's judge cassette
/// accumulates one entry per DISTINCT (criterion, target, model) ever graded against it, since
/// many different `assert_judge` calls across many tests can share one fixture. An entry
/// superseded by an intentional re-grade (`FLUX_GOLDEN=update` against a changed target) is simply
/// never looked up again — harmless clutter, not pruned.
fn append_judge_call(dir: &Path, record: ModelCallRecord) -> Result<()> {
    let mut records = read_judge_calls(dir)?;
    records.push(record);
    let mut text = String::new();
    for r in &records {
        text.push_str(&serde_json::to_string(r)?);
        text.push('\n');
    }
    std::fs::write(dir.join("judge.jsonl"), text)?;
    Ok(())
}

/// The canonical Flux-Lang text of every accepted plan attempt across `session`'s turns, joined by
/// a `---` marker — the single source of truth `plan.flux.snap` is written from and compared
/// against (used identically at record time and at replay/assertion time).
fn accepted_plan_text(events: &EventStore, session: &str) -> Result<String> {
    let mut parts = Vec::new();
    for turn in events.turns(session)? {
        for attempt in turn.plan_attempts {
            if attempt.outcome == "accepted" {
                if let Some(src) = attempt.plan_source {
                    parts.push(src);
                }
            }
        }
    }
    Ok(parts.join("\n---\n"))
}

/// Copy a SQLite store file together with its `-wal`/`-shm` sidecars, if present (best-effort —
/// WAL checkpointing on close is not guaranteed, so a fixture's sidecars may or may not exist).
fn copy_sqlite_family(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)?;
    for ext in ["-wal", "-shm"] {
        let s = with_appended_ext(src, ext);
        if s.exists() {
            std::fs::copy(&s, with_appended_ext(dst, ext))?;
        }
    }
    Ok(())
}

fn with_appended_ext(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// A process-wide counter decorrelating two work dirs minted in the same millisecond (e.g. by two
/// threads of the same test binary) — simpler and more portable than parsing `ThreadId`.
static WORK_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn fresh_work_dir(label: &str) -> PathBuf {
    let n = WORK_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let unique = format!("{}-{}-{}", std::process::id(), now_ms(), n);
    std::env::temp_dir().join(format!("{label}-{unique}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --- judge assertions (D-195) -------------------------------------------------

/// Explicit configuration for a [`Scenario::judge`]/[`Scenario::assert_judge`] call — the judge
/// model is always named here, per assertion, so no call can spend without the caller choosing a
/// target for it. `#[non_exhaustive]`: a future knob (temperature, a stricter system prompt, …)
/// grows this struct, never a new constructor.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Rubric {
    /// The `provider/model` spec the judge call is made against (e.g. `"mock"`,
    /// `"anthropic/claude-haiku-4.6"`) — there is no default; every rubric names one.
    pub model: String,
}

impl Rubric {
    /// A rubric graded by `model` — the only field a judge call needs.
    pub fn model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

/// The graded result of a judge assertion: pass/fail plus the judge's own rationale — surfaced on
/// failure so a red assertion says *why*, not just that it failed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Verdict {
    /// Whether the judged text satisfied the criterion.
    pub passed: bool,
    /// The judge's own explanation — shown in [`assert_pass`](Self::assert_pass)'s panic message.
    pub rationale: String,
}

impl Verdict {
    /// Panics with the judge's rationale if [`passed`](Self::passed) is false.
    pub fn assert_pass(&self) {
        assert!(self.passed, "judge verdict: FAIL — {}", self.rationale);
    }
}

/// The judge's fixed system prompt — stable text keeps the canonical request (and therefore its
/// hash) identical across runs, so a committed verdict matches on every replay of the same
/// criterion/target/model.
const JUDGE_SYSTEM_PROMPT: &str = "You are a strict grader for an automated test suite. You will \
                                    be given one grading criterion and one piece of text. Decide \
                                    whether the text satisfies the criterion. Respond with \
                                    EXACTLY one JSON object and nothing else, of the shape \
                                    {\"passed\": true|false, \"rationale\": \"<one short \
                                    sentence>\"}.";

/// Build the judge's request, deterministically, from `criterion` + `target` — the same shape
/// every time, so the same call always hashes the same.
fn judge_request(model: &str, criterion: &str, target: &str) -> Request {
    let prompt = format!(
        "Criterion:\n{criterion}\n\nText under test:\n{target}\n\nDoes the text satisfy the \
         criterion? Reply with the JSON verdict only."
    );
    Request::new(model.to_string(), prompt)
        .with_system(JUDGE_SYSTEM_PROMPT.to_string())
        .with_max_tokens(512)
}

/// Concatenate every text-bearing chunk in a completed response. The judge's replies are plain
/// text, never tool calls, but a codec may stream text as `TextDelta`s or as one assembled
/// `Block(ContentBlock::Text)` — collect whichever form appears.
fn chunks_to_text(chunks: &[Chunk]) -> String {
    let mut out = String::new();
    for chunk in chunks {
        match chunk {
            Chunk::TextDelta(t) => out.push_str(t),
            Chunk::Block(ContentBlock::Text { text }) => out.push_str(text),
            _ => {}
        }
    }
    out
}

/// Parse a judge's raw reply into a [`Verdict`] — lenient about surrounding prose (some models
/// wrap the JSON in a code fence or a sentence despite the system prompt): scans for the first
/// `{...}` object rather than requiring the whole reply to be bare JSON.
fn parse_verdict(text: &str) -> Result<Verdict> {
    #[derive(Deserialize)]
    struct RawVerdict {
        passed: bool,
        #[serde(default)]
        rationale: String,
    }
    let slice = match (text.find('{'), text.rfind('}')) {
        (Some(start), Some(end)) if end >= start => &text[start..=end],
        _ => text,
    };
    let raw: RawVerdict = serde_json::from_str(slice).map_err(|e| {
        flux_core::Error::Other(format!(
            "judge: could not parse a verdict JSON object from the reply: {e}\nreply: {text:?}"
        ))
    })?;
    Ok(Verdict {
        passed: raw.passed,
        rationale: raw.rationale,
    })
}

// --- Scenario ----------------------------------------------------------------

/// A recorded agent run, redacted-by-construction and safe to `git commit`. See the [module
/// docs](self) for the fixture format.
///
/// Every door ([`load`](Self::load), [`replay`](Self::replay), [`inject_at`](Self::inject_at))
/// operates on an isolated **temp copy** of the fixture's stores, never the committed directory
/// itself (except [`record`](Self::record), which writes it) — so running a scenario's assertions
/// repeatedly, or concurrently, never perturbs (or races on) the checked-in fixture.
pub struct Scenario {
    work_dir: PathBuf,
    source_dir: PathBuf,
    manifest: Manifest,
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

impl Scenario {
    /// Record one live turn against `client` and write a fixture at `path`. Runs a **variant**
    /// engine (same tools/permissions/authorization as `client`, a [`RecordingProvider`] tee
    /// wrapping its real provider) for a single turn on a fresh session of `client`'s own event
    /// store, then exports that session into the fixture via
    /// [`EventStore::copy_session_to`](flux_events::EventStore::copy_session_to).
    ///
    /// Refuses to overwrite an existing fixture unless `FLUX_GOLDEN=update` is set (the re-baseline
    /// convention, mirroring `cargo-insta`). Errors if the turn ends suspended — scenarios are
    /// single-turn only in this version.
    pub async fn record(client: &Client, input: &str, path: impl AsRef<Path>) -> Result<Scenario> {
        Self::record_with_call_count(client, input, path)
            .await
            .map(|(scenario, _live_calls)| scenario)
    }

    /// Same as [`record`](Self::record), but also returns how many live model calls the recording
    /// turn actually made — the honest count [`check`](Self::check) reports back in a
    /// `FLUX_GOLDEN=update` [`Report`] instead of a fabricated one (D-184).
    async fn record_with_call_count(
        client: &Client,
        input: &str,
        path: impl AsRef<Path>,
    ) -> Result<(Scenario, usize)> {
        let path = path.as_ref();
        let updating = std::env::var("FLUX_GOLDEN").as_deref() == Ok("update");
        if path.join("scenario.toml").exists() && !updating {
            return Err(flux_core::Error::Other(format!(
                "scenario fixture already exists at {} — set FLUX_GOLDEN=update to re-record it",
                path.display()
            )));
        }

        let redactor = client.engine.executor.context().redactor.clone();
        let records: Arc<Mutex<Vec<ModelCallRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let recording = Arc::new(RecordingProvider {
            inner: client.assembly.provider.clone(),
            redactor,
            records: records.clone(),
        });

        let engine = client.assembly.variant(VariantOverrides {
            provider: Some(recording),
            ..Default::default()
        })?;

        let session_id = engine.events.create_session(&engine.model)?;
        let mut sink = NullSink;
        engine.run_turn(&session_id, input, &mut sink).await?;
        if engine.flow.has_suspension(&session_id)? {
            return Err(flux_core::Error::Other(format!(
                "Scenario::record: turn on session {session_id} ended suspended — scenarios are \
                 single-turn only (v1); drive the flow to completion before recording"
            )));
        }

        std::fs::create_dir_all(path)?;
        for name in [
            "events.db",
            "events.db-wal",
            "events.db-shm",
            "flow.db",
            "flow.db-wal",
            "flow.db-shm",
        ] {
            let _ = std::fs::remove_file(path.join(name));
        }

        let fixture_events = Arc::new(EventStore::open(path.join("events.db"))?);
        let new_session = engine
            .events
            .copy_session_to(&session_id, &fixture_events)?;
        // An empty, schema-valid flow.db — v1 scenarios never suspend, so there is never any flow
        // state to persist, but a real `Storage::dir` needs the file to exist.
        drop(FlowStore::open(
            path.join("flow.db"),
            fixture_events.clone(),
        )?);

        let live_calls = records.lock().unwrap().len();
        write_model_calls(path, &records.lock().unwrap())?;
        let plan_text = accepted_plan_text(&fixture_events, &new_session)?;
        std::fs::write(path.join("plan.flux.snap"), &plan_text)?;

        let manifest = Manifest {
            schema: 1,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            flux_version: env!("CARGO_PKG_VERSION").to_string(),
            recorded_at_ms: now_ms(),
            session: new_session,
            input: input.to_string(),
            model: engine.model.clone(),
            cassette_max_bytes: flux_flow::cassette::max_cell_bytes(),
            redaction_marker: REDACTION_MARKER.to_string(),
        };
        write_manifest(path, &manifest)?;

        Ok((Self::load(path)?, live_calls))
    }

    /// Load a fixture from `path`, copying its stores into an isolated temp work directory — $0,
    /// offline, no client needed.
    pub fn load(path: impl AsRef<Path>) -> Result<Scenario> {
        let source_dir = path.as_ref().to_path_buf();
        let manifest = read_manifest(&source_dir)?;
        let work_dir = fresh_work_dir("flux-sdk-scenario");
        std::fs::create_dir_all(&work_dir)?;
        copy_sqlite_family(&source_dir.join("events.db"), &work_dir.join("events.db"))?;
        copy_sqlite_family(&source_dir.join("flow.db"), &work_dir.join("flow.db"))?;
        Ok(Scenario {
            work_dir,
            source_dir,
            manifest,
        })
    }

    /// [`load`](Self::load) if a fixture already exists at `path`, else [`record`](Self::record)
    /// one against `client` — the convenient one-liner for a test that should self-bootstrap its
    /// own golden on first run.
    pub async fn load_or_record(
        client: &Client,
        input: &str,
        path: impl AsRef<Path>,
    ) -> Result<Scenario> {
        let path = path.as_ref();
        if path.join("scenario.toml").exists() {
            Self::load(path)
        } else {
            Self::record(client, input, path).await
        }
    }

    /// The isolated temp directory this scenario's stores were copied into — where a
    /// [`replay`](Self::replay)/[`check`](Self::check) records its own session. Removed when the
    /// `Scenario` drops; open it (rather than the fixture) to inspect what a replay just produced.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// The manifest this scenario was loaded/recorded with.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// **Engine 1 — hermetic replay.** Re-run the recorded plan(s) with every leaf-op output served
    /// from the fixture's cassette: zero live dispatches, the model is never called, side effects
    /// never re-fire. `client` supplies only the **executor** (op catalog, permissions,
    /// authorization) — never its provider or its stores — so this is safe to run against a client
    /// built with a deny-all approver and a never-called provider (the CI guard).
    pub async fn replay(&self, client: &Client) -> Result<Outcome> {
        let events = Arc::new(EventStore::open(self.work_dir.join("events.db"))?);
        let session = events.latest_session()?.ok_or_else(|| {
            flux_core::Error::Other(format!(
                "scenario fixture {} has no recorded session",
                self.source_dir.display()
            ))
        })?;
        let mut sink = NullSink;
        let report = flux_flow::replay::replay_session(
            &events,
            &client.engine.executor,
            &session,
            None,
            &mut sink,
        )
        .await
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
        Outcome::new(events, session, report, self.source_dir.clone())
    }

    /// **Fault door.** Build a variant engine over this scenario's isolated work copy, fork the
    /// recorded session at statement `node` (skipping the op that produced its bound value), inject
    /// `value` in its place, and run the diverged tail through `client`'s executor's real envelope.
    /// Returns a [`Counterfactual`] to assert on (`assert_compensated_with`/`assert_diverges_at`).
    pub async fn inject_at(
        &self,
        client: &Client,
        node: u32,
        value: &serde_json::Value,
    ) -> Result<Counterfactual> {
        let events = Arc::new(EventStore::open(self.work_dir.join("events.db"))?);
        let flow = FlowStore::open(self.work_dir.join("flow.db"), events.clone())?;
        let session_id = events.latest_session()?.ok_or_else(|| {
            flux_core::Error::Other(format!(
                "scenario fixture {} has no recorded session",
                self.source_dir.display()
            ))
        })?;

        let engine =
            client
                .assembly
                .variant_with_stores(VariantOverrides::default(), events, flow)?;
        let session = Session {
            engine: Arc::new(engine),
            id: session_id,
            assembly: client.assembly.clone(),
            turn_guard: Arc::new(tokio::sync::Mutex::new(())),
            // A fixture replayed into a throwaway work dir is never a crashed production session.
            auto_resurrect: false,
        };
        let fork = session.fork(node as usize).await?;
        let mut sink = NullSink;
        fork.inject(value, &mut sink).await?;
        Ok(Counterfactual::from_fork(session, fork))
    }

    /// **Engine 2 — world-pinned re-drive.** Re-run the scenario's recorded turn against a live
    /// `client` with BOTH the world and the model pinned: every leaf op is served from the golden
    /// [`FrozenTape`] under `OffTape::Halt` (a miss latches — the frozen world is the whole world,
    /// same as `check`'s honesty contract), and every model call is served from the golden
    /// `model.jsonl` by a [`ServingProvider`], falling through to `client`'s own real provider on a
    /// miss (counted, never silent). A clean re-drive costs $0 and reproduces the recorded plan
    /// exactly; drift shows up classified in the returned [`Report`] instead of a bare pass/fail —
    /// this is what tells a config edit (a prompt tweak, a new op registration) apart from an actual
    /// behavior regression.
    ///
    /// `FLUX_GOLDEN=update` re-records the fixture in place (delegating to
    /// [`record`](Self::record)) instead of checking it — the same re-baseline convention every
    /// other Test Kit assertion follows. This is a live re-baseline, never a pinned re-drive, so the
    /// returned [`Report`] is **never** clean (D-184): it carries the honest diff between the
    /// previous golden and the freshly-recorded one, and `model_live` counts the live calls the
    /// re-recording turn actually made (never a fabricated `1`) — a guard that reads `is_clean()`
    /// must reject a `FLUX_GOLDEN=update` run exactly as it would reject any other live fall-through.
    pub async fn check(&self, client: &Client) -> Result<Report> {
        if std::env::var("FLUX_GOLDEN").as_deref() == Ok("update") {
            // Capture the outgoing golden's trace/text *before* `record` overwrites the fixture in
            // place, so the report reflects a real diff instead of a hardcoded "nothing changed".
            let previous_events = EventStore::open(self.source_dir.join("events.db"))?;
            let previous_trace = previous_events.run_trace(&self.manifest.session)?;
            let previous_turns = previous_events.turns(&self.manifest.session)?;
            drop(previous_events);

            let (updated, live_calls) =
                Self::record_with_call_count(client, &self.manifest.input, &self.source_dir)
                    .await?;

            let new_events = EventStore::open(self.source_dir.join("events.db"))?;
            let new_session = &updated.manifest.session;
            let new_trace = new_events.run_trace(new_session)?;
            let new_turns = new_events.turns(new_session)?;

            let diff = flux_events::run_diff(&previous_trace, &new_trace);
            let plan_changed = diff.rows.iter().any(|r| matches!(r, DiffRow::Plan { .. }));

            let mut texts_turns = previous_turns;
            texts_turns.extend(new_turns);
            let texts = flux_events::stmt_texts(&texts_turns);

            return Ok(Report {
                diff,
                plan_changed,
                // A re-baseline never stays on a pinned, hermetic world — it is a live re-record by
                // definition — so it must never read as a clean CI pass no matter what the diff says.
                left_world: true,
                model_served: 0,
                model_live: live_calls,
                texts,
            });
        }

        let golden_events = Arc::new(EventStore::open(self.source_dir.join("events.db"))?);
        let golden_trace = golden_events.run_trace(&self.manifest.session)?;
        if golden_trace.is_empty() {
            return Err(flux_core::Error::Other(format!(
                "scenario fixture {} has no recorded run trace to check against",
                self.source_dir.display()
            )));
        }
        let tape = ReplayTape::from_trace(&golden_trace);
        let frozen = FrozenTape::hermetic(tape);
        let scope = Arc::new(CassetteScope::Frozen(frozen));

        let redactor = client.engine.executor.context().redactor.clone();
        let model_calls = read_model_calls(&self.source_dir)?;
        let serving = Arc::new(ServingProvider::new(
            client.assembly.provider.clone(),
            redactor,
            model_calls,
        ));

        let events = Arc::new(EventStore::open(self.work_dir.join("events.db"))?);
        let flow = FlowStore::open(self.work_dir.join("flow.db"), events.clone())?;
        let engine = client.assembly.variant_with_stores(
            VariantOverrides {
                provider: Some(serving.clone()),
                ..Default::default()
            },
            events.clone(),
            flow,
        )?;

        let cf_session = events.create_session_with_context(
            &engine.model,
            &EventContext {
                correlation_id: Some(self.manifest.session.clone()),
                agent_id: Some(format!("what_if:check@{}", self.manifest.name)),
                ..Default::default()
            },
        )?;
        // Under `Frozen(Halt)` every dispatch is SERVED, and a served dispatch is deliberately never
        // re-recorded by the cassette (nothing ran live, so there is no live tail). Without a
        // self-recorder the re-drive's own session would hold no cells at all and diff as "every
        // statement vanished" — so record the served trail onto it, exactly as `rerun_pinned` does.
        let mut inner = NullSink;
        let mut sink = flux_flow::whatif::RerunRecordingSink::new(
            &mut inner,
            RecordScope::new(events.clone(), cf_session.clone()),
            client.engine.executor.context().redactor.clone(),
            true,
        );
        engine
            .run_turn_pinned(&cf_session, &self.manifest.input, scope.clone(), &mut sink)
            .await
            .map_err(|e| flux_core::Error::Other(e.to_string()))?;

        let cf_trace = events.run_trace(&cf_session)?;
        let diff = flux_events::run_diff(&golden_trace, &cf_trace);
        let plan_changed = diff.rows.iter().any(|r| matches!(r, DiffRow::Plan { .. }));
        let latched = matches!(scope.as_ref(), CassetteScope::Frozen(f) if f.diverged().is_some());
        let left_world = latched
            || diff
                .rows
                .iter()
                .any(|r| matches!(r, DiffRow::Output { .. }));

        let mut texts_turns = golden_events.turns(&self.manifest.session)?;
        texts_turns.extend(events.turns(&cf_session)?);
        let texts = flux_events::stmt_texts(&texts_turns);

        Ok(Report {
            diff,
            plan_changed,
            left_world,
            model_served: serving.served(),
            model_live: serving.live(),
            texts,
        })
    }

    /// **Judge door (D-195).** Grade `target` against `criterion` with an LLM judge — the
    /// complementary axis to [`replay`](Self::replay)'s exact/deterministic plan assertions, for
    /// TEXT outputs that don't have one canonical answer.
    ///
    /// The judge's own model call flows through this fixture's cassette exactly like an agent
    /// model call: its canonical (redacted) request is hashed and looked up in `judge.jsonl`
    /// first. A HIT is served straight from disk — `client`'s provider is never touched, so a
    /// plain `cargo test` replay costs nothing (the same hermeticity proof `replay` uses: pair
    /// `client` with a provider that panics if ever invoked, and show it never panics). A MISS
    /// (the first time this exact call is made, or `criterion`/`target`/`rubric.model` changed
    /// since the last recording) is a **hard error** unless `FLUX_GOLDEN=update` is set — never a
    /// silent live fall-through, and never a silent pass against a stale grade: a regressed agent
    /// answer changes the hash, so the next plain call fails loudly demanding a re-record instead
    /// of quietly reusing yesterday's verdict. With `FLUX_GOLDEN=update`, the call is made live
    /// against `client`'s real provider (spends once) and the fresh verdict is committed to
    /// `judge.jsonl`.
    ///
    /// `rubric.model` is always explicit — there is no default judge model, so no assertion can
    /// spend without the caller naming a target for it.
    pub async fn judge(
        &self,
        client: &Client,
        criterion: &str,
        target: &str,
        rubric: &Rubric,
    ) -> Result<Verdict> {
        let redactor = client.engine.executor.context().redactor.clone();
        let req = judge_request(&rubric.model, criterion, target);
        let (canonical, hash) = redact_and_hash_request(&req, &redactor)?;

        if let Some(record) = read_judge_calls(&self.source_dir)?
            .into_iter()
            .find(|r| r.hash == hash)
        {
            return parse_verdict(&chunks_to_text(&record.chunks));
        }

        let updating = std::env::var("FLUX_GOLDEN").as_deref() == Ok("update");
        if !updating {
            return Err(flux_core::Error::Other(format!(
                "no recorded judge verdict for this call in {} — the judged text, criterion, or \
                 rubric model changed (or this is the first run); set FLUX_GOLDEN=update to \
                 record a fresh verdict against a live judge provider",
                self.source_dir.join("judge.jsonl").display()
            )));
        }

        use futures::stream::TryStreamExt;
        let raw_chunks: Vec<Chunk> = client
            .assembly
            .provider
            .stream(req)
            .await?
            .try_collect()
            .await?;
        let chunks = raw_chunks
            .iter()
            .map(|c| redact_chunk(c, &redactor))
            .collect::<Result<Vec<_>>>()?;

        append_judge_call(
            &self.source_dir,
            ModelCallRecord {
                v: 1,
                hash,
                model: rubric.model.clone(),
                request: canonical,
                chunks: chunks.clone(),
            },
        )?;

        parse_verdict(&chunks_to_text(&chunks))
    }

    /// Panicking convenience over [`judge`](Self::judge): panics on an IO/cache-miss error (with
    /// the same re-record hint), and panics with the judge's own rationale if the verdict itself
    /// is a FAIL ([`Verdict::assert_pass`]).
    pub async fn assert_judge(
        &self,
        client: &Client,
        criterion: &str,
        target: &str,
        rubric: &Rubric,
    ) -> Verdict {
        let verdict = self
            .judge(client, criterion, target, rubric)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        verdict.assert_pass();
        verdict
    }
}

// --- Report (D-176: Scenario::check) ------------------------------------------

/// The classified result of [`Scenario::check`] — a world-and-model-pinned re-drive against the
/// recorded golden. `is_clean()` is the whole-report pass/fail a CI guard reads; the individual
/// fields tell a config edit (`plan_changed`, harmless — the model asked for something different on
/// purpose) apart from an actual behavior regression (`left_world`, a different world served a
/// different answer to the SAME plan) apart from a plain honesty failure (`model_live > 0`, the
/// golden cassette didn't cover a request and real money was spent — never reported clean, D-184).
#[non_exhaustive]
pub struct Report {
    /// The aligned per-statement diff between the golden run and this re-drive.
    pub diff: RunDiff,
    /// Whether any statement's PLAN content differs from the golden (`DiffRow::Plan`) — the model
    /// (or a substituted prompt) asked for something different.
    pub plan_changed: bool,
    /// Whether the re-drive left the pinned world at all: the `FrozenTape` latched a divergence
    /// (`OffTape::Halt`, so nothing ran live — a dispatch simply had no matching recorded cell), OR
    /// the diff shows an `Output` row (the SAME statement recorded a different op outcome).
    pub left_world: bool,
    /// How many model calls were served from the golden `model.jsonl` — $0, deterministic.
    pub model_served: usize,
    /// How many model calls fell through to `client`'s real provider (a request the golden doesn't
    /// cover) — the honesty counter: `0` on a fully-pinned, $0 re-drive.
    pub model_live: usize,
    texts: HashMap<String, String>,
}

impl Report {
    /// Whether this re-drive reproduced the golden exactly: no plan changed, the world was never
    /// left, AND no call fell through to the real provider (`model_live == 0`, D-184). A re-drive
    /// that quietly reached the live model — a golden that no longer covers every request — is a
    /// hard fail here even when the plan and world otherwise line up: a CI guard reading this must
    /// never let a real, spend-incurring model call pass as "clean".
    pub fn is_clean(&self) -> bool {
        !self.plan_changed && !self.left_world && self.model_live == 0
    }

    /// Render [`Self::diff`] as human-readable lines (`flux_events::render_run_diff`, resolved
    /// against the golden's + this re-drive's own statement text).
    pub fn render(&self) -> Vec<String> {
        flux_events::render_run_diff(&self.diff, &self.texts)
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }
}

// --- Outcome -------------------------------------------------------------------

/// The result of [`Scenario::replay`] — assertions panic with a rendered plan-source (+ divergence
/// detail, when present), matching the `cargo test` failure idiom rather than propagating a
/// `Result` through the ordinary test harness.
pub struct Outcome {
    events: Arc<EventStore>,
    session: String,
    report: flux_flow::replay::ReplayReport,
    fixture_dir: PathBuf,
    calls: Vec<String>,
    text: String,
    current_plan: String,
}

impl Outcome {
    fn new(
        events: Arc<EventStore>,
        session: String,
        report: flux_flow::replay::ReplayReport,
        fixture_dir: PathBuf,
    ) -> Result<Self> {
        let calls = events
            .run_trace(&session)?
            .into_iter()
            .filter_map(|ev| match ev {
                RunEvent::OpRecorded { op, .. } => Some(op),
                _ => None,
            })
            .collect();
        let text = events
            .turns(&session)?
            .last()
            .and_then(|t| t.answer.clone())
            .unwrap_or_default();
        let current_plan = accepted_plan_text(&events, &session)?;
        Ok(Self {
            events,
            session,
            report,
            fixture_dir,
            calls,
            text,
            current_plan,
        })
    }

    /// Whether the replay was faithful: no latched divergence, and every recorded cell was
    /// consumed. A truncated cell is a diagnostic here, never a silent pass — its message is the
    /// same actionable "re-record with a larger `FLUX_CASSETTE_MAX_BYTES`" text
    /// [`ReplayTape`](flux_flow::cassette::ReplayTape) latches.
    pub fn faithful(&self) -> std::result::Result<(), String> {
        if let Some(reason) = &self.report.diverged {
            return Err(format!(
                "replay diverged: {reason}\n\n{}",
                self.render_plan()
            ));
        }
        if self.report.cells_consumed != self.report.cells_total {
            return Err(format!(
                "replay left {} of {} recorded cassette cells unconsumed — the run diverged \
                 silently\n\n{}",
                self.report.cells_total - self.report.cells_consumed,
                self.report.cells_total,
                self.render_plan()
            ));
        }
        Ok(())
    }

    /// Panicking form of [`faithful`](Self::faithful).
    pub fn assert_faithful(&self) {
        if let Err(msg) = self.faithful() {
            panic!("{msg}");
        }
    }

    /// Assert the replayed run's leaf-op call sequence is exactly `ops`, in order.
    pub fn assert_calls(&self, ops: &[&str]) {
        let got: Vec<&str> = self.calls.iter().map(String::as_str).collect();
        assert_eq!(
            got,
            ops,
            "recorded op call sequence mismatch\n\n{}",
            self.render_plan()
        );
    }

    /// Assert `op` never appears in the replayed run's leaf-op call sequence — the "my agent never
    /// runs `shell.exec`" safety assertion.
    pub fn assert_never_calls(&self, op: &str) {
        assert!(
            !self.calls.iter().any(|c| c == op),
            "expected `{op}` never to run, but it did (calls: {:?})\n\n{}",
            self.calls,
            self.render_plan()
        );
    }

    /// Assert the turn's final assistant text contains `s`.
    pub fn assert_text_contains(&self, s: &str) {
        assert!(
            self.text.contains(s),
            "expected the turn's answer to contain {s:?}, got: {:?}",
            self.text
        );
    }

    /// Assert the replayed session's priced cost (via
    /// [`PricingTable::builtin`](flux_core::PricingTable::builtin)) is under `usd`.
    pub fn assert_cost_under(&self, usd: f64) {
        let pricing = PricingTable::builtin();
        let rows = self
            .events
            .cost_summary(&self.session, &pricing)
            .expect("cost_summary");
        let total: f64 = rows.iter().filter_map(|r| r.cost.map(|m| m.usd)).sum();
        assert!(
            total < usd,
            "expected session cost under ${usd}, got ${total:.6}"
        );
    }

    /// The replayed run's canonical accepted-plan text — what
    /// [`assert_plan_snapshot`](Self::assert_plan_snapshot) compares against the fixture's committed
    /// `plan.flux.snap`, and what a non-`cargo test` gate (`flux test`) renders on a regression.
    pub fn plan_source(&self) -> &str {
        &self.current_plan
    }

    /// The leaf ops the replayed run called, in order.
    pub fn calls(&self) -> &[String] {
        &self.calls
    }

    /// The turn's final assistant text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Non-panicking form of [`assert_plan_snapshot`](Self::assert_plan_snapshot): `Err` carries the
    /// unified line diff. `FLUX_GOLDEN=update` rewrites the fixture and returns `Ok`, exactly as the
    /// panicking form does — so `flux test` and `cargo test` re-baseline identically.
    pub fn plan_snapshot(&self) -> std::result::Result<(), String> {
        if std::env::var("FLUX_GOLDEN").as_deref() == Ok("update") {
            return std::fs::write(self.fixture_dir.join("plan.flux.snap"), &self.current_plan)
                .map_err(|e| format!("rewrite plan.flux.snap: {e}"));
        }
        let golden =
            std::fs::read_to_string(self.fixture_dir.join("plan.flux.snap")).unwrap_or_default();
        if golden != self.current_plan {
            return Err(format!(
                "plan snapshot mismatch (set FLUX_GOLDEN=update to rewrite {}):\n\n{}",
                self.fixture_dir.join("plan.flux.snap").display(),
                line_diff(&golden, &self.current_plan)
            ));
        }
        Ok(())
    }

    /// insta-style snapshot assertion: compare the replayed run's canonical accepted-plan text
    /// against the fixture's committed `plan.flux.snap`. `FLUX_GOLDEN=update` rewrites the fixture
    /// in place instead of panicking.
    ///
    /// # Panics
    /// On mismatch (unless `FLUX_GOLDEN=update`), with a unified line diff.
    pub fn assert_plan_snapshot(&self) {
        if let Err(msg) = self.plan_snapshot() {
            panic!("{msg}");
        }
    }

    /// Assert the replay was faithful ([`Self::faithful`]) — the world-diff half of the fault-door
    /// contract lives on [`crate::whatif::Counterfactual`]; here, "faithful" already IS the
    /// assertion that no world diverged from what was recorded.
    fn render_plan(&self) -> String {
        let mut out = format!("canonical plan:\n{}", self.current_plan);
        if let Some(reason) = &self.report.diverged {
            out.push_str(&format!("\n\ndivergence: {reason}"));
        }
        out
    }
}

/// A minimal hand-rolled unified-ish line diff (no external dep) — good enough for a snapshot
/// mismatch panic: every line position where old/new differ gets a `-`/`+` pair.
fn line_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut out = String::new();
    for i in 0..old_lines.len().max(new_lines.len()) {
        match (old_lines.get(i), new_lines.get(i)) {
            (Some(a), Some(b)) if a == b => out.push_str(&format!("  {a}\n")),
            (Some(a), Some(b)) => out.push_str(&format!("- {a}\n+ {b}\n")),
            (Some(a), None) => out.push_str(&format!("- {a}\n")),
            (None, Some(b)) => out.push_str(&format!("+ {b}\n")),
            (None, None) => {}
        }
    }
    out
}

/// Render a [`RunDiff`] into text lines with a caller-supplied [`DiffLineKind`] prefix — a thin
/// convenience over [`flux_events::render_run_diff`] for callers that just want plain text (no
/// color/kind branching). Kept here (not on `Outcome`) since D-176's `Report` also needs it.
#[allow(dead_code)]
pub(crate) fn plain_diff_lines(diff: &RunDiff, texts: &HashMap<String, String>) -> Vec<String> {
    flux_events::render_run_diff(diff, texts)
        .into_iter()
        .map(|(kind, line)| match kind {
            DiffLineKind::Same | DiffLineKind::Plan | DiffLineKind::Output => line,
            _ => line,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-184: a plain unit fixture for `Report`, isolated from the async engine plumbing —
    /// constructing every field directly (this module, unlike a downstream crate, isn't blocked by
    /// `#[non_exhaustive]`) lets the `is_clean()` pinning test below vary exactly one field at a
    /// time instead of relying on an end-to-end `check()` to happen to produce the right shape.
    fn report(plan_changed: bool, left_world: bool, model_live: usize) -> Report {
        Report {
            diff: RunDiff {
                rows: Vec::new(),
                identical: true,
            },
            plan_changed,
            left_world,
            model_served: 0,
            model_live,
            texts: HashMap::new(),
        }
    }

    /// The regression this story closes: before D-184, `is_clean()` was `!plan_changed &&
    /// !left_world` — a `check()` re-drive that quietly fell through to the real provider
    /// (`model_live > 0`) but otherwise reproduced the golden read as clean. A CI guard that gates
    /// on `is_clean()` (the doc comment's own stated contract) must hard-fail that case.
    #[test]
    fn is_clean_hard_fails_on_any_live_model_fall_through() {
        assert!(
            report(false, false, 0).is_clean(),
            "no plan drift, no world drift, no live calls — genuinely clean"
        );
        assert!(
            !report(false, false, 1).is_clean(),
            "a single live fall-through must never be reported clean, even with plan/world intact"
        );
        assert!(
            !report(false, false, 7).is_clean(),
            "many live fall-throughs must never be reported clean either"
        );
    }

    /// `plan_changed`/`left_world` still independently fail the guard — D-184 adds a THIRD
    /// condition, it doesn't relax the first two.
    #[test]
    fn is_clean_still_fails_on_plan_or_world_drift_alone() {
        assert!(!report(true, false, 0).is_clean());
        assert!(!report(false, true, 0).is_clean());
        assert!(!report(true, true, 0).is_clean());
    }

    /// A `CI-style guard` reading only the boolean, exactly as the doc comment on `Report` promises
    /// callers may — proves the fix is usable as a gate, not just an internal invariant.
    #[test]
    fn a_ci_style_guard_rejects_a_live_fall_through() {
        fn ci_guard(report: &Report) -> Result<()> {
            if report.is_clean() {
                Ok(())
            } else {
                Err(flux_core::Error::Other(
                    "check() drifted from its golden".into(),
                ))
            }
        }

        assert!(ci_guard(&report(false, false, 0)).is_ok());
        assert!(
            ci_guard(&report(false, false, 1)).is_err(),
            "a guard built on is_clean() must reject a live fall-through"
        );
    }
}
