use serde::{Deserialize, Serialize};

/// The application's window. It is always open — it is the application — so
/// its options describe how autoplay's hand shows — its pointer drawn over
/// the game, resting between
/// turns where the next discard is likely to be, and what it presses between
/// decisions — and whether the game is offered to a streaming program.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    /// Draw autoplay's pointer over the game: the path it takes and where it presses.
    pub pointer_trail: bool,
    /// While the others play, rest autoplay's pointer on the tiles the bot
    /// expects to throw next, as a person's eye already would be (see
    /// `autoplay::pointer::lean`). Asks the bot between turns, so only a bot
    /// that answers with a lean gives the pointer anywhere to go.
    pub pointer_lean: bool,
    /// Press a recognized hand-result confirm after a reaction, then observe
    /// again before another press (see `autoplay::ui`).
    pub quick_confirm: bool,
    /// Some while after our riichi, or after our hand comes to be ready and
    /// to win on some wait, closed or open, switch the client's own auto-win
    /// on, below a mangan, and for a hand not in riichi not in a hand that may
    /// be the game's last; the client switches it off for every new hand (see
    /// `autoplay::toggles`, `autoplay::tenpai`).
    pub auto_win: bool,
    /// Play game after game: with autoplay on, press through the game's own
    /// screens after its end — the standings, the rank, the room's rewards —
    /// and "play again" (see `autoplay::ui`). The level of autoplay above
    /// playing a game; switched off during a game, that game is the last.
    pub auto_continue: bool,
}
