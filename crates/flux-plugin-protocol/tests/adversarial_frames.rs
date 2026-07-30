//! Deterministic generated-input smoke for the exact framed-NDJSON decoder used by the plugin host.

use flux_plugin_protocol::{decode_ndjson_frame, Frame, FrameKind, PROTOCOL};
use serde_json::json;

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

fn cases() -> usize {
    std::env::var("FLUX_ADVERSARIAL_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(128)
        .clamp(1, 8192)
}

fn encoded(frame: &Frame) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(frame).unwrap();
    bytes.push(b'\n');
    bytes
}

#[test]
fn generated_plugin_frames_are_total_and_protocol_checked() {
    let corpus = [
        encoded(&Frame::request("r1", "manifest", json!(null))),
        encoded(&Frame::request(
            "r2",
            "operation.call",
            json!({"operation": "read", "input": {"path": "a\n🦀"}}),
        )),
        encoded(&Frame::ok_response("r1", json!({"operations": []}))),
        encoded(&Frame::err_response("r2", "fixture error")),
    ];
    assert_eq!(corpus.len(), 4, "the committed valid-frame corpus drifted");
    for fixture in &corpus {
        decode_ndjson_frame(fixture).expect("committed frame corpus must decode");
    }

    // These are prior failure classes at the real seam: missing newline, invalid UTF-8, missing
    // required fields, and a foreign protocol marker. Keeping them in the seed corpus makes the
    // oracle fail if a future decoder becomes permissive before generated mutations even run.
    let known_bad = [
        br#"{"protocol":"flux.plugin.v1"}"#.as_slice(),
        b"\xff\xfe\n".as_slice(),
        b"{}\n".as_slice(),
        b"{\"protocol\":\"future\",\"id\":\"r1\",\"type\":\"response\",\"command\":\"\",\"payload\":null,\"ok\":true,\"result\":null}\n".as_slice(),
    ];
    for fixture in known_bad {
        assert!(
            decode_ndjson_frame(fixture).is_err(),
            "known-bad frame was admitted: {fixture:?}"
        );
    }

    let mut rng = Rng(0xC264_5EED_D15C_A11E);
    for case in 0..cases() {
        let seed = &corpus[case % corpus.len()];
        let mut candidate = seed.clone();
        let operation = rng.next() % 4;
        match operation {
            0 => {
                let index = rng.next() as usize % candidate.len();
                candidate[index] ^= (rng.next() as u8) | 1;
            }
            1 => {
                let index = rng.next() as usize % candidate.len();
                candidate.insert(index, b'\n');
            }
            2 => {
                let new_len = rng.next() as usize % candidate.len();
                candidate.truncate(new_len);
            }
            _ => {
                let index = rng.next() as usize % candidate.len();
                candidate.insert(index, 0xff);
            }
        }

        // Rejection is normal. Any accepted candidate must still be a same-protocol typed frame;
        // most importantly, arbitrary bytes must never panic the decoder.
        if let Ok(frame) = decode_ndjson_frame(&candidate) {
            assert_eq!(frame.protocol, PROTOCOL, "case {case}");
            assert!(
                matches!(frame.kind, FrameKind::Request | FrameKind::Response),
                "case {case}"
            );
        }
    }
}
