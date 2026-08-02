#!/usr/bin/env node
// sidecar.js — the `flux.room-media.v1` sidecar: a headless Chrome running `lib-jitsi-meet`, driven
// over CDP, speaking flux's NDJSON control protocol on stdin/stdout (D-232).
//
// This is the half D-208 deliberately did not ship. flux spawns it through
// `flux_system::System::spawn_interactive` — argv-only, cwd-pinned, environment **cleared** — and
// speaks one JSON object per line to it. Everything Linux-specific lives here, which is what keeps
// the flux-side port portable (`protocol.rs` names no device, sink, source or audio server).
//
// ## Usage
//
//   flux-room-media --audio-server unix:/run/user/1000/pulse/native \
//                   --token <jaas-guest-jwt> \
//                   [--chrome /usr/bin/google-chrome-stable] \
//                   --jitsi <local-path> [--jitsi-release 6869] \
//                   [--jitsi-integrity sha256-…] [--no-sandbox] [--probe-window-ms 300]
//
// ## The cleared environment, and why every host fact is a flag
//
// `DISPLAY`, `XDG_RUNTIME_DIR`, `PULSE_SERVER` and `PULSE_RUNTIME_PATH` **do not reach this
// process** — flux clears the environment to PATH/HOME/LANG/TERM/TZ/USER/TMPDIR. That is the rule
// working, not a bug to route around, so anything about the host arrives in argv and this file
// re-exports what Chrome itself needs into *its* environment. `HOME` and `USER` do survive, so
// `--audio-server` is defaulted from the conventional `/run/user/<uid>/pulse/native` rather than
// being mandatory. The JaaS credential is also an argv flag: flux deliberately clears secret env
// variables, and `MediaSidecarConfig` redacts argv after the program in every `Debug` rendering.
//
// ## Measured on 2026-08-02, and both answers changed the shape of this file
//
// 1. **Chrome runs fine under flux's bubblewrap policy.** `--headless=new` rendered a page inside
//    the exact `bwrap` argv `flux-system` builds (`--unshare-pid --ro-bind / / --dev /dev --proc
//    /proc --tmpfs /run`), and a nested user namespace was creatable inside it — so Chrome's own
//    content sandbox works and `--no-sandbox` is **not** required. It stays available as a flag for
//    hosts that refuse nesting, but it is off by default: forcing it would trade Chrome's
//    purpose-built sandbox for nothing.
// 2. **`--tmpfs /run` masks the Pulse socket, so argv alone is not enough.** The socket path *is*
//    `/run/user/<uid>/pulse/native`, and the sandbox masks `/run` wholesale to keep docker.sock and
//    D-Bus unreachable. `pactl --server=unix:/run/user/1000/pulse/native` therefore fails
//    `Connection refused` **inside** the sandbox while succeeding outside it. Passing the path in
//    argv — the story's stated route — is necessary but not sufficient: the operator must also grant
//    the socket's directory as a writable sandbox path, which re-exposes it past the mask. The
//    runbook in the story records the one-line config. No env passthrough was added to flux, and
//    none is needed.

"use strict";

const { spawn, execFile } = require("node:child_process");
const { createHash } = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const readline = require("node:readline");

const PROTOCOL = "flux.room-media.v1";
const MEASURE_JS = path.join(__dirname, "measure.js");
const PAGE_JS = path.join(__dirname, "page.js");

// ── argv ─────────────────────────────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const uid = typeof process.getuid === "function" ? process.getuid() : 1000;
  const options = {
    chrome: process.env.CHROME || "/usr/bin/google-chrome-stable",
    audioServer: `unix:/run/user/${uid}/pulse/native`,
    token: null,
    // 8x8's release 6869, fetched and measured 2026-08-02. The URL may move; the release header and
    // digest may not. Updating this snapshot is an explicit reviewed change, never a runtime drift.
    // No ambient network execution: joining requires an explicit local source, while the reviewed
    // tenant release and integrity stay pinned as the defaults for that opt-in. The sidecar may not
    // bypass flux-system's guarded egress by fetching code itself.
    jitsi: null,
    jitsiRelease: "6869",
    jitsiIntegrity: "sha256-CfA+2dA/TH3EaR2eh4H5hyyolmDAelna1cKSyD+JoOE=",
    noSandbox: false,
    probeWindowMs: 300,
    nick: "flux",
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const next = () => {
      const value = argv[++i];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      return value;
    };
    switch (arg) {
      case "--audio-server": options.audioServer = next(); break;
      case "--chrome": options.chrome = next(); break;
      case "--token": options.token = next(); break;
      case "--jitsi": options.jitsi = next(); break;
      case "--jitsi-release": options.jitsiRelease = next(); break;
      case "--jitsi-integrity": options.jitsiIntegrity = next(); break;
      case "--nick": options.nick = next(); break;
      case "--probe-window-ms": options.probeWindowMs = Number(next()); break;
      // Off by default: Chrome's own sandbox works under flux's bwrap policy (measured). This is for
      // a host that refuses nested user namespaces, and it is the operator's call, not ours.
      case "--no-sandbox": options.noSandbox = true; break;
      default:
        // Unknown flags are the operator's business, not ours to reject — but say so on stderr,
        // which flux inherits, rather than silently ignoring them.
        process.stderr.write(`flux-room-media: ignoring unknown argument ${arg}\n`);
    }
  }
  return options;
}

// ── the protocol ─────────────────────────────────────────────────────────────────────────────────

/// One NDJSON line on stdout. The only way anything leaves this process toward flux.
function write(object) {
  process.stdout.write(`${JSON.stringify(object)}\n`);
}

// Diagnostics go to **stderr**, never stdout: stdout is the protocol. flux skips and counts
// non-protocol stdout lines rather than dying on them, but relying on that would be sloppy.
function log(message) {
  process.stderr.write(`flux-room-media: ${message}\n`);
}

/// Remove known credentials before an exception can cross stderr or the NDJSON error seam.
function safeError(error, secrets) {
  let message = String((error && error.message) || error);
  for (const secret of secrets) {
    if (secret) message = message.split(secret).join("[REDACTED]");
  }
  return message;
}

// ── Chrome over CDP ──────────────────────────────────────────────────────────────────────────────

/// Launch Chrome and return a CDP session attached to one page target.
async function launchChrome(options) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "flux-room-media-"));
  const argv = [
    "--headless=new",
    "--remote-debugging-port=0",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    // A synthesized track still counts as autoplay; without this the graph starts suspended and
    // publishes silence — which would look exactly like the failure the level probe exists to catch.
    "--autoplay-policy=no-user-gesture-required",
    `--user-data-dir=${profile}`,
    "about:blank",
  ];
  if (options.noSandbox) argv.push("--no-sandbox");

  // The environment Chrome needs, rebuilt from argv rather than inherited — because flux cleared it.
  // `PULSE_SERVER` is how the browser's audio reaches the host's server; `XDG_RUNTIME_DIR` is
  // derived from the same socket path so Chrome's own lookups agree with it.
  const env = { ...process.env, PULSE_SERVER: options.audioServer };
  const socket = options.audioServer.replace(/^unix:/, "");
  const runtimeDir = path.dirname(path.dirname(socket));
  if (runtimeDir.startsWith("/run/")) env.XDG_RUNTIME_DIR = runtimeDir;

  const child = spawn(options.chrome, argv, { env, stdio: ["ignore", "pipe", "pipe"] });
  child.on("exit", (code) => log(`chrome exited ${code}`));

  const url = await new Promise((resolve, reject) => {
    let buffered = "";
    const timer = setTimeout(
      () => reject(new Error(`chrome printed no DevTools URL: ${buffered.slice(-400)}`)),
      30000,
    );
    child.stderr.on("data", (chunk) => {
      buffered += chunk.toString();
      const match = buffered.match(/DevTools listening on (ws:\/\/\S+)/);
      if (match) {
        clearTimeout(timer);
        resolve(match[1]);
      }
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`chrome exited ${code} before listening: ${buffered.slice(-400)}`));
    });
  });

  const ws = new WebSocket(url);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error("could not open the DevTools socket"));
  });

  let nextId = 0;
  const pending = new Map();
  ws.onmessage = (message) => {
    const frame = JSON.parse(message.data);
    if (frame.id && pending.has(frame.id)) {
      const { resolve, reject } = pending.get(frame.id);
      pending.delete(frame.id);
      frame.error ? reject(new Error(JSON.stringify(frame.error))) : resolve(frame.result);
    }
  };
  const call = (method, params, sessionId) =>
    new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params, sessionId }));
    });

  // Runtime lives on a page session, not on the browser endpoint.
  const { targetId } = await call("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await call("Target.attachToTarget", { targetId, flatten: true });
  await call("Runtime.enable", {}, sessionId);

  /// Evaluate an expression in the page, awaiting a promise and returning by value.
  const evaluate = async (expression) => {
    const result = await call(
      "Runtime.evaluate",
      { expression, awaitPromise: true, returnByValue: true },
      sessionId,
    );
    if (result.exceptionDetails) {
      const detail =
        (result.exceptionDetails.exception && result.exceptionDetails.exception.description) ||
        result.exceptionDetails.text;
      throw new Error(`page: ${String(detail).split("\n")[0]}`);
    }
    return result.result.value;
  };

  return { child, ws, evaluate };
}

/// Load the exact reviewed `lib-jitsi-meet` bytes, then inject them only after integrity succeeds.
/// Only a local file is accepted. Network schemes are refused before IO so this child cannot bypass
/// flux-system's guarded-egress path; the local bytes still need the mandatory digest check.
async function injectJitsi(evaluate, source, expectedIntegrity, expectedRelease) {
  if (!source) {
    throw new Error(
      "lib-jitsi-meet source is required; opt in with --jitsi <local-path> and a reviewed integrity pin",
    );
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(source)) {
    throw new Error(
      `lib-jitsi-meet network sources are refused (${source}); obtain the reviewed bundle through ` +
        "a guarded host path and pass its local file with --jitsi",
    );
  }
  if (!expectedIntegrity || !expectedIntegrity.startsWith("sha256-")) {
    throw new Error(`lib-jitsi-meet integrity is required for ${source}`);
  }

  const bytes = fs.readFileSync(source);

  const observedIntegrity = `sha256-${createHash("sha256").update(bytes).digest("base64")}`;
  if (observedIntegrity !== expectedIntegrity) {
    throw new Error(
      `lib-jitsi-meet integrity mismatch for ${source}: expected ${expectedIntegrity}, ` +
        `received ${observedIntegrity}`,
    );
  }

  // Base64 keeps the bundle out of the CDP expression's syntax; the indirect eval preserves the
  // global scope the upstream browser bundle expects. Integrity was checked before this point.
  const encoded = bytes.toString("base64");
  await evaluate(`(() => {
    const raw = atob(${JSON.stringify(encoded)});
    const bytes = Uint8Array.from(raw, (char) => char.charCodeAt(0));
    (0, eval)(new TextDecoder().decode(bytes));
    if (!globalThis.JitsiMeetJS) throw new Error("lib-jitsi-meet loaded but exported nothing");
    return true;
  })()`);
  return `local:${source}`;
}

// ── device routing: per-stream, never the default source ─────────────────────────────────────────

const pactl = (args, audioServer) =>
  new Promise((resolve) => {
    execFile("pactl", ["--server", audioServer, ...args], (error, stdout) => {
      if (error) return resolve(null);
      resolve(stdout);
    });
  });

/// Fail before the handshake if the named Pulse server is masked or absent. A zero RMS is evidence
/// about a track; it is not an acceptable diagnostic for a socket the sandbox never exposed.
async function preflightAudioServer(audioServer) {
  const reachable = await pactl(["info"], audioServer);
  if (reachable !== null) return true;

  const socket = audioServer.replace(/^unix:/, "");
  const directory = path.dirname(socket);
  throw new Error(
    `audio server ${socket} is unreachable; flux's sandbox masks \`/run\` by default. ` +
      `Pass --audio-server ${audioServer} and add [sandbox] writable = [${JSON.stringify(directory)}] ` +
      `so the socket is bound into confinement`,
  );
}

/// Move **only Chrome's own** capture stream onto the agent's private source.
///
/// This is D-206's measured recipe and the reason it is safe to run on a machine someone is using:
/// `move-source-output` is **per-stream**. The default source is never touched, so the human in the
/// same call keeps their own microphone. Changing the server-wide default device is not done here and
/// must never be — `tests/room_media_harness.rs`'s
/// `the_harness_routes_per_stream_and_never_moves_the_default_source` greps this file for those
/// `pactl` verbs and fails if one appears.
///
/// Returns whether routing was established, which is what the handshake's `owns_device_routing`
/// reports. Saying `true` when this returned `false` would defeat the check flux uses it for.
async function routeOwnCaptureStream(audioServer, chromePid) {
  try {
    await preflightAudioServer(audioServer);
  } catch (error) {
    return { routed: false, why: safeError(error, []) };
  }
  const sinkName = `fluxagent_${process.pid}`;
  const sourceName = `${sinkName}_mic`;

  const sink = await pactl(
    ["load-module", "module-null-sink", `sink_name=${sinkName}`,
     `sink_properties=device.description=${sinkName}`],
    audioServer,
  );
  if (sink === null) return { routed: false, why: "could not load module-null-sink" };

  const source = await pactl(
    ["load-module", "module-remap-source", `source_name=${sourceName}`,
     `master=${sinkName}.monitor`, `source_properties=device.description=${sourceName}`],
    audioServer,
  );
  if (source === null) return { routed: false, why: "could not load module-remap-source" };

  // Find the source-outputs belonging to *our* Chrome, by pid, and move only those.
  const outputs = await pactl(["list", "source-outputs"], audioServer);
  const moved = [];
  if (outputs) {
    for (const block of outputs.split(/\n(?=Source Output #)/)) {
      const id = (block.match(/Source Output #(\d+)/) || [])[1];
      const pid = (block.match(/application\.process\.id = "(\d+)"/) || [])[1];
      if (!id || !pid) continue;
      // Chrome's capture happens in a child renderer/audio-service process, so match the whole
      // process group rather than the launcher pid alone.
      if (Number(pid) === chromePid || (await isDescendantOf(Number(pid), chromePid))) {
        if ((await pactl(["move-source-output", id, sourceName], audioServer)) !== null) {
          moved.push(id);
        }
      }
    }
  }

  return {
    routed: true,
    sink: sinkName,
    source: sourceName,
    movedStreams: moved,
    why: moved.length ? undefined : "no capture stream to move yet (synthesized track needs none)",
  };
}

/// Whether `pid` descends from `ancestor`, by walking `/proc/<pid>/stat`'s ppid.
async function isDescendantOf(pid, ancestor) {
  let current = pid;
  for (let hops = 0; hops < 12 && current > 1; hops++) {
    let stat;
    try {
      stat = fs.readFileSync(`/proc/${current}/stat`, "utf8");
    } catch {
      return false;
    }
    // The comm field can contain spaces and parentheses; ppid is the field after the last ')'.
    const ppid = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]);
    if (!Number.isFinite(ppid)) return false;
    if (ppid === ancestor) return true;
    current = ppid;
  }
  return false;
}

// ── the command loop ─────────────────────────────────────────────────────────────────────────────

async function main() {
  const options = parseArgs(process.argv.slice(2));

  const chrome = await launchChrome(options);
  const { evaluate } = chrome;

  // The page's own code, injected rather than served: no HTTP server, no origin, no network needed
  // for the harness itself.
  await evaluate(fs.readFileSync(MEASURE_JS, "utf8"));
  await evaluate(fs.readFileSync(PAGE_JS, "utf8"));
  await evaluate("FluxRoomMedia.setupAudio()");

  // Routing is established **before** the handshake, because the handshake's claim has to be true
  // when it is made — flux refuses to publish audio through a sidecar that has not taken ownership,
  // and the whole value of that check is that the claim is not aspirational.
  const routing = await routeOwnCaptureStream(options.audioServer, chrome.child.pid);
  if (!routing.routed) log(`device routing not established: ${routing.why}`);
  else log(`device routing: ${routing.source} (moved ${routing.movedStreams.length} stream(s))`);

  write({
    ready: PROTOCOL,
    owns_device_routing: routing.routed,
    ...(!routing.routed && routing.why ? { routing_error: routing.why } : {}),
  });

  // Forward queued in-page events as protocol lines.
  const pump = setInterval(async () => {
    try {
      const events = await evaluate("JSON.stringify(FluxRoomMedia.drainEvents())");
      for (const event of JSON.parse(events || "[]")) write(event);
    } catch {
      // A dead page will surface on the next command; the pump is not the place to fail.
    }
  }, 50);

  const handlers = {
    join: async (request) => {
      const jitsi = await injectJitsi(
        evaluate,
        options.jitsi,
        options.jitsiIntegrity,
        options.jitsiRelease,
      );
      log(`lib-jitsi-meet release ${options.jitsiRelease} from ${jitsi}`);
      const join = buildJoinOptions(request, options);
      const result = await evaluate(`FluxRoomMedia.join(${JSON.stringify(join)})`);
      return { ok: true, joined: result.joined };
    },
    leave: async () => {
      await evaluate("FluxRoomMedia.leave()");
      return { ok: true };
    },
    publish_audio: async (request) => {
      const audio = request.audio || {};
      await evaluate(
        `FluxRoomMedia.pushAudio(${JSON.stringify(audio.pcm16_le || "")},` +
          `${Number(audio.sample_rate_hz) || 48000},${Number(audio.channels) || 1})`,
      );
      return { ok: true };
    },
    publish_video: async () => {
      // D-211's half. Refused explicitly rather than answered `ok`: a silent no-op here would be the
      // same class of lie the level probe exists to catch.
      return { ok: false, error: "publish_video is not implemented by this sidecar (D-211)" };
    },
    mute: async (request) => {
      await evaluate(`FluxRoomMedia.setMuted(${Boolean(request.muted)})`);
      return { ok: true };
    },
    level: async () => {
      const level = await evaluate(`FluxRoomMedia.measure(${Number(options.probeWindowMs) || 300})`);
      // `rms`/`peak` are whatever the page measured — including `NaN`, which JSON renders as `null`
      // and flux's `rms > floor` check refuses. Not sanitized on purpose.
      return { ok: true, level: { rms: level.rms, peak: level.peak } };
    },
  };

  const lines = readline.createInterface({ input: process.stdin });
  for await (const line of lines) {
    const text = line.trim();
    if (!text) continue;
    let request;
    try {
      request = JSON.parse(text);
    } catch {
      log(`ignoring a line that is not JSON: ${text.slice(0, 120)}`);
      continue;
    }
    const handler = handlers[request.cmd];
    if (!handler) {
      write({ id: request.id, ok: false, error: `unknown command ${request.cmd}` });
      continue;
    }
    try {
      const reply = await handler(request);
      write({ id: request.id, ...reply });
    } catch (error) {
      // Every failure is an operation failure, with a reason — flux's posture, mirrored here.
      write({ id: request.id, ok: false, error: safeError(error, [options.token]) });
    }
  }

  clearInterval(pump);
  try {
    await evaluate("FluxRoomMedia.leave()");
  } catch {
    // Shutting down; the page may already be gone.
  }
  chrome.child.kill("SIGKILL");
}

/// Build the page's join options from the server-spelled MUC JID and the argv-resolved credential.
function buildJoinOptions(request, options) {
  const jid = String(request.room);
  const [local, domainPart] = jid.split("@");
  const domain = (domainPart || "").replace(/^conference\./, "");
  const tenant = domain.split(".")[0];
  return {
    room: local,
    nick: request.nick || options.nick,
    tenant,
    token: options.token,
    serviceUrl: `wss://8x8.vc/${tenant}/xmpp-websocket?room=${local}`,
    hosts: { domain, muc: domainPart },
  };
}

module.exports = {
  buildJoinOptions,
  injectJitsi,
  parseArgs,
  preflightAudioServer,
  safeError,
};

if (require.main === module) {
  main().catch((error) => {
    const tokenArg = parseArgs(process.argv.slice(2)).token;
    log(`fatal: ${safeError((error && error.stack) || error, [tokenArg])}`);
    process.exit(1);
  });
}
