//! Replay captured, sanitized Tenhou frames without a browser or network.
use akagi::bridge::{Bridge, Direction, TenhouBridge};
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader},
};
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: replay_tenhou frames.jsonl")?;
    let mut bridge = TenhouBridge::new(None, None);
    for line in BufReader::new(File::open(path)?).lines() {
        for event in bridge.parse(Direction::Down, line?.as_bytes()).events {
            println!("{}", serde_json::to_string(&event)?);
        }
    }
    Ok(())
}
