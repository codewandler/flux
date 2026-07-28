//! A protocol test fixture: a plugin that speaks a wire protocol this host does not.
//!
//! It answers structurally valid frames but stamps a *different* protocol marker, which is exactly
//! what a plugin built against a future breaking wire revision would do. The host must reject it
//! with an actionable message rather than failing somewhere downstream (C-144). Build target name
//! `future_protocol_plugin`; used by the host integration test.

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else {
            break;
        };
        let id = frame["id"].as_str().unwrap_or("0");
        // Deliberately NOT `flux_plugin_protocol::PROTOCOL`: this fixture exists to be incompatible.
        let response = serde_json::json!({
            "protocol": "flux.plugin.v99",
            "id": id,
            "type": "response",
            "command": frame["command"].as_str().unwrap_or(""),
            "ok": true,
            "result": {"name": "from-the-future", "operations": []}
        });
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}
