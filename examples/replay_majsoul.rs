//! Replay a local inspector capture through the real bridge, without a client or network.
//!
//! Give it a session's full timeline, `<session>/sensitive/inspector.jsonl`:
//! the shareable `inspector.jsonl` beside it withholds the frames the bridge
//! needs to start a game (the `authGame` request among them).
//!
//! With a second argument, every replay opened during the capture is exported
//! there as a tenhou.net/6 log and NAGA's order-form lines, as the app does.
use akagi::bridge::{Bridge, Direction, MajsoulBridge};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .context("usage: replay_majsoul <session>/sensitive/inspector.jsonl [replay-dir]")?;
    let replay_dir = args.next().map(PathBuf::from);
    let mut bridges = HashMap::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let row: Value = serde_json::from_str(&line?)?;
        if row["kind"] != "ws_frame" || row["raw"]["format"] != "binary" {
            continue;
        }
        let flow = row["flow_id"].as_str().context("frame has no flow id")?;
        if !row["parsed"]["method"]
            .as_str()
            .is_some_and(|method| method.starts_with(".lq."))
        {
            continue;
        }
        let direction = match row["direction"].as_str() {
            Some("up") => Direction::Up,
            Some("down") => Direction::Down,
            _ => anyhow::bail!("unknown frame direction"),
        };
        let bytes = STANDARD.decode(
            row["raw"]["data"]
                .as_str()
                .context("frame has no binary data")?,
        )?;
        let bridge = bridges
            .entry(flow.to_owned())
            .or_insert_with(|| MajsoulBridge::new(None, None).with_replay_dir(replay_dir.clone()));
        for event in bridge.parse(direction, &bytes).events {
            println!("{}", json!({"flow_id": flow, "event": event}));
        }
    }
    Ok(())
}
