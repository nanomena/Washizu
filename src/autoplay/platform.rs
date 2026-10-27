//! Normalized semantic action, physical target and read-only site contracts.

use crate::autoplay::context::CanvasRect;
use crate::autoplay::pointer::PointerLayout;
use crate::game_state::snapshot::GameStateSnapshot;
use crate::schema::MjaiEvent;
use chromiumoxide::page::Page;
use riichienv_core::action::Action;
use std::time::Instant;

/// A physical control's complete identity, supplied by the actual client.
/// Window and hand are distinct from the coordinator's protocol window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetIdentity {
    pub window: String,
    pub hand: Vec<String>,
    pub control: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputTarget {
    pub identity: TargetIdentity,
    /// Actual target bounds and containing play surface in viewport CSS pixels.
    pub bounds: CanvasRect,
    pub viewport: CanvasRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressRole {
    Action,
    Declaration,
    Discard,
    Candidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// A dispatched physical input only permits waiting for the next target.
    Dispatch,
    ClientCommand,
    /// A real declaration echo opens one precisely bound continuation.
    DeclarationEcho,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PressIntent {
    pub role: PressRole,
    pub action: MjaiEvent,
    pub confirmation: Confirmation,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TurnPlan {
    pub presses: Vec<PressIntent>,
    pub dealt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnClockFacts {
    pub started_at: Instant,
    pub fixed_ms: u32,
    /// Unknown is kept unknown, never replaced with the initial bank cap.
    pub remaining_bank_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeclarationFacts {
    /// Lossless opaque projection of the actual protocol-window proof.
    pub window: String,
    pub hand: Vec<String>,
}

/// A lossless projection of the producer-owned proof, shared by adapters and
/// session orchestration. It creates no new window or generation.
pub fn window_identity(proof: crate::bot::control::WindowProof) -> String {
    serde_json::to_string(&proof).expect("window proof serialization is infallible")
}

pub struct SiteObservation {
    pub ready: bool,
    pub target_kind: TargetKind,
    pub target: Option<InputTarget>,
    pub layout: Option<PointerLayout>,
    pub clock: Option<TurnClockFacts>,
    pub continuation: Option<DeclarationFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Reveal,
    Commit,
}

/// Read-only normalization and exact physical target matching. Transaction
/// state, pacing, retries, and all mouse input belong to the common runtime.
#[async_trait::async_trait]
pub trait SiteAdapter: Send + Sync {
    fn plan(&self, context: &ActionContext<'_>) -> TurnPlan;
    async fn pointer_layout(
        &self,
        page: &Page,
        context: &ActionContext<'_>,
    ) -> anyhow::Result<Option<PointerLayout>>;
    async fn observe(
        &self,
        page: &Page,
        context: &ActionContext<'_>,
        intent: &PressIntent,
        point: Option<(f64, f64)>,
    ) -> anyhow::Result<SiteObservation>;
}

/// Everything the platform impl needs to translate one bot decision
/// into a concrete click sequence.
pub struct ActionContext<'a> {
    /// The bot's chosen action (from `BotResponseBus`).
    pub action: &'a MjaiEvent,
    /// Live game state from the riichi engine.
    pub snapshot: &'a GameStateSnapshot,
    /// Currently legal actions for `our_seat`, sourced from the riichi
    /// engine's `_get_legal_actions_internal`. The platform impl uses
    /// this to:
    /// - decide which action button (chi/pon/kan/...) is in which
    ///   on-screen position, by intersecting with the platform's
    ///   priority table;
    /// - enumerate chi/pon/kan candidate combinations when the bot's
    ///   action is ambiguous (multiple `consume_tiles`).
    pub legal_actions: &'a [Action],
    /// Bot's seat.
    pub our_seat: u8,
    /// The most recent tile any seat discarded — needed to disambiguate
    /// chi/pon target.
    pub last_kawa_tile: Option<&'a str>,
    /// The tile we drew this turn, if any. Used to detect tsumohai
    /// position when emitting `dahai`.
    pub last_self_tsumo: Option<&'a str>,
    /// True from the moment the server confirms our riichi until the
    /// kyoku ends. While set, dahai clicks are suppressed (Majsoul auto-
    /// discards in riichi mode).
    pub self_riichi_accepted: bool,
    /// 3 (sanma) or 4 (yonma).
    pub num_players: u8,
    /// Where the pointer is as the plan is made, in viewport CSS pixels,
    /// when that is known. A discard of a tile the pointer is already on
    /// presses that copy rather than its twin.
    pub pointer: Option<(f64, f64)>,
}
