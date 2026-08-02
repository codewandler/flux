//! D-232 — the browser harness's own guts, checked without a browser.
//!
//! D-208 shipped the seam and could not exercise a browser; `room_media.rs` drives the flux side of
//! the wire against a scripted double. This file covers the half that lives in the *sidecar*, and it
//! exists because of one sentence in D-208's report: **"a sidecar that hardcodes `rms: 0.5`
//! passes."** flux enforces the floor and cannot verify the number, so the honesty of the level
//! probe is the only thing standing between the seam and a silent demo.
//!
//! What can and cannot be checked here is worth stating exactly, because the temptation is to write
//! a test that passes by not testing:
//!
//! - **Checkable in CI:** that the arithmetic the page runs is right. A sine of amplitude `a` has
//!   RMS `a/√2` analytically, so the measurement code has a correct answer that needs no browser —
//!   and these tests run it as `assets/room-media/measure.js`, *the file the page loads*, rather
//!   than against a reimplementation that would agree with itself no matter what shipped.
//! - **Not checkable in CI, and `#[ignore]`d rather than faked:** that a real Chrome publishes
//!   audible audio into a real room. Those tests need a browser and the network; each carries the
//!   exact command to run it by hand, and the story records the results.
//!
//! Behind `--features room-media`; `scripts/check-feature-gated-tests.sh` runs it in CI.
#![cfg(feature = "room-media")]

use std::path::{Path, PathBuf};
use std::process::Command;

use flux_channels::rooms::media::{AudioChunk, MediaPeer, MediaSidecarConfig, SidecarMediaPeer};
use flux_channels::rooms::{BraveTalkTokens, JaasTokens, RoomId, RoomIdentity};
use flux_system::sandbox::{Sandbox, SandboxMode, SandboxSettings};
use flux_system::{System, Workspace};

/// The sidecar's asset directory — the files the harness loads at runtime.
fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/room-media")
}

/// Run a snippet of Node with `measure.js` required, printing one JSON line, and parse it.
///
/// **A missing `node` fails these tests rather than skipping them**, and that is deliberate. The
/// first version of this file returned early with an `eprintln!` — but libtest captures stderr for
/// *passing* tests, so with `node` off `PATH` the suite reported `ok. 4 passed` and the word
/// "skipped" appeared nowhere without `--nocapture`. Three of the four legs no-op'd while the gate
/// read green, which is precisely the defect class this suite exists to catch, turned on itself.
///
/// `node` is not a flux dependency: it is the language the sidecar is written in, so without it
/// there is no harness to check and a green result would be a lie. `check-feature-gated-tests.sh`
/// marks this suite `run`, and a `run` leg that silently declines to run is worse than one that is
/// honestly absent — so the absence is loud.
fn node_eval(script: &str) -> serde_json::Value {
    let measure = assets().join("measure.js");
    assert!(
        measure.is_file(),
        "the page's measurement module must ship with the crate: {}",
        measure.display()
    );
    let program = format!(
        "const M = require({});\n{script}",
        serde_json::to_string(&measure.to_string_lossy()).unwrap()
    );
    let output = Command::new("node")
        .arg("-e")
        .arg(&program)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "`node` must be on PATH to check the sidecar's measurement code, and this suite is \
                 marked `run` in scripts/check-feature-gated-tests.sh: {e}. Install Node, or move \
                 that ledger row off `run` and say why — do not let it pass by not testing."
            )
        });
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected one JSON line, got {stdout:?}: {e}"))
}

/// Run a snippet with the sidecar loaded as a module. `sidecar.js` keeps its executable entry point
/// behind `require.main === module`, so the exact argument, integrity and diagnostic helpers used by
/// the live process remain testable without starting Chrome.
fn sidecar_eval(script: &str) -> serde_json::Value {
    let sidecar = assets().join("sidecar.js");
    let program = format!(
        "const S = require({});\n(async () => {{ {script} }})().catch((error) => {{ \
         console.error(error); process.exit(1); }});",
        serde_json::to_string(&sidecar.to_string_lossy()).unwrap()
    );
    let output = Command::new("node")
        .arg("-e")
        .arg(&program)
        .output()
        .expect("node must be on PATH to check the shipped sidecar");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected one JSON line, got {stdout:?}: {e}"))
}

/// The probe's arithmetic is right, checked against an answer derived rather than recorded.
///
/// This is the test D-208 asked for and could not write. A sine of amplitude `a` has RMS `a/√2` —
/// `0.5` reads `0.3536`, and no hardcoded constant in the sidecar can satisfy the whole table at
/// once. The live-browser half of the same claim is
/// [`the_in_page_probe_measures_a_real_track`] below, which measures these same amplitudes through a
/// real `MediaStreamTrack`; on 2026-08-02 that run reported `0.3550` for amplitude `0.5` — the same
/// number to three places, which is what says the in-page path and this one agree.
#[test]
fn the_level_probe_computes_a_real_rms_rather_than_reporting_a_constant() {
    // Amplitude → expected RMS (a/√2). A constant-returning probe fails every row but one.
    let script = r#"
      const rows = [];
      for (const amplitude of [1.0, 0.5, 0.12, 0.01, 0.0]) {
        const n = 4096, rate = 48000;
        const frame = new Float32Array(n);
        for (let i = 0; i < n; i++) {
          frame[i] = amplitude * Math.sin((2 * Math.PI * 440 * i) / rate);
        }
        rows.push({ amplitude, ...M.frameLevel(frame) });
      }
      console.log(JSON.stringify(rows));
    "#;
    let rows = node_eval(script);

    for row in rows.as_array().expect("rows") {
        let amplitude = row["amplitude"].as_f64().unwrap();
        let rms = row["rms"].as_f64().unwrap();
        let peak = row["peak"].as_f64().unwrap();
        let expected = amplitude / 2.0_f64.sqrt();
        assert!(
            (rms - expected).abs() < 0.01,
            "amplitude {amplitude} must measure RMS ≈{expected:.4}, got {rms:.4} \
             — a probe that reports a constant fails here"
        );
        assert!(
            (peak - amplitude).abs() < 0.01,
            "amplitude {amplitude} must peak at ≈{amplitude}, got {peak:.4}"
        );
    }

    // The two values the 2026-07-30 call measured, held against flux's floor (0.01): silence must
    // land below it and audible speech above it, or the floor separates nothing.
    let script = r#"
      const silence = new Float32Array(4096);
      const speech = new Float32Array(4096);
      for (let i = 0; i < speech.length; i++) {
        speech[i] = 0.17 * Math.sin((2 * Math.PI * 220 * i) / 48000);
      }
      console.log(JSON.stringify({
        silence: M.frameLevel(silence),
        speech: M.frameLevel(speech),
      }));
    "#;
    let measured = node_eval(script);
    assert!(
        measured["silence"]["rms"].as_f64().unwrap() < 0.01,
        "digital silence must read below flux's floor: {measured}"
    );
    assert!(
        measured["speech"]["rms"].as_f64().unwrap() > 0.01,
        "audio a human confirmed hearing must read above it: {measured}"
    );
}

/// An unmeasurable track stays unmeasurable all the way to flux.
///
/// `verify_audible` refuses `NaN` because `rms > floor` is false for it — but only if the sidecar
/// actually reports the `NaN` instead of sanitizing it to `0.0` or dropping the frame. A probe that
/// "cleaned up" its own bad measurement would hand flux a number flux then has to trust.
#[test]
fn an_unmeasurable_frame_is_reported_as_unmeasurable_not_as_silence() {
    let script = r#"
      const broken = new Float32Array([0.1, NaN, 0.2, 0.3]);
      const loud = new Float32Array(64).fill(0.5);
      console.log(JSON.stringify({
        frame: M.frameLevel(broken),
        // A NaN frame anywhere in the window must not be hidden by a louder neighbour.
        window: M.windowLevel([loud, broken, loud]),
      }));
    "#;
    let measured = node_eval(script);
    assert!(
        measured["frame"]["rms"].is_null() || !measured["frame"]["rms"].is_number(),
        "NaN must survive as NaN (JSON null), not become 0.0: {measured}"
    );
    assert!(
        measured["window"]["rms"].is_null() || !measured["window"]["rms"].is_number(),
        "a NaN frame must not be masked by louder frames around it: {measured}"
    );
}

/// The PCM16 decode agrees with what flux encoded, including the asymmetric full-scale bound.
///
/// The wire carries little-endian bytes, not samples. A byte-order or scaling error here is exactly
/// the class of bug that publishes a track which measures healthy and sounds like noise.
#[test]
fn pcm16_decodes_little_endian_at_full_scale_without_clipping() {
    let script = r#"
      const decode = (b64) => Buffer.from(b64, "base64");
      // 0, +1, -1, +32767, -32768 as PCM16 LE.
      const bytes = Buffer.alloc(10);
      [0, 1, -1, 32767, -32768].forEach((v, i) => bytes.writeInt16LE(v, i * 2));
      const samples = Array.from(M.pcm16ToFloat(bytes.toString("base64"), decode));
      console.log(JSON.stringify(samples));
    "#;
    let samples = node_eval(script);
    let samples: Vec<f64> = samples
        .as_array()
        .expect("samples")
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(samples.len(), 5, "five samples in, five out: {samples:?}");
    assert!(samples[0].abs() < 1e-9, "zero stays zero: {samples:?}");
    assert!(samples[1] > 0.0, "+1 must decode positive: {samples:?}");
    assert!(samples[2] < 0.0, "-1 must decode negative: {samples:?}");
    assert!(
        (samples[3] - 1.0).abs() < 1e-4,
        "+32767 is full scale: {samples:?}"
    );
    assert!(
        (samples[4] + 1.0).abs() < 1e-9,
        "-32768 must reach exactly -1.0 and not clip past it: {samples:?}"
    );
}

/// D-236: the token uses the argv seam that survives flux's cleared subprocess environment, reaches
/// the join options, and is redacted from an error before that error can become a protocol reply.
#[test]
fn the_room_token_reaches_join_from_argv_without_becoming_a_diagnostic() {
    let measured = sidecar_eval(
        r#"
          const secret = "jwt.D236.super-secret";
          const options = S.parseArgs(["--token", secret]);
          const join = S.buildJoinOptions(
            { room: "standup@conference.tenant.8x8.vc", nick: "flux" }, options,
          );
          const diagnostic = S.safeError(new Error(`join failed for ${secret}`), [secret]);
          console.log(JSON.stringify({ token: join.token, diagnostic }));
        "#,
    );
    assert_eq!(measured["token"], "jwt.D236.super-secret");
    let diagnostic = measured["diagnostic"].as_str().unwrap();
    assert!(diagnostic.contains("[REDACTED]"), "{diagnostic}");
    assert!(!diagnostic.contains("super-secret"), "{diagnostic}");

    let sidecar = std::fs::read_to_string(assets().join("sidecar.js")).unwrap();
    assert!(
        !sidecar.contains("process.env.FLUX_ROOM_TOKEN"),
        "a credential env path cannot survive flux-system's env_clear"
    );
}

/// D-237: bytes whose digest is not the pinned digest are refused before the evaluator sees them.
#[test]
fn lib_jitsi_meet_integrity_mismatch_is_refused_before_execution() {
    let measured = sidecar_eval(
        r#"
          const fs = require("node:fs");
          const os = require("node:os");
          const path = require("node:path");
          const source = path.join(os.tmpdir(), `flux-d237-${process.pid}.js`);
          fs.writeFileSync(source, "globalThis.JitsiMeetJS={}");
          let evaluated = false;
          let error = null;
          try {
            await S.injectJitsi(
              async () => { evaluated = true; },
              source,
              "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
              null,
            );
          } catch (caught) {
            error = String(caught.message || caught);
          } finally {
            fs.unlinkSync(source);
          }
          console.log(JSON.stringify({ evaluated, error }));
        "#,
    );
    assert_eq!(measured["evaluated"], false, "unverified bytes executed");
    let error = measured["error"].as_str().unwrap();
    assert!(error.contains("integrity mismatch"), "{error}");
    assert!(error.contains("flux-d237-"), "{error}");
}

/// Network acquisition belongs to flux-system's guarded egress seam, never to the spawned Node
/// process. Even a digest-checked URL would be a bypass of host authorization and DNS pinning.
#[test]
fn the_sidecar_refuses_every_network_library_source_before_execution() {
    let measured = sidecar_eval(
        r#"
          let evaluated = false;
          let error = null;
          try {
            await S.injectJitsi(
              async () => { evaluated = true; },
              "https://8x8.vc/libs/lib-jitsi-meet.min.js",
              "sha256-CfA+2dA/TH3EaR2eh4H5hyyolmDAelna1cKSyD+JoOE=",
              "6869",
            );
          } catch (caught) { error = String(caught.message || caught); }
          console.log(JSON.stringify({ evaluated, error }));
        "#,
    );
    assert_eq!(measured["evaluated"], false);
    let error = measured["error"].as_str().unwrap();
    assert!(error.contains("network sources are refused"), "{error}");
    assert!(error.contains("guarded host path"), "{error}");
}

/// The shipped no-argument path names an exact tenant release and digest; a moving URL alone is not
/// a version pin.
#[test]
fn the_default_jitsi_source_is_release_and_integrity_pinned() {
    let measured = sidecar_eval(
        r#"
          const options = S.parseArgs([]);
          console.log(JSON.stringify({
            source: options.jitsi,
            release: options.jitsiRelease,
            integrity: options.jitsiIntegrity,
          }));
        "#,
    );
    assert_eq!(measured["release"], "6869");
    assert_eq!(
        measured["integrity"],
        "sha256-CfA+2dA/TH3EaR2eh4H5hyyolmDAelna1cKSyD+JoOE="
    );
    assert!(
        measured["source"].is_null(),
        "the no-argument path must not fetch remote code"
    );
}

/// D-235: a correct-looking Pulse path that the process cannot reach must name both the missing
/// socket and the companion sandbox grant. It must never degrade into a healthy handshake + zero RMS.
#[test]
fn an_unreachable_audio_socket_names_the_mask_and_required_grant() {
    let measured = sidecar_eval(
        r#"
          const socket = "unix:/run/user/424242/pulse/native";
          let error = null;
          try { await S.preflightAudioServer(socket); }
          catch (caught) { error = String(caught.message || caught); }
          console.log(JSON.stringify({ error }));
        "#,
    );
    let error = measured["error"].as_str().unwrap();
    assert!(error.contains("/run/user/424242/pulse/native"), "{error}");
    assert!(error.contains("sandbox masks `/run`"), "{error}");
    assert!(error.contains("[sandbox] writable"), "{error}");
    assert!(error.contains("/run/user/424242/pulse"), "{error}");
}

/// The full D-235 composition: the same sidecar argv is generic under the `/run` mask and becomes
/// routable only when the socket directory is explicitly bound back into confinement.
///
/// ```bash
/// cargo test -p flux-channels --features room-media --test room_media_harness \
///     -- --ignored the_pulse_socket_grant_changes_the_real_sandboxed_sidecar --nocapture
/// ```
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs bwrap, Chrome, Node, pactl and the host Pulse socket"]
async fn the_pulse_socket_grant_changes_the_real_sandboxed_sidecar() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let pulse_dir = PathBuf::from(runtime).join("pulse");
    let audio_server = format!("unix:{}/native", pulse_dir.display());
    assert!(
        pulse_dir.join("native").exists(),
        "host Pulse socket is absent"
    );

    let sidecar_argv = vec![
        "/usr/bin/node".into(),
        assets().join("sidecar.js").to_string_lossy().into_owned(),
        "--audio-server".into(),
        audio_server,
    ];
    let spawn = |writable: Vec<PathBuf>| {
        let sandbox = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            extra_writable: writable,
        });
        assert!(sandbox.is_active(), "a real sandbox backend is required");
        let system = System::new(Workspace::new(root).unwrap()).with_sandbox(sandbox);
        let mut config = MediaSidecarConfig::new(sidecar_argv.clone());
        config.handshake_timeout = std::time::Duration::from_secs(15);
        (system, config)
    };

    let (masked_system, masked_config) = spawn(Vec::new());
    let masked = SidecarMediaPeer::spawn(&masked_system, masked_config)
        .await
        .expect("a diagnosed routing failure still completes the handshake");
    let error = masked
        .publish_audio(&AudioChunk::mono(vec![0; 960], 48_000))
        .await
        .expect_err("the `/run` mask must stop publication")
        .to_string();
    assert!(
        error.contains(&pulse_dir.to_string_lossy().to_string()),
        "{error}"
    );
    assert!(error.contains("[sandbox] writable"), "{error}");

    let (granted_system, granted_config) = spawn(vec![pulse_dir]);
    let granted = SidecarMediaPeer::spawn(&granted_system, granted_config)
        .await
        .expect("the explicitly bound socket reaches the same sidecar");
    assert!(
        granted.owns_device_routing(),
        "the companion writable grant must change the real routing result"
    );
}

/// D-232's final acceptance check. This reaches only a Brave Talk room the operator created and
/// names in `FLUX_LIVE_BRAVE_ROOM`; the ignored test mints the guest JWT internally so no credential
/// is pasted into a prompt or printed. A human must already be listening in that room and must
/// confirm the three-second tone was audible — the passing RMS probe is necessary, not sufficient.
///
/// ```bash
/// FLUX_LIVE_BRAVE_ROOM=<owned-room-handle> \
/// FLUX_LIVE_JITSI_PATH=/tmp/lib-jitsi-meet-6869.min.js \
/// cargo test -p flux-channels --features room-media --test room_media_harness \
///     -- --ignored audible_audio_reaches_an_owned_live_brave_room --nocapture
/// ```
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs an operator-owned live Brave room, a listening human, bwrap, Chrome, Node and Pulse"]
async fn audible_audio_reaches_an_owned_live_brave_room() {
    let room = std::env::var("FLUX_LIVE_BRAVE_ROOM")
        .expect("set FLUX_LIVE_BRAVE_ROOM to a Brave Talk room you created");
    let room = room
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|room| !room.is_empty())
        .expect("the room handle must not be empty");

    let tokens = BraveTalkTokens::new();
    let token = tokens
        .guest_token(room)
        .await
        .expect("mint a guest token for the operator-owned room");
    let conference = tokens
        .conference(room, &token)
        .await
        .expect("allocate conference focus");
    let jitsi = std::env::var("FLUX_LIVE_JITSI_PATH")
        .expect("set FLUX_LIVE_JITSI_PATH to the reviewed release-6869 bundle");

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let pulse_dir = PathBuf::from(runtime).join("pulse");
    let audio_server = format!("unix:{}/native", pulse_dir.display());
    let sandbox = Sandbox::resolve(SandboxSettings {
        mode: SandboxMode::On,
        network: true,
        extra_writable: vec![pulse_dir],
    });
    assert!(sandbox.is_active(), "a real sandbox backend is required");
    let system = System::new(Workspace::new(root).unwrap()).with_sandbox(sandbox);
    let mut config = MediaSidecarConfig::new(vec![
        "/usr/bin/node".into(),
        assets().join("sidecar.js").to_string_lossy().into_owned(),
        "--audio-server".into(),
        audio_server,
        "--token".into(),
        token.jwt().into(),
        "--jitsi".into(),
        jitsi,
    ]);
    config.command_timeout = std::time::Duration::from_secs(30);
    let peer = SidecarMediaPeer::spawn(&system, config)
        .await
        .expect("start the confined browser sidecar");
    let _stream = peer
        .join(
            &RoomId::new(conference.room_jid),
            &RoomIdentity::agent("flux"),
        )
        .await
        .expect("join the live media conference");

    // Three seconds at 440 Hz and amplitude 0.20. Long enough for the listener to identify, quiet
    // enough not to be hostile; the expected RMS is ~0.141, comfortably above the 0.01 floor.
    let mut pcm = Vec::with_capacity(48_000 * 3 * 2);
    for sample in 0..(48_000 * 3) {
        let phase = 2.0 * std::f32::consts::PI * 440.0 * sample as f32 / 48_000.0;
        let value = (phase.sin() * 0.20 * i16::MAX as f32) as i16;
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    peer.publish_audio(&AudioChunk::mono(pcm, 48_000))
        .await
        .expect("publish the audible tone");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let level = peer
        .verify_publish_carries_signal()
        .await
        .expect("the published track carries measured signal");
    println!(
        "LIVE AUDIO PROBE rms={:.4} peak={:.4}; listener must confirm the tone was audible",
        level.rms, level.peak
    );
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    peer.leave().await.expect("leave the live media conference");
}

/// The harness never names a capture device in the protocol, and never touches the default source.
///
/// Both are D-206/D-208 findings with teeth. The protocol's silence about devices is pinned on the
/// flux side (`protocol.rs::the_protocol_never_names_a_capture_device`); this is the sidecar side of
/// the same contract, plus the property that makes the recipe safe to run on a machine someone is
/// using: routing is **per-stream** (`move-source-output`), so the human's own microphone in the
/// same call never moves.
#[test]
fn the_harness_routes_per_stream_and_never_moves_the_default_source() {
    let asset_dir = assets();
    let mut files = std::fs::read_dir(&asset_dir)
        .unwrap_or_else(|e| panic!("{} must ship: {e}", asset_dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("js"))
        .collect::<Vec<_>>();
    files.sort();

    let sidecar = asset_dir.join("sidecar.js");
    let sidecar_source = std::fs::read_to_string(&sidecar)
        .unwrap_or_else(|e| panic!("{} must ship: {e}", sidecar.display()));

    assert!(
        sidecar_source.contains("move-source-output"),
        "per-stream routing is the recipe D-206 measured; nothing else worked"
    );
    assert!(!files.is_empty(), "the room-media asset directory is empty");
    for file in files {
        let source = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("{} must be readable: {e}", file.display()));
        let executable = strip_javascript_comments(&source)
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>();
        for forbidden in [
            "set-default-source",
            "set-default-sink",
            // The flags Chrome 150 ignores. Shipping them would look like device handling and do nothing.
            "use-fake-device-for-media-capture",
            "use-file-for-fake-audio-capture",
            // `setAudioInputDevice` reported the right label and did not stick.
            "setAudioInputDevice",
        ] {
            let call = format!("{forbidden}(");
            assert!(
                !executable.contains(&call),
                "`{forbidden}` is a measured dead end (D-206/D-208) and must not be called by any \
                 shipped room-media asset; checked {}",
                file.display()
            );
        }
    }
}

/// Remove comments before the routing-call census. The shipped rationale deliberately names the
/// dead APIs; a guard that confuses that explanation with executable code teaches maintainers to
/// delete the explanation or weaken the guard.
fn strip_javascript_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let mut quote = None;
    while i < bytes.len() {
        if quote.is_none() && i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if quote.is_none() && i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        let ch = bytes[i] as char;
        if let Some(delimiter) = quote {
            if ch == '\\' && i + 1 < bytes.len() {
                out.push(ch);
                i += 1;
                out.push(bytes[i] as char);
            } else if ch == delimiter {
                quote = None;
                out.push(ch);
            } else {
                out.push(ch);
            }
        } else {
            if matches!(ch, '\'' | '"' | '`') {
                quote = Some(ch);
            }
            out.push(ch);
        }
        i += 1;
    }
    out
}

// ── the half that needs a browser, and is ignored rather than faked ──────────────────────────────

/// The in-page probe measures a **real `MediaStreamTrack`**, not the samples it was handed.
///
/// This is the claim D-208 could not make and the reason D-232 exists. It needs a real Chrome, so
/// CI cannot run it. Run it by hand:
///
/// ```bash
/// cargo test -p flux-channels --features room-media --test room_media_harness \
///     -- --ignored the_in_page_probe_measures_a_real_track --nocapture
/// ```
///
/// Measured on 2026-08-02 (Chrome 150.0.7871.46, headless=new): amplitude `0.5` → `rms 0.3550`,
/// `peak 0.5000` (analytic answer `0.3536`); digital silence → `rms 0.0000`. The same run under
/// flux's bubblewrap policy returned `rms 0.3550`, which is what says the probe survives
/// confinement.
#[test]
#[ignore = "needs a real Chrome — no browser in CI; see the doc comment for the command"]
fn the_in_page_probe_measures_a_real_track() {
    let probe = assets().join("selftest.js");
    let output = Command::new("node")
        .arg(&probe)
        .output()
        .expect("node and Chrome must be present for the ignored browser tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("{stdout}");
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "the in-page self-test failed");

    let measured: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .last()
            .expect("the self-test prints one JSON line last"),
    )
    .expect("the self-test's last line is JSON");

    // A real track, measured through a second AudioContext — so the number cannot be the input.
    assert_eq!(measured["kind"], "audio", "{measured}");
    let tone = measured["tone"]["rms"].as_f64().unwrap();
    assert!(
        (tone - 0.3536).abs() < 0.02,
        "a 0.5-amplitude tone must measure ≈0.3536 through a real track, got {tone}: {measured}"
    );
    assert!(
        measured["silence"]["rms"].as_f64().unwrap() < 0.01,
        "silence through the same path must fall below flux's floor: {measured}"
    );
}
