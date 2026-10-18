//! What the hand does at the table between decisions, as a person's does.
//!
//! - **A hand's result.** A result is two screens, the hand with its yaku
//!   and then the scores moving, and each shows a blue confirm button bottom
//!   right once it is drawn, counting down from three. A person presses a
//!   button once they can see it, not while the screen is still being drawn.
//!   So from the moment a hand's result arrives the hand looks for the button
//!   (`autoplay::pixels`) a few times a second; each time one shows, it keeps
//!   an eye on it for a reaction (`humanize::CONFIRM_SEEN`, longer after a
//!   win of our own) and, the button still there, presses it, in bursts of
//!   two to seven quick presses, a pause (`humanize::RESULT_PAUSE`) and a look
//!   before the next burst. No burst starts once the client confirms the
//!   result; one already begun is pressed to its end, its last presses on the
//!   next screen or the empty corner of the next hand's table. A screen that
//!   cannot be read falls back to pressing on a clock (`humanize::RESULT_START`
//!   from the result's arrival).
//! - **The game's last result.** The game ends as its last result shows, and
//!   the client confirms nothing more, so the hand cannot count it gone. Its
//!   two screens are pressed through all the same, a press at a time. The
//!   game's own screens that follow, the final standings and then the rank,
//!   are not the result's: they have a yellow confirm in the same place, and
//!   the first sight of it ends the presses. Nothing is pressed on a clock at
//!   the end.
//! - **The game's own screens, to play again.** After the game's end the
//!   hand looks for them: the standings, the rank, then the room's rewards
//!   with "play again" beside the yellow confirm. Each is read for a while
//!   (`humanize::END_SEEN`) and pressed once, its yellow confirm, until
//!   "play again" shows; that is pressed instead. A room's chest whose bar
//!   fills gives its rewards on a band over the last screen first; it is
//!   read the same and pressed away. "Play again" asks whether
//!   to match another game, a moment after; that question is answered
//!   (`humanize::ASK_SEEN`), its confirm, and the room matches the next game.
//!   After "play again" only it and its question are pressed. A screen it
//!   does not know is never pressed, nor one that cannot be read. The manager makes these only at the level of autoplay
//!   that plays game after game; below it the player's own.
//! - **Auto-win after a riichi.** Once our riichi is accepted, the hand may
//!   switch the client's own auto-win on, at any moment as likely as the next
//!   (`humanize::AUTO_WIN_ON`): an early riichi, with longer to go, is the
//!   likelier to have it on. The client switches it off again by itself for
//!   every new hand.
//!
//! This is the plan alone; the manager makes the looks and the presses, and
//! reads the toggle's colour to know where it stands (`autoplay::pixels`).

use crate::autoplay::humanize::{
    self, Hazard, ASK_SEEN, AUTO_WIN_ON, CONFIRM_SEEN, CONFIRM_SEEN_MOST_MS, END_LOOK_EVERY_MS,
    END_LOOK_FIRST_MS, END_PAUSE, END_SEEN, OUR_BUTTON_LINGER, OUR_WIN_LINGER,
    RESULT_LOOK_EVERY_MS, RESULT_LOOK_FIRST_MS, RESULT_PAUSE, RESULT_START,
};
use crate::autoplay::pixels::Sight;
use rand::Rng;
use std::time::{Duration, Instant};

/// The longest a result is pressed: past it the client has long confirmed
/// by its own clock, and a confirm missed by the count must not leave the
/// hand pressing whatever came next.
const RESULT_MOST: Duration = Duration::from_secs(30);
/// The longest the game's last result is pressed after the game's end: its
/// two screens are past by then.
const LAST_MOST: Duration = Duration::from_secs(45);
/// The longest the game's own screens are looked for after its end: the
/// next game is matched well within it.
const END_MOST: Duration = Duration::from_secs(180);
/// The most presses on the game's own screens: four or five go by in a
/// game's end, and one that does not go is left after this.
const END_PRESSES: u32 = 8;
/// How long after "play again" its question is waited for: it comes with a
/// little animation, and a look may fall before it.
const ASK_WAIT: Duration = Duration::from_secs(5);

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Look whether a confirm button shows.
    Look,
    /// Press the confirm button this many times.
    Burst(u32),
    /// Switch the client's auto-win on.
    AutoWinOn,
    /// Look at the game's own screens after its end.
    EndLook,
    /// Press the button of the game's own screen in sight, once: its yellow
    /// confirm ([`Sight::Standings`]), "play again" ([`Sight::Rematch`]), or
    /// the confirm of the question that asks ([`Sight::RematchAsk`]).
    EndPress(Sight),
}

/// What comes next for a result on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Look,
    Burst,
}

/// A result on screen.
#[derive(Debug, Clone, Copy)]
struct Shown {
    /// How many results the client had confirmed when it showed: one more,
    /// and it is gone.
    seen: u64,
    /// Until when it is pressed at most.
    until: Instant,
    /// When the next step goes, and which; `None` while one is under way.
    next: Option<(Instant, Step)>,
    /// The seat's own win: the result is let show for longer.
    ours: bool,
    /// A confirm button of it has shown.
    met: bool,
    /// The button the last look saw, if it saw one.
    in_sight: Option<Sight>,
    /// When the button in sight is pressed, if it is still there; `None`
    /// once the reaction to it is over.
    press_at: Option<Instant>,
    /// When it is pressed if the screen cannot be read.
    blind_at: Instant,
    /// The game's last result: the client confirms none, and the game's own
    /// screens follow it.
    last: bool,
}

/// What comes next at the game's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndStep {
    Look,
    Press(Sight),
}

/// The game's own screens after its end.
#[derive(Debug, Clone, Copy)]
struct Ending {
    /// Until when they are looked for at most.
    until: Instant,
    /// When the next step goes, and which; `None` while one is under way.
    next: Option<(Instant, EndStep)>,
    /// The button the last look saw, if it saw one of theirs.
    in_sight: Option<Sight>,
    /// When the button in sight is pressed, if the look then still sees it.
    press_at: Option<Instant>,
    /// Presses made on them so far.
    presses: u32,
    /// When "play again" was pressed: after it, only it and its question
    /// are, and once neither shows the game's end is over.
    again: Option<Instant>,
    /// The question has been answered.
    asked: bool,
}

/// The acts in hand.
#[derive(Debug, Default)]
pub struct Acts {
    result: Option<Shown>,
    win_on_at: Option<Instant>,
    end: Option<Ending>,
}

fn after<R: Rng + ?Sized>(now: Instant, wait: Hazard, rng: &mut R) -> Instant {
    now + Duration::from_millis(u64::from(wait.draw_ms(rng)))
}

impl Shown {
    /// From a button showing to pressing it.
    fn reaction<R: Rng + ?Sized>(&self, rng: &mut R) -> Duration {
        let wait = f64::from(CONFIRM_SEEN.draw_ms(rng));
        let wait = if self.ours {
            wait * OUR_BUTTON_LINGER
        } else {
            wait
        };
        Duration::from_millis(wait.min(CONFIRM_SEEN_MOST_MS) as u64)
    }

    /// Still on screen, the client having confirmed `confirmed` results.
    fn up(&self, confirmed: u64) -> bool {
        self.last || self.seen == confirmed
    }
}

impl Acts {
    /// A hand's result has arrived, the seat's own win when `ours`; the client
    /// had confirmed `confirmed` so far.
    pub fn result_shown<R: Rng + ?Sized>(
        &mut self,
        confirmed: u64,
        ours: bool,
        now: Instant,
        rng: &mut R,
    ) {
        let blind = |ours: bool, rng: &mut R| {
            let wait = f64::from(RESULT_START.draw_ms(rng));
            let wait = if ours { wait * OUR_WIN_LINGER } else { wait };
            now + Duration::from_millis(wait as u64)
        };
        match &mut self.result {
            None => {
                self.result = Some(Shown {
                    seen: confirmed,
                    until: now + RESULT_MOST,
                    next: Some((
                        now + Duration::from_millis(RESULT_LOOK_FIRST_MS),
                        Step::Look,
                    )),
                    ours,
                    met: false,
                    in_sight: None,
                    press_at: None,
                    blind_at: blind(ours, rng),
                    last: false,
                });
            }
            // Two wins on one discard are one result; one of them ours makes
            // it ours, while its button has not shown.
            Some(shown) if ours && !shown.ours && !shown.met => {
                shown.ours = true;
                shown.blind_at = shown.blind_at.max(blind(true, rng));
            }
            Some(_) => {}
        }
        // A riichi that ended before its auto-win was switched on stays without it.
        self.win_on_at = None;
    }

    /// Our riichi was accepted.
    pub fn riichi_accepted<R: Rng + ?Sized>(&mut self, now: Instant, rng: &mut R) {
        self.win_on_at = Some(after(now, AUTO_WIN_ON, rng));
    }

    /// A new hand is dealt, or a new game begins: nothing that was due is.
    pub fn hand_over(&mut self) {
        *self = Self::default();
    }

    /// The game is over at `now`. Its last result, if one is on screen, is
    /// still pressed through, up to the game's own screens; those are looked
    /// for after it.
    pub fn game_over(&mut self, now: Instant) {
        self.win_on_at = None;
        if let Some(shown) = &mut self.result {
            shown.last = true;
            shown.until = now + LAST_MOST;
        }
        self.end = Some(Ending {
            until: now + END_MOST,
            next: Some((
                now + Duration::from_millis(END_LOOK_FIRST_MS),
                EndStep::Look,
            )),
            in_sight: None,
            press_at: None,
            presses: 0,
            again: None,
            asked: false,
        });
    }

    /// Whether the game's own screens are looked at now: not while the last
    /// result is (its looks see the first of them, and end it).
    fn end_turn(&self) -> bool {
        self.result.is_none_or(|r| r.next.is_none())
    }

    /// When [`Self::next`] next has something to do.
    pub fn due(&self) -> Option<Instant> {
        let end = self
            .end
            .filter(|_| self.end_turn())
            .and_then(|e| e.next.map(|n| n.0));
        [
            self.result.and_then(|r| r.next.map(|n| n.0)),
            self.win_on_at,
            end,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// What to do at `now`, the client having confirmed `confirmed` results.
    pub fn next<R: Rng + ?Sized>(
        &mut self,
        now: Instant,
        confirmed: u64,
        rng: &mut R,
    ) -> Option<Act> {
        if let Some(result) = &mut self.result {
            if !result.up(confirmed) || now >= result.until {
                self.result = None;
            } else if let Some((_, step)) = result.next.filter(|(at, _)| now >= *at) {
                result.next = None;
                return Some(match step {
                    Step::Look => Act::Look,
                    // At the end a press at a time, each after a look.
                    Step::Burst if result.last => Act::Burst(1),
                    Step::Burst => Act::Burst(humanize::burst_len(rng)),
                });
            }
        }
        if self.win_on_at.is_some_and(|at| now >= at) {
            self.win_on_at = None;
            return Some(Act::AutoWinOn);
        }
        if !self.end_turn() {
            return None;
        }
        if let Some(end) = &mut self.end {
            if now >= end.until {
                self.end = None;
            } else if let Some((_, step)) = end.next.filter(|(at, _)| now >= *at) {
                end.next = None;
                return Some(match step {
                    EndStep::Look => Act::EndLook,
                    EndStep::Press(button) => Act::EndPress(button),
                });
            }
        }
        None
    }

    /// A look made at `now` found `sight`.
    pub fn looked<R: Rng + ?Sized>(
        &mut self,
        sight: Sight,
        now: Instant,
        confirmed: u64,
        rng: &mut R,
    ) {
        let Some(mut shown) = self.result.filter(|r| r.up(confirmed)) else {
            self.result = None;
            return;
        };
        // The yellow button is the game's own, after its last result: it
        // ends the result's presses, and the game's own screens are next.
        let sight = match sight {
            Sight::Standings if !shown.last => Sight::Nothing,
            Sight::Rematch | Sight::RematchAsk | Sight::Reward => Sight::Nothing,
            sight => sight,
        };
        let button = (sight == Sight::Button).then_some(sight);
        let fresh = button.is_some() && shown.in_sight != button;
        shown.in_sight = button;
        shown.met |= button.is_some();
        if fresh {
            shown.press_at = Some(now + shown.reaction(rng));
        }
        if button.is_none() {
            shown.press_at = None;
        }
        let every = now + Duration::from_millis(RESULT_LOOK_EVERY_MS);
        shown.next = match sight {
            Sight::Standings => None,
            Sight::Nothing | Sight::Rematch | Sight::RematchAsk | Sight::Reward => {
                Some((every, Step::Look))
            }
            // A button in sight: watched while the reaction lasts, then
            // pressed; one still there after a burst is pressed now.
            Sight::Button => match shown.press_at {
                Some(at) if now < at => Some((every.min(at), Step::Look)),
                _ => {
                    shown.press_at = None;
                    Some((now, Step::Burst))
                }
            },
            // Not to be seen: pressed on the clock, but never at the end.
            Sight::Unread if shown.last => None,
            Sight::Unread => Some((shown.blind_at.max(now), Step::Burst)),
        };
        self.result = shown.next.is_some().then_some(shown);
    }

    /// A look at the game's own screens made at `now` found `sight`. One of
    /// them in sight is read for a while, then looked at once more, and
    /// pressed if it is still there; anything else is looked at again in a
    /// while, and pressed never. After "play again" only it and its
    /// question are, the question only then.
    pub fn end_looked<R: Rng + ?Sized>(&mut self, sight: Sight, now: Instant, rng: &mut R) {
        let Some(end) = &mut self.end else {
            return;
        };
        let button = match sight {
            Sight::Rematch if !end.asked => Some(sight),
            Sight::Standings | Sight::Reward if end.again.is_none() => Some(sight),
            Sight::RematchAsk if end.again.is_some() => Some(sight),
            _ => None,
        };
        if let (Some(again), None) = (end.again, button) {
            // Answered, or no question came: the room is matching the next game.
            if end.asked || now >= again + ASK_WAIT {
                self.end = None;
                return;
            }
        }
        if button != end.in_sight {
            end.in_sight = button;
            let read = if button == Some(Sight::RematchAsk) {
                ASK_SEEN
            } else {
                END_SEEN
            };
            end.press_at = button.map(|_| after(now, read, rng));
        }
        let every = now + Duration::from_millis(END_LOOK_EVERY_MS);
        end.next = Some(match (button, end.press_at) {
            (Some(button), Some(at)) if now >= at => (now, EndStep::Press(button)),
            (Some(_), Some(at)) => (at, EndStep::Look),
            _ => (every, EndStep::Look),
        });
    }

    /// A press on the game's own screen showing `sight` is over at `now`:
    /// the next screen, or the same one if the press did not take, is looked
    /// for after a pause, and read afresh.
    pub fn end_pressed<R: Rng + ?Sized>(&mut self, sight: Sight, now: Instant, rng: &mut R) {
        let Some(end) = &mut self.end else {
            return;
        };
        end.presses += 1;
        if end.presses >= END_PRESSES {
            self.end = None;
            return;
        }
        if sight == Sight::Rematch {
            end.again = Some(now);
        }
        end.asked |= sight == Sight::RematchAsk;
        end.in_sight = None;
        end.press_at = None;
        end.next = Some((after(now, END_PAUSE, rng), EndStep::Look));
    }

    /// Whether the result a burst is pressing is still up.
    pub fn result_up(&self, confirmed: u64) -> bool {
        self.result.is_some_and(|r| r.up(confirmed))
    }

    /// A burst is over at `now`: if the result is still up, a look after a
    /// pause says whether another follows.
    pub fn burst_done<R: Rng + ?Sized>(&mut self, now: Instant, confirmed: u64, rng: &mut R) {
        self.result = self.result.filter(|r| r.up(confirmed)).map(|r| Shown {
            next: Some((after(now, RESULT_PAUSE, rng), Step::Look)),
            ..r
        });
    }

    /// Not now: the act comes again a while after `now` — a riichi's auto-win
    /// while our own decision is open, say. A burst put off is looked for
    /// again first, so a button gone meanwhile is not pressed.
    pub fn later(&mut self, act: Act, now: Instant, wait: Duration) {
        let at = now + wait;
        match act {
            Act::Look | Act::Burst(_) => {
                if let Some(result) = &mut self.result {
                    result.next = Some((at, Step::Look));
                }
            }
            Act::AutoWinOn => self.win_on_at = Some(at),
            Act::EndLook | Act::EndPress(_) => {
                if let Some(end) = &mut self.end {
                    end.next = Some((at, EndStep::Look));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    /// Steps the plan to its next act, as the manager's loop does.
    fn step(acts: &mut Acts, confirmed: u64, rng: &mut StdRng) -> Option<(Instant, Act)> {
        let at = acts.due()?;
        acts.next(at, confirmed, rng).map(|act| (at, act))
    }

    /// The next look sees `sight`, and so does every look while the reaction
    /// lasts; returns when the press is due, which it is at the last look.
    fn see(acts: &mut Acts, sight: Sight, confirmed: u64, rng: &mut StdRng) -> Instant {
        let (first, act) = step(acts, confirmed, rng).unwrap();
        assert_eq!(act, Act::Look);
        watch(acts, sight, first, confirmed, rng)
    }

    /// As [`see`], from a look under way at `first`.
    fn watch(
        acts: &mut Acts,
        sight: Sight,
        first: Instant,
        confirmed: u64,
        rng: &mut StdRng,
    ) -> Instant {
        acts.looked(sight, first, confirmed, rng);
        loop {
            let at = acts.due().unwrap();
            match acts.next(at, confirmed, rng).unwrap() {
                Act::Look => {
                    assert!(at <= first + Duration::from_millis(CONFIRM_SEEN_MOST_MS as u64));
                    acts.looked(sight, at, confirmed, rng);
                }
                act => {
                    assert!(matches!(act, Act::Burst(_)));
                    assert!(at >= first + Duration::from_millis(CONFIRM_SEEN.floor as u64));
                    // Put back as it was: the caller takes the burst.
                    acts.result.as_mut().unwrap().next = Some((at, Step::Burst));
                    return at;
                }
            }
        }
    }

    /// Nothing is pressed while the result is being drawn: the hand looks for
    /// the button a few times a second, and presses only after it has shown,
    /// a person has seen it, and it is still there.
    #[test]
    fn a_result_is_pressed_only_once_its_button_shows() {
        let mut rng = StdRng::seed_from_u64(1);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(4, false, t0, &mut rng);
        let (first, act) = step(&mut acts, 4, &mut rng).unwrap();
        assert_eq!(act, Act::Look);
        assert_eq!(first, ms(t0, RESULT_LOOK_FIRST_MS), "after the cut-in");
        assert_eq!(acts.due(), None, "nothing while a look is under way");
        // Eight seconds of the result being drawn: looks only.
        let mut now = first;
        while now < ms(t0, 8000) {
            acts.looked(Sight::Nothing, now, 4, &mut rng);
            let (at, act) = step(&mut acts, 4, &mut rng).unwrap();
            assert_eq!(act, Act::Look);
            assert_eq!(at, now + Duration::from_millis(RESULT_LOOK_EVERY_MS));
            now = at;
        }
        // The button shows: watched for a reaction, then a burst.
        let again = watch(&mut acts, Sight::Button, now, 4, &mut rng);
        assert!(again >= now + Duration::from_millis(CONFIRM_SEEN.floor as u64));
        let (at, act) = step(&mut acts, 4, &mut rng).unwrap();
        assert_eq!(at, again);
        let Act::Burst(n) = act else {
            panic!("a burst")
        };
        assert!((humanize::BURST_LEAST..=humanize::BURST_MOST).contains(&n));
        assert!(acts.result_up(4));
        // After it, a pause and a look; still up, at once another burst.
        acts.burst_done(at, 4, &mut rng);
        let (look, act) = step(&mut acts, 4, &mut rng).unwrap();
        assert_eq!(act, Act::Look);
        assert!(look >= at + Duration::from_millis(RESULT_PAUSE.floor as u64));
        acts.looked(Sight::Button, look, 4, &mut rng);
        let (at, act) = step(&mut acts, 4, &mut rng).unwrap();
        assert!(matches!(act, Act::Burst(_)));
        assert_eq!(at, look, "no second reaction to the same button");
        // The scores' screen: drawn first, then its own button, its own reaction.
        acts.burst_done(at, 4, &mut rng);
        let (look, _) = step(&mut acts, 4, &mut rng).unwrap();
        acts.looked(Sight::Nothing, look, 4, &mut rng);
        let pressed = see(&mut acts, Sight::Button, 4, &mut rng);
        assert!(matches!(step(&mut acts, 4, &mut rng), Some((t, Act::Burst(_))) if t == pressed));
        // The client confirms: the result is gone, and nothing follows.
        acts.burst_done(pressed, 5, &mut rng);
        assert_eq!(acts.due(), None);
        assert!(!acts.result_up(5));
    }

    /// A button that has gone by the look after the reaction (the client's
    /// count ran out) is not pressed; the hand looks on.
    #[test]
    fn a_button_gone_by_the_second_look_is_not_pressed() {
        let mut rng = StdRng::seed_from_u64(11);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(0, false, t0, &mut rng);
        let (at, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.looked(Sight::Button, at, 0, &mut rng);
        let (again, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.looked(Sight::Nothing, again, 0, &mut rng);
        assert_eq!(step(&mut acts, 0, &mut rng).map(|s| s.1), Some(Act::Look));
    }

    /// After a win of our own the button is looked at about half as long again
    /// before it is pressed, and never so long that its count runs out; a
    /// second win on the same discard that is ours makes the wait ours too.
    #[test]
    fn our_own_win_is_looked_at_longer() {
        let mut rng = StdRng::seed_from_u64(31);
        let t0 = Instant::now();
        let wait = |ours: bool, rng: &mut StdRng| {
            let mut acts = Acts::default();
            acts.result_shown(0, false, t0, rng);
            if ours {
                acts.result_shown(0, true, t0, rng);
            }
            let pressed = see(&mut acts, Sight::Button, 0, rng);
            pressed
                .duration_since(ms(t0, RESULT_LOOK_FIRST_MS))
                .as_millis() as f64
        };
        let theirs: Vec<f64> = (0..3000).map(|_| wait(false, &mut rng)).collect();
        let ours: Vec<f64> = (0..3000).map(|_| wait(true, &mut rng)).collect();
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let ratio = mean(&ours) / mean(&theirs);
        assert!((1.3..1.6).contains(&ratio), "{ratio}");
        assert!(ours
            .iter()
            .chain(&theirs)
            .all(|&w| w <= CONFIRM_SEEN_MOST_MS));
        assert!(theirs.iter().all(|&w| w >= CONFIRM_SEEN.floor));
    }

    /// A screen that cannot be read is pressed on the clock, from the result's
    /// arrival, as before the hand could look.
    #[test]
    fn an_unread_screen_is_pressed_on_the_clock() {
        let mut rng = StdRng::seed_from_u64(2);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(0, false, t0, &mut rng);
        let (at, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.looked(Sight::Unread, at, 0, &mut rng);
        let (burst, act) = step(&mut acts, 0, &mut rng).unwrap();
        assert!(matches!(act, Act::Burst(_)));
        assert!(burst >= ms(t0, RESULT_START.floor as u64));
    }

    /// A result whose confirm was never counted is not pressed for ever.
    #[test]
    fn a_result_is_pressed_for_half_a_minute_at_most() {
        let mut rng = StdRng::seed_from_u64(5);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(0, false, t0, &mut rng);
        let (mut bursts, mut last) = (0, t0);
        while let Some((at, act)) = step(&mut acts, 0, &mut rng) {
            match act {
                Act::Look => acts.looked(Sight::Button, at, 0, &mut rng),
                Act::Burst(_) => {
                    (bursts, last) = (bursts + 1, at);
                    acts.burst_done(at, 0, &mut rng);
                }
                Act::AutoWinOn | Act::EndLook | Act::EndPress(_) => unreachable!(),
            }
        }
        assert!(bursts >= 1);
        assert!(
            last < t0 + RESULT_MOST,
            "the last burst started within the half minute"
        );
    }

    /// The game's last result is pressed through all the same, a press at a
    /// time on each of its two screens; the game's final standings that come
    /// next, with their yellow button, end the presses unpressed.
    #[test]
    fn the_last_result_is_pressed_up_to_the_game_s_own_screens() {
        let mut rng = StdRng::seed_from_u64(6);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        // The game ends two milliseconds after its last result arrives.
        acts.result_shown(7, false, t0, &mut rng);
        acts.game_over(ms(t0, 2));
        for _screen in 0..2 {
            let at = see(&mut acts, Sight::Button, 7, &mut rng);
            assert_eq!(
                step(&mut acts, 7, &mut rng),
                Some((at, Act::Burst(1))),
                "a single press"
            );
            assert!(acts.result_up(7), "no confirm is counted at the end");
            acts.burst_done(at, 7, &mut rng);
            let (look, _) = step(&mut acts, 7, &mut rng).unwrap();
            acts.looked(Sight::Nothing, look, 7, &mut rng);
        }
        // The standings: not the result's to press; the game's own screens are next.
        let (look, act) = step(&mut acts, 7, &mut rng).unwrap();
        assert_eq!(act, Act::Look);
        acts.looked(Sight::Standings, look, 7, &mut rng);
        assert!(!acts.result_up(7));
        assert_eq!(
            step(&mut acts, 7, &mut rng).map(|s| s.1),
            Some(Act::EndLook)
        );
    }

    /// The yellow button belongs to the game's end: in a hand's result it is
    /// not pressed, and the hand looks on for the hand's own button.
    #[test]
    fn a_yellow_button_in_a_hand_s_result_is_not_pressed() {
        let mut rng = StdRng::seed_from_u64(13);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(0, false, t0, &mut rng);
        let (at, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.looked(Sight::Standings, at, 0, &mut rng);
        let (next, act) = step(&mut acts, 0, &mut rng).unwrap();
        assert_eq!(act, Act::Look);
        assert_eq!(next, at + Duration::from_millis(RESULT_LOOK_EVERY_MS));
    }

    /// At the end nothing is pressed blind, and nothing past its time.
    #[test]
    fn the_last_result_is_never_pressed_blind_or_late() {
        let mut rng = StdRng::seed_from_u64(7);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(0, false, t0, &mut rng);
        acts.game_over(t0);
        let (at, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.looked(Sight::Unread, at, 0, &mut rng);
        assert!(acts.result.is_none(), "not pressed blind");

        let mut acts = Acts::default();
        acts.result_shown(0, false, t0, &mut rng);
        acts.game_over(t0);
        let (at, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.looked(Sight::Nothing, at, 0, &mut rng);
        let late = acts.next(t0 + LAST_MOST, 0, &mut rng);
        assert!(!matches!(late, Some(Act::Look | Act::Burst(_))), "{late:?}");
        assert!(acts.result.is_none());
    }

    /// A game that ends with no result on screen leaves nothing of its
    /// hands to press, only its own screens to look for; a new hand or game
    /// clears what was due.
    #[test]
    fn nothing_but_the_end_s_screens_after_the_end_without_a_result() {
        let mut rng = StdRng::seed_from_u64(8);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.riichi_accepted(t0, &mut rng);
        acts.game_over(t0);
        assert_eq!(acts.due(), Some(ms(t0, END_LOOK_FIRST_MS)));
        assert_eq!(acts.next(ms(t0, 60_000), 0, &mut rng), Some(Act::EndLook));
        acts.result_shown(0, false, t0, &mut rng);
        acts.game_over(t0);
        acts.hand_over();
        assert_eq!(acts.due(), None);
        assert_eq!(acts.next(ms(t0, 5000), 0, &mut rng), None);
    }

    /// Steps the game's end to its next press, every look seeing `sight`;
    /// returns when it is pressed.
    fn end_until_press(acts: &mut Acts, sight: Sight, rng: &mut StdRng) -> Instant {
        loop {
            let (at, act) = step(acts, 0, rng).expect("something due");
            match act {
                Act::EndLook => acts.end_looked(sight, at, rng),
                Act::EndPress(button) => {
                    assert_eq!(button, sight);
                    return at;
                }
                act => panic!("not an act of the end: {act:?}"),
            }
        }
    }

    /// The game's own screens are pressed through, each once after it has
    /// been read, the yellow confirm until "play again" shows; "play again"
    /// is pressed, and once it has gone the end is over.
    #[test]
    fn the_game_s_screens_are_pressed_through_to_play_again() {
        let mut rng = StdRng::seed_from_u64(21);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.game_over(t0);
        let mut since = t0;
        for sight in [
            Sight::Standings,
            Sight::Standings,
            Sight::Reward,
            Sight::Rematch,
        ] {
            // The screen is drawn first.
            let (look, act) = step(&mut acts, 0, &mut rng).unwrap();
            assert_eq!(act, Act::EndLook);
            acts.end_looked(Sight::Nothing, look, &mut rng);
            let (seen, _) = step(&mut acts, 0, &mut rng).unwrap();
            assert_eq!(seen, look + Duration::from_millis(END_LOOK_EVERY_MS));
            acts.end_looked(sight, seen, &mut rng);
            let pressed = end_until_press(&mut acts, sight, &mut rng);
            assert!(
                pressed >= seen + Duration::from_millis(END_SEEN.floor as u64),
                "read first"
            );
            assert!(pressed > since);
            since = pressed;
            acts.end_pressed(sight, pressed, &mut rng);
        }
        // "Play again" asks, a moment later: answered after a shorter read.
        let (look, act) = step(&mut acts, 0, &mut rng).unwrap();
        assert_eq!(act, Act::EndLook);
        assert!(look >= since + Duration::from_millis(END_PAUSE.floor as u64));
        acts.end_looked(Sight::Nothing, look, &mut rng);
        let (seen, act) = step(&mut acts, 0, &mut rng).unwrap();
        assert_eq!(act, Act::EndLook, "the question is waited for");
        acts.end_looked(Sight::RematchAsk, seen, &mut rng);
        let answered = end_until_press(&mut acts, Sight::RematchAsk, &mut rng);
        assert!(answered >= seen + Duration::from_millis(ASK_SEEN.floor as u64));
        assert!(
            answered
                < seen + Duration::from_millis(ASK_SEEN.most as u64) + Duration::from_millis(1)
        );
        acts.end_pressed(Sight::RematchAsk, answered, &mut rng);
        // The room is matching: nothing more.
        let (look, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.end_looked(Sight::Nothing, look, &mut rng);
        assert_eq!(acts.due(), None);
    }

    /// After "play again" only it (the press did not take) and its question
    /// are pressed: a yellow confirm then is something else, never pressed;
    /// no question within a few seconds, and the end is over.
    #[test]
    fn after_play_again_only_it_and_its_question_are_pressed() {
        let mut rng = StdRng::seed_from_u64(22);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.game_over(t0);
        let (look, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.end_looked(Sight::Rematch, look, &mut rng);
        let pressed = end_until_press(&mut acts, Sight::Rematch, &mut rng);
        acts.end_pressed(Sight::Rematch, pressed, &mut rng);
        let again = end_until_press(&mut acts, Sight::Rematch, &mut rng);
        acts.end_pressed(Sight::Rematch, again, &mut rng);
        let mut last = again;
        while let Some((at, act)) = step(&mut acts, 0, &mut rng) {
            assert_eq!(act, Act::EndLook, "nothing pressed");
            last = at;
            acts.end_looked(
                if at.duration_since(again).as_millis() % 2 == 0 {
                    Sight::Standings
                } else {
                    Sight::Reward
                },
                at,
                &mut rng,
            );
        }
        assert!(last >= again + ASK_WAIT && last < again + ASK_WAIT + Duration::from_secs(4));
    }

    /// The question is answered only after "play again" was pressed here: a
    /// question on screen before it is the player's.
    #[test]
    fn a_question_not_asked_here_is_not_answered() {
        let mut rng = StdRng::seed_from_u64(25);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.game_over(t0);
        for _ in 0..20 {
            let (at, act) = step(&mut acts, 0, &mut rng).unwrap();
            assert_eq!(act, Act::EndLook);
            acts.end_looked(Sight::RematchAsk, at, &mut rng);
        }
    }

    /// A screen of the game's own that is gone by the look before the press
    /// is not pressed; nothing unknown is ever pressed, nor anything after
    /// a few minutes or a few presses.
    #[test]
    fn nothing_unknown_or_gone_is_pressed_at_the_end() {
        let mut rng = StdRng::seed_from_u64(23);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.game_over(t0);
        let (look, _) = step(&mut acts, 0, &mut rng).unwrap();
        acts.end_looked(Sight::Standings, look, &mut rng);
        let (again, act) = step(&mut acts, 0, &mut rng).unwrap();
        assert_eq!(act, Act::EndLook);
        acts.end_looked(Sight::Nothing, again, &mut rng);
        // Unknown, unreadable, or a hand's blue confirm: looked at, never pressed.
        let mut at = again;
        for sight in [
            Sight::Button,
            Sight::Unread,
            Sight::RematchAsk,
            Sight::Nothing,
        ]
        .into_iter()
        .cycle()
        {
            let Some((next, act)) = step(&mut acts, 0, &mut rng) else {
                break;
            };
            assert_eq!(act, Act::EndLook);
            at = next;
            acts.end_looked(sight, at, &mut rng);
        }
        assert!(
            at < t0 + END_MOST && at + Duration::from_millis(END_LOOK_EVERY_MS) >= t0 + END_MOST
        );

        // A yellow confirm that never goes is pressed a few times, then left.
        let mut acts = Acts::default();
        acts.game_over(t0);
        let mut presses = 0;
        while let Some((at, act)) = step(&mut acts, 0, &mut rng) {
            match act {
                Act::EndLook => acts.end_looked(Sight::Standings, at, &mut rng),
                Act::EndPress(sight) => {
                    presses += 1;
                    acts.end_pressed(sight, at, &mut rng);
                }
                act => panic!("{act:?}"),
            }
        }
        assert_eq!(presses, END_PRESSES);
    }

    /// The game's last result is looked at first; the game's own screens
    /// only once its looks have seen the first of them.
    #[test]
    fn the_last_result_comes_before_the_game_s_screens() {
        let mut rng = StdRng::seed_from_u64(24);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.result_shown(3, false, t0, &mut rng);
        acts.game_over(ms(t0, 2));
        let (look, act) = step(&mut acts, 3, &mut rng).unwrap();
        assert_eq!(act, Act::Look, "the result's look");
        acts.looked(Sight::Nothing, look, 3, &mut rng);
        let (look, act) = step(&mut acts, 3, &mut rng).unwrap();
        assert_eq!(
            act,
            Act::Look,
            "still the result's, though the end's first look is past due"
        );
        acts.looked(Sight::Standings, look, 3, &mut rng);
        assert!(!acts.result_up(3));
        let (at, act) = step(&mut acts, 3, &mut rng).unwrap();
        assert_eq!((at, act), (ms(t0, 2 + END_LOOK_FIRST_MS), Act::EndLook));
    }

    /// A riichi has auto-win switched on at some moment after it, once.
    #[test]
    fn auto_win_goes_on_some_while_after_a_riichi() {
        let mut rng = StdRng::seed_from_u64(2);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.riichi_accepted(t0, &mut rng);
        let on = acts.due().expect("due to go on");
        assert!(on >= ms(t0, 2000));
        assert_eq!(
            acts.next(ms(t0, 1000), 0, &mut rng),
            None,
            "not before its time"
        );
        assert_eq!(acts.next(on, 0, &mut rng), Some(Act::AutoWinOn));
        assert_eq!(acts.due(), None, "once");
    }

    /// A riichi whose hand ends before auto-win went on stays without it.
    #[test]
    fn a_hand_that_ends_first_keeps_auto_win_off() {
        let mut rng = StdRng::seed_from_u64(3);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.riichi_accepted(t0, &mut rng);
        acts.result_shown(0, false, ms(t0, 100), &mut rng);
        acts.hand_over();
        assert_eq!(acts.due(), None);
    }

    /// An act put off comes again after the wait; a burst as a look first.
    #[test]
    fn an_act_put_off_comes_again() {
        let mut rng = StdRng::seed_from_u64(9);
        let t0 = Instant::now();
        let mut acts = Acts::default();
        acts.riichi_accepted(t0, &mut rng);
        let on = acts.due().unwrap();
        assert_eq!(acts.next(on, 0, &mut rng), Some(Act::AutoWinOn));
        acts.later(Act::AutoWinOn, on, Duration::from_millis(700));
        assert_eq!(acts.due(), Some(on + Duration::from_millis(700)));

        acts.result_shown(0, false, t0, &mut rng);
        let at = see(&mut acts, Sight::Button, 0, &mut rng);
        let (_, act) = step(&mut acts, 0, &mut rng).unwrap();
        assert!(matches!(act, Act::Burst(_)));
        acts.later(act, at, Duration::from_millis(300));
        assert_eq!(
            acts.next(at + Duration::from_millis(300), 0, &mut rng),
            Some(Act::Look)
        );
    }

    /// The longer a riichi lasts, the likelier auto-win went on in it: any
    /// moment as likely as the next.
    #[test]
    fn a_longer_riichi_is_likelier_to_have_auto_win_on() {
        let mut rng = StdRng::seed_from_u64(4);
        let mut within = |secs: u64| {
            (0..4000)
                .filter(|_| u64::from(AUTO_WIN_ON.draw_ms(&mut rng)) <= secs * 1000)
                .count() as f64
                / 4000.0
        };
        let (short, long) = (within(10), within(40));
        // 1 - e^(-(t - 2) / 25): about 0.27 by 10 s, 0.78 by 40 s.
        assert!((short - 0.274).abs() < 0.04, "{short}");
        assert!((long - 0.781).abs() < 0.04, "{long}");
    }
}
