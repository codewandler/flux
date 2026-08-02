#!/usr/bin/env node
// selftest.js — prove the level probe measures a **real `MediaStreamTrack`** (D-232).
//
// Needs a real Chrome, so CI cannot run it; it is driven by the `#[ignore]`d
// `tests/room_media_harness.rs::the_in_page_probe_measures_a_real_track`, and by hand:
//
//   node crates/flux-channels/assets/room-media/selftest.js
//
// It needs **no network and no room** — which is the point. It exercises exactly the code path a
// live call uses for the one claim flux cannot verify (`page.js`'s outbound graph and probe), and
// leaves only "does a human hear it" for the call itself.
//
// It prints a human-readable log, then **one JSON line last** for the Rust test to parse.
//
// Measured 2026-08-02, Chrome 150.0.7871.46:
//   amplitude 0.5 → rms 0.3550 peak 0.5000   (analytic 0.3536)
//   silence       → rms 0.0000 peak 0.0000
// and identically inside flux's bubblewrap policy, which is what says the probe survives
// confinement.

"use strict";

const { spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const CHROME = process.env.CHROME || "/usr/bin/google-chrome-stable";
const HERE = __dirname;

/// A 440 Hz tone as base64 PCM16 LE — the same shape flux puts on the wire.
function tone(amplitude, ms = 500, rate = 48000) {
  const count = Math.floor((rate * ms) / 1000);
  const bytes = Buffer.alloc(count * 2);
  for (let i = 0; i < count; i++) {
    bytes.writeInt16LE(Math.round(amplitude * 32767 * Math.sin((2 * Math.PI * 440 * i) / rate)), i * 2);
  }
  return bytes.toString("base64");
}

async function main() {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "flux-room-media-selftest-"));
  const chrome = spawn(
    CHROME,
    [
      "--headless=new", "--remote-debugging-port=0", "--disable-gpu", "--no-first-run",
      "--autoplay-policy=no-user-gesture-required", `--user-data-dir=${profile}`, "about:blank",
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );

  const url = await new Promise((resolve, reject) => {
    let buffered = "";
    const timer = setTimeout(() => reject(new Error("no DevTools URL")), 30000);
    chrome.stderr.on("data", (chunk) => {
      buffered += chunk.toString();
      const match = buffered.match(/DevTools listening on (ws:\/\/\S+)/);
      if (match) { clearTimeout(timer); resolve(match[1]); }
    });
    chrome.on("exit", (code) => { clearTimeout(timer); reject(new Error(`chrome exited ${code}`)); });
  });

  const ws = new WebSocket(url);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
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

  const { targetId } = await call("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await call("Target.attachToTarget", { targetId, flatten: true });
  await call("Runtime.enable", {}, sessionId);
  const evaluate = async (expression) => {
    const result = await call(
      "Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId,
    );
    if (result.exceptionDetails) {
      const detail =
        (result.exceptionDetails.exception && result.exceptionDetails.exception.description) ||
        result.exceptionDetails.text;
      throw new Error(`page: ${String(detail).split("\n")[0]}`);
    }
    return result.result.value;
  };

  // The shipped page code — not a copy of it.
  await evaluate(fs.readFileSync(path.join(HERE, "measure.js"), "utf8"));
  await evaluate(fs.readFileSync(path.join(HERE, "page.js"), "utf8"));
  const { sampleRate } = await evaluate("FluxRoomMedia.setupAudio()");
  const kind = await evaluate("FluxRoomMedia.outbound.track.kind");
  console.log(`outbound track: kind=${kind} sampleRate=${sampleRate}`);

  // Push a tone, then measure the *track*. The graph is real, the track is real, and the probe reads
  // it through a separate AudioContext — so this number cannot be the input echoed back.
  await evaluate(`FluxRoomMedia.pushAudio(${JSON.stringify(tone(0.5))}, 48000, 1)`);
  const toneLevel = await evaluate("FluxRoomMedia.measure(400)");
  console.log(`amplitude 0.5 → rms ${toneLevel.rms.toFixed(4)} peak ${toneLevel.peak.toFixed(4)} (analytic 0.3536)`);

  // Let the tone drain, then measure silence through the identical path.
  await new Promise((r) => setTimeout(r, 700));
  const silenceLevel = await evaluate("FluxRoomMedia.measure(400)");
  console.log(`silence      → rms ${silenceLevel.rms.toFixed(4)} peak ${silenceLevel.peak.toFixed(4)}`);

  ws.close();
  chrome.kill("SIGKILL");

  // One JSON line last, for the Rust test.
  console.log(JSON.stringify({ kind, sampleRate, tone: toneLevel, silence: silenceLevel }));
}

main().catch((error) => {
  console.error(`FAIL: ${(error && error.stack) || error}`);
  process.exit(1);
});
