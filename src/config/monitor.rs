use serde::{Deserialize, Serialize};

/// The application's window. It is always open — it is the application — so
/// what is left to choose is whether it stays above other windows, how
/// autoplay's hand shows — its pointer drawn over the game, resting between
/// turns where the next discard is likely to be, and what it presses between
/// decisions — and whether the game is offered to a streaming program.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    pub always_on_top: bool,
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
    /// Offer the game view — the game alone, without the dock or any pointer
    /// drawn over it — at a local address a streaming program can show. The
    /// shell serves it while this is on.
    pub stream_output: bool,
    /// The port that address is on, on 127.0.0.1, and the secret it carries so
    /// that nothing else on the machine can guess it. Both are the shell's
    /// choice, kept here so that the address stays the same from run to run.
    pub stream_port: u16,
    pub stream_token: String,
}
