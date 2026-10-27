//! Platform-agnostic autoplay surface.
//!
//! `PlatformAutoplay` is the only thing `AutoplayManager` knows about —
//! by holding `Arc<dyn PlatformAutoplay>` it can drop in a Tenhou impl
//! later (DOM-based clicks or direct WS inject) without touching the
//! manager's bus subscription / timing logic.

use crate::autoplay::context::CanvasRect;
use crate::autoplay::pointer::PointerLayout;
use crate::game_state::snapshot::GameStateSnapshot;
use crate::monitor::DecisionContext;
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
    pub context: DecisionContext,
    /// Lossless opaque projection of the actual protocol-window proof.
    pub window: String,
    pub action: MjaiEvent,
    pub hand: Vec<String>,
}

pub struct SiteObservation {
    pub ready: bool,
    pub target: Option<InputTarget>,
    pub layout: Option<PointerLayout>,
    pub clock: Option<TurnClockFacts>,
    pub continuation: Option<DeclarationFacts>,
}

/// Read-only normalization and exact physical target matching. Transaction
/// state, pacing, retries, and all mouse input belong to the common runtime.
#[async_trait::async_trait]
pub trait SiteAdapter: Send + Sync {
    fn plan(&self, context: &ActionContext<'_>) -> TurnPlan;
    async fn observe(
        &self,
        page: &Page,
        context: &ActionContext<'_>,
        intent: &PressIntent,
    ) -> anyhow::Result<SiteObservation>;
}

/// One step in the click sequence the manager will execute.
///
/// The 16:9-normalised coordinates match the convention used by the
/// original Akagi Python autoplay `LOCATION` table.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Click at a normalised 16:9 point on the game canvas.
    Click { x_norm: f64, y_norm: f64 },
    /// Pause for `duration_ms` before the next step. Used for the
    /// pre-click "thinking" delay and the inter-click gap inside one
    /// action.
    Sleep { duration_ms: u32 },
    /// Match a semantic descriptor to one actual Web3 native offer. This is
    /// never a synthesized socket frame or a menu-position fallback.
    ClientChoice {
        payload: serde_json::Value,
        label: String,
    },
    /// Block until the client is ready to take input, then continue.
    ///
    /// Frame arrival is not the start of the turn. Tenhou's server can send
    /// several seats' actions at once — against millisecond-fast opponents it
    /// routinely does — while the client spends seconds animating them, and
    /// only then draws the buttons and starts its clock. Everything timed
    /// from frame arrival is timed from the wrong instant.
    AwaitReady { timeout_ms: u32 },
    /// Discard the tile with this Tenhou index (`0..=135`) through the
    /// client's own handler.
    ///
    /// Not a position: the client's discard entry point is addressed by tile,
    /// so it resolves the rest itself — including where the tile currently
    /// sits, which is why calls moving the hand cannot affect this.
    Discard { tile_index: u32 },
    /// Transmit a pre-built wire frame to the game server through the MITM
    /// proxy's injection channel (Riichi City: no browser page exists to
    /// click, so the action goes out as the client's own protocol frame).
    /// See `bridge::riichi_city::build` and `autoplay::inject`.
    SendFrame(Vec<u8>),
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
    /// Where the pointer is as the plan is made, as a normalised 16:9 point,
    /// when that is known. A discard of a tile the pointer is already on
    /// presses that copy rather than its twin.
    pub pointer: Option<(f64, f64)>,
}

/// Output of `PlatformAutoplay::plan`: the click sequence to execute.
///
/// The riichi declaring discard is always resolved before the plan is
/// built — the bot fills `Reach.pai` (natively or via the manager's
/// autoplay reach follow-up, see `bot::manager` and #257) — so both
/// platforms declare and discard in a single plan; there is no
/// bus-injection follow-up path.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanResult {
    pub steps: Vec<Step>,
    /// The delay model asked for a look at the runner-up before this
    /// plan's press. Whether there is one to look at is the manager's to
    /// work out (see `autoplay::pointer`); the steps are the same either
    /// way.
    pub hesitate: bool,
    /// How long the pointer should rest on a tile it looks at, when the
    /// delay model said.
    pub look_ms: Option<u32>,
    /// The decision's window opened with a new hand being dealt (Majsoul's
    /// dealer opening): nothing in it is on screen before
    /// `humanize::DEAL_MS`, and the pointer's way to the press, looks and
    /// all, comes after (see `autoplay::pointer`).
    pub dealt: bool,
    /// The middles of the targets the presses aim at, in press order, once
    /// they have been scattered within them (`autoplay::pointer::aim`); empty
    /// before.
    pub aims: Vec<(f64, f64)>,
}

pub trait PlatformAutoplay: Send + Sync {
    /// Translate the bot's action into a click sequence + side-effect
    /// hints. Pure: must not perform IO. The manager handles the actual
    /// CDP dispatch and bus injection.
    fn plan(&self, ctx: &ActionContext) -> PlanResult;
}
