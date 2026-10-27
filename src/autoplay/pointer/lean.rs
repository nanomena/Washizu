//! Where the pointer rests while the others play: on the tiles the bot
//! expects its seat to throw next, where a person's eye already would be.
//!
//! With `monitor.pointer_lean` on, the bot manager asks the bot at every
//! other seat's discard too — a batch the seat has nothing to answer — and a
//! bot that plays its next turn out ahead answers `none` with `meta.lean`:
//!
//! ```json
//! {"lines": 18.5, "options": [{"discard": true, "tsumogiri": false, "pai": "6s", "share": 0.7},
//!                            {"discard": true, "tsumogiri": true, "share": 0.2}]}
//! ```
//!
//! its likeliest next answers with their shares, and how many lines of play
//! they rest on. The independent forecast worker binds the result to its
//! owner and real context before handing it here. It is never a decision.
//!
//! When the pointer moves is this module's to say, not the table's. After the
//! seat's discard it rests a while; then, every so often, it looks at one of
//! the likely tiles — the likelier, the more often — or stays where it is,
//! as a hand on a mouse mostly does. A changed mind is acted on after a
//! reaction time. Nothing moves the moment an opponent discards.

use crate::autoplay::humanize;
use rand::Rng;
use serde::Deserialize;
use std::sync::OnceLock;
use tokio::sync::broadcast;

/// What a bot expects of its seat's next turn.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Lean {
    /// How many lines of play the options rest on.
    pub lines: f64,
    /// The likeliest answers, likeliest first.
    pub options: Vec<LeanOption>,
}

/// One answer the seat's next turn may give.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LeanOption {
    /// A discard, rather than a riichi, a win or a kan.
    pub discard: bool,
    /// The tile the seat will draw, thrown as it comes.
    #[serde(default)]
    pub tsumogiri: bool,
    /// The tile thrown from the hand.
    #[serde(default)]
    pub pai: Option<String>,
    pub share: f64,
}

/// Where an option puts the pointer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Spot {
    /// On a tile of the hand.
    Hand(String),
    /// Where the next draw will land.
    Draw,
}

impl LeanOption {
    pub fn spot(&self) -> Option<Spot> {
        match (self.discard, self.tsumogiri, &self.pai) {
            (false, _, _) => None,
            (true, true, _) => Some(Spot::Draw),
            (true, false, Some(pai)) => Some(Spot::Hand(pai.clone())),
            (true, false, None) => None,
        }
    }
}

impl Lean {
    /// The spot the lean favours, once it rests on enough lines to say.
    pub fn first(&self) -> Option<Spot> {
        if self.lines < MIN_LINES {
            return None;
        }
        self.options.first()?.spot()
    }
}

/// How many lines a lean must rest on before the pointer follows it.
pub const MIN_LINES: f64 = 4.0;

static LEANS: OnceLock<broadcast::Sender<crate::bot::forecast::ForecastLean>> = OnceLock::new();

fn leans() -> &'static broadcast::Sender<crate::bot::forecast::ForecastLean> {
    LEANS.get_or_init(|| broadcast::channel(16).0)
}

/// Pass on what a bot said of its next turn.
pub fn publish(lean: crate::bot::forecast::ForecastLean) {
    let _ = leans().send(lean);
}

/// Every lean from now on.
pub fn subscribe() -> broadcast::Receiver<crate::bot::forecast::ForecastLean> {
    leans().subscribe()
}

/// What the pointer does at one glance: look at a spot, or (`None`) stay
/// where it is. A hand mostly stays — the more so once the mind is made up,
/// since the pointer is then already where it means to be — and looks at
/// the likelier tiles more often than their shares alone would say.
pub fn glance<R: Rng + ?Sized>(lean: &Lean, rng: &mut R) -> Option<Spot> {
    if lean.lines < MIN_LINES {
        return None;
    }
    let spots: Vec<(Spot, f64)> = lean
        .options
        .iter()
        .filter_map(|option| option.spot().map(|spot| (spot, option.share.max(0.0))))
        .collect();
    let top = spots.first()?.1;
    if humanize::chance((0.25 + 0.35 * top).clamp(0.0, 0.8), rng) {
        return None;
    }
    let weights: Vec<f64> = spots.iter().map(|(_, share)| share.powf(1.5)).collect();
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return Some(spots[0].0.clone());
    }
    let mut at = humanize::fraction(rng) * total;
    for ((spot, _), weight) in spots.iter().zip(&weights) {
        if at < *weight {
            return Some(spot.clone());
        }
        at -= weight;
    }
    Some(spots[spots.len() - 1].0.clone())
}

/// How long until the next glance: mostly a second or two, now and then a
/// good deal longer, and longer once the mind is made up.
pub fn wait_ms<R: Rng + ?Sized>(lean: Option<&Lean>, rng: &mut R) -> u32 {
    let top = lean
        .and_then(|lean| lean.options.first())
        .map_or(0.5, |option| option.share.clamp(0.0, 1.0));
    let median: f64 = 1200.0 + 1000.0 * top;
    humanize::Tail {
        median,
        ..humanize::GLANCE
    }
    .draw_ms(rng)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn lean(json: &str) -> Lean {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_lean_is_read_as_the_bot_writes_it() {
        let read = lean(
            r#"{"lines": 18.5, "options": [{"discard": true, "tsumogiri": false, "pai": "6s", "share": 0.7},
                {"discard": true, "tsumogiri": true, "share": 0.2}, {"discard": false, "tsumogiri": false, "share": 0.1}]}"#,
        );
        let spots: Vec<_> = read.options.iter().map(LeanOption::spot).collect();
        assert_eq!(
            spots,
            [Some(Spot::Hand("6s".into())), Some(Spot::Draw), None]
        );
        assert_eq!(read.first(), Some(Spot::Hand("6s".into())));
    }

    #[test]
    fn a_lean_on_too_few_lines_moves_nothing() {
        let thin =
            lean(r#"{"lines": 2, "options": [{"discard": true, "pai": "6s", "share": 1.0}]}"#);
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(thin.first(), None);
        assert!((0..50).all(|_| glance(&thin, &mut rng).is_none()));
    }

    #[test]
    fn glances_go_to_the_likely_tiles_the_likelier_more_often_and_sometimes_nowhere() {
        let split = lean(
            r#"{"lines": 20, "options": [{"discard": true, "pai": "6s", "share": 0.6},
                {"discard": true, "pai": "7s", "share": 0.3}, {"discard": false, "share": 0.1}]}"#,
        );
        let mut rng = StdRng::seed_from_u64(7);
        let mut seen = std::collections::HashMap::new();
        for _ in 0..2000 {
            *seen.entry(glance(&split, &mut rng)).or_insert(0u32) += 1;
        }
        let first = seen[&Some(Spot::Hand("6s".into()))];
        let second = seen[&Some(Spot::Hand("7s".into()))];
        let stays = seen[&None];
        // 0.6^1.5 : 0.3^1.5 is near 2.8 : 1, of the glances that move.
        assert!(first > 2 * second && second > 100, "{seen:?}");
        // Staying is about 0.46 of the glances here.
        assert!((700..1150).contains(&stays), "{seen:?}");
        assert_eq!(seen.len(), 3, "a riichi or a win is no spot: {seen:?}");
    }

    #[test]
    fn the_wait_between_glances_is_human_and_longer_when_settled() {
        let mut rng = StdRng::seed_from_u64(3);
        let settled =
            lean(r#"{"lines": 20, "options": [{"discard": true, "pai": "6s", "share": 0.95}]}"#);
        let torn =
            lean(r#"{"lines": 20, "options": [{"discard": true, "pai": "6s", "share": 0.4}]}"#);
        let mean = |lean: &Lean, rng: &mut StdRng| {
            (0..400)
                .map(|_| wait_ms(Some(lean), rng) as f64)
                .sum::<f64>()
                / 400.0
        };
        let (calm, busy) = (mean(&settled, &mut rng), mean(&torn, &mut rng));
        assert!(calm > busy + 300.0, "{calm} vs {busy}");
        assert!((0..400).all(|_| (600..=6000).contains(&wait_ms(None, &mut rng))));
    }
}
