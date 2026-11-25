//! One captured frame on its way into the core: its flow's bridge parses it,
//! the inspector timeline records it, and its mjai events go on the bus. Every
//! frame source takes this one path.

use crate::bridge::{Direction, ParseResult};
use crate::capture::flow::SharedBridge;
use crate::event_bus::MjaiBus;
use crate::inspector::InspectorWriter;
use crate::schema::{FrameDirection, FrameRaw, InspectorEntry};
use chrono::Local;

/// Hand `payload` to `bridge`, record it as `raw` under `flow_id`, and publish
/// the events it translated into.
pub fn deliver(
    bridge: &SharedBridge,
    direction: Direction,
    payload: &[u8],
    raw: FrameRaw,
    flow_id: String,
    inspector: &InspectorWriter,
    mjai_bus: &MjaiBus,
) {
    let result = bridge
        .lock()
        .expect("bridge mutex poisoned")
        .parse(direction, payload);
    record(inspector, direction, flow_id, payload.len(), raw, &result);
    for event in result.into_captured() {
        let _ = mjai_bus.send(event);
    }
}

/// One inspector `WsFrame` entry. Its content reaches the shareable copy only
/// when the bridge marked the frame `shareable`.
fn record(
    inspector: &InspectorWriter,
    direction: Direction,
    flow_id: String,
    size: usize,
    raw: FrameRaw,
    result: &ParseResult,
) {
    inspector.record_as(
        InspectorEntry::WsFrame {
            ts_ms: Local::now().timestamp_millis(),
            direction: match direction {
                Direction::Up => FrameDirection::Up,
                Direction::Down => FrameDirection::Down,
            },
            flow_id,
            size,
            raw,
            parsed: result.parsed.clone(),
            emitted: result.events.len(),
        },
        result.shareable,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames off the bridge's allowlist keep only metadata in the shareable
    /// inspector file; the full frame goes under `sensitive/`.
    #[test]
    fn record_follows_the_bridge_allowlist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (inspector, _) = InspectorWriter::open(
            &tmp.path().join("inspector.jsonl"),
            &tmp.path().join("full.jsonl"),
            8,
        )
        .unwrap();
        let secret = r#"{"tag":"HELO","auth":"fake-auth"}"#;
        let mut result = ParseResult {
            parsed: Some(crate::schema::ParsedFrame {
                method: "HELO".into(),
                args: serde_json::from_str(secret).unwrap(),
            }),
            ..Default::default()
        };
        let write = |result: &ParseResult, raw: &str| {
            record(
                &inspector,
                Direction::Down,
                "ws:1".into(),
                raw.len(),
                FrameRaw::Text(raw.into()),
                result,
            )
        };
        write(&result, secret);
        let init = r#"{"tag":"INIT","seed":"fake-seed"}"#;
        result.shareable = true;
        result.parsed = Some(crate::schema::ParsedFrame {
            method: "INIT".into(),
            args: serde_json::from_str(init).unwrap(),
        });
        write(&result, init);
        let read = |name: &str| std::fs::read_to_string(tmp.path().join(name)).unwrap();
        let shared = read("inspector.jsonl");
        assert!(!shared.contains("fake-auth"), "{shared}");
        assert!(shared.contains("HELO") && shared.contains("fake-seed"));
        assert!(read("full.jsonl").contains("fake-auth"));
    }
}
