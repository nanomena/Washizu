//! Platform-specific protocol bridges.
//!
//! A `Bridge` translates between a game platform's wire protocol and the
//! mjai JSONL event stream consumed by AI bots. One bridge instance per
//! independent game session (e.g. one Majsoul WebSocket flow).

pub mod majsoul;
pub mod riichi_city;
pub mod tenhou;

pub use majsoul::MajsoulBridge;
pub use riichi_city::RiichiCityBridge;
pub use tenhou::TenhouBridge;

use crate::{
    logger::{FlowLogger, Session},
    schema::{MjaiEvent, ParsedFrame},
};
use std::sync::Arc;

/// Direction of a parsed frame relative to the proxied client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server (uplink, e.g. requests).
    Up,
    /// Server → client (downlink, e.g. responses, notifies).
    Down,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// Result of parsing one wire frame.
///
/// `events` are the mjai events the frame translated into (zero or more).
/// `parsed` is the bridge's first-pass structured view of the frame —
/// Majsoul's decoded protobuf method+payload, Tenhou's `{tag, …}` JSON
/// dict — surfaced for the inspector so a developer can see what the
/// bridge thought the frame meant. Bridges that can't decode a particular
/// frame (handshake, unsupported method, malformed payload) return `None`.
#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    pub window: Option<crate::bot::control::InputWindow>,
    pub events: Vec<MjaiEvent>,
    pub parsed: Option<ParsedFrame>,
}

impl ParseResult {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn just_events(events: Vec<MjaiEvent>) -> Self {
        Self {
            window: None,
            events,
            parsed: None,
        }
    }

    pub fn into_captured(self) -> impl Iterator<Item = crate::event_bus::CapturedEvent> {
        let last = self.events.len().checked_sub(1);
        self.events
            .into_iter()
            .enumerate()
            .map(move |(i, event)| crate::event_bus::CapturedEvent {
                event,
                window: (Some(i) == last).then_some(self.window).flatten(),
            })
    }
}

/// Translates raw platform frames to mjai events and vice-versa.
pub trait Bridge: Send {
    /// Parse a raw platform frame into zero or more mjai events plus an
    /// optional structured view for the inspector.
    fn parse(&mut self, direction: Direction, content: &[u8]) -> ParseResult;

    /// Build a raw platform frame from an mjai command, if applicable.
    fn build(&mut self, command: &MjaiEvent) -> Option<Vec<u8>>;
}

/// Slots the autoplay layer shares with a bridge.
///
/// Every field is optional and platform-specific: the chromium capture path
/// wires the browser-page slots (the MITM path has no `Page` handle), the
/// MITM path wires the frame-injection slot, and each bridge fills in the
/// subset its own autoplay needs.
/// Bundled into one struct so adding a platform's slot doesn't grow the
/// argument list of every constructor along the way.
#[derive(Clone, Default)]
pub struct BridgeHooks {
    /// Majsoul: the server's per-decision-window time budget, taken from
    /// `OptionalOperationList` (see `autoplay::budget`).
    pub time_budget: Option<crate::autoplay::budget::SharedTimeBudget>,
    /// Majsoul: counter bumped on every uplink input command, so autoplay can
    /// verify a click registered (see `autoplay::verify`).
    pub input_watch: Option<crate::autoplay::verify::SharedInputWatch>,
    /// Tenhou: hand at Tenhou tile-index resolution plus the current decision
    /// window, needed to encode a client frame (see `autoplay::tenhou_state`).
    pub tenhou_state: Option<crate::autoplay::tenhou_state::SharedTenhouState>,
    /// Riichi City: frame-injection gate, maintained by the bridge between
    /// `cmd_enter_room` and `cmd_room_end` (see `autoplay::inject`).
    pub riichi_inject: Option<crate::autoplay::inject::SharedInjectBus>,
    /// Frontend toast channel, for states a bridge must tell the user about
    /// that no mjai event can carry (Tenhou: a mid-hand rejoin pauses
    /// analysis until the next hand). Wired by both capture backends.
    pub notify: Option<crate::event_bus::NotifyBus>,
    /// Majsoul: where the player's own ranks go when the server says them
    /// (login, a game's start, the account asked for again after a game).
    /// Wired by the capture backend.
    pub account: Option<crate::event_bus::AccountBus>,
    /// Majsoul: folder an opened replay is exported to as a tenhou.net/6 log
    /// and NAGA's order-form lines (see `bridge::majsoul::tenhou_log`). Set by
    /// the embedded capture to `<home>/replays`.
    pub replay_dir: Option<std::path::PathBuf>,
}

/// Construct a bridge for the given platform.
///
/// - `flow_log`: per-WS-flow text dump (one JSON line per parsed message).
/// - `session`: passed through to bridges that open additional log files
///   on demand (e.g. Majsoul rotates a fresh `*.mjai.jsonl` per game).
/// - `hooks`: autoplay's shared slots — see [`BridgeHooks`].
pub fn for_platform(
    platform: crate::config::Platform,
    flow_log: Option<Arc<FlowLogger>>,
    session: Option<Arc<Session>>,
    hooks: BridgeHooks,
) -> Box<dyn Bridge> {
    match platform {
        crate::config::Platform::Majsoul => Box::new(
            MajsoulBridge::new(flow_log, session)
                .with_time_budget(hooks.time_budget)
                .with_input_watch(hooks.input_watch)
                .with_account_bus(hooks.account)
                .with_replay_dir(hooks.replay_dir),
        ),
        crate::config::Platform::Tenhou => Box::new(
            TenhouBridge::new(flow_log, session)
                .with_shared_state(hooks.tenhou_state)
                .with_input_watch(hooks.input_watch)
                .with_notify(hooks.notify),
        ),
        crate::config::Platform::RiichiCity => {
            Box::new(RiichiCityBridge::new(flow_log, session).with_inject(hooks.riichi_inject))
        }
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;
    #[test]
    fn only_the_complete_frame_prefix_gets_the_producer_window() {
        use crate::autoplay::{BudgetSource, TimeBudget};
        let window = crate::bot::control::InputWindow::Majsoul(TimeBudget {
            fixed_ms: 5000,
            add_ms: 0,
            opened_at: std::time::Instant::now(),
            source: BudgetSource::DealTile,
        });
        for tail in [
            r#"{"type":"dora","dora_marker":"9p"}"#,
            r#"{"type":"reach_accepted","actor":1}"#,
        ] {
            let last = serde_json::from_str(tail).unwrap();
            let captured: Vec<_> = ParseResult {
                events: vec![MjaiEvent::None, last],
                parsed: None,
                window: Some(window),
            }
            .into_captured()
            .collect();
            assert_eq!(captured[0].window, None);
            assert_eq!(captured[1].window, Some(window));
        }
    }
}
