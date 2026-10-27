//! The pointer over one hand, and the one place it is moved and pressed.
//!
//! [`Hand`] keeps where the pointer is and what the bot expects of the seat's
//! next turn, and makes every move and every press autoplay makes — no other
//! code sends the pointer anywhere. Others say what they want: the way to a
//! decision's press ([`super::approach`]), a plan's next press, a press made
//! again, a press between decisions, the rest after a press, the lift after
//! a tsumogiri, the looks between turns ([`super::lean`]). Where each ends is
//! [`super::spots`]'s and [`super::aim`]'s, every time and distance
//! `humanize`'s, and every move passes [`super::rules`] here, in-line or
//! queued ([`super::queue`]), whoever planned it: [`Hand::go`] for a move
//! made at once, [`Hand::step`] for one that waited its turn, [`Hand::press`]
//! for a press.
//!
//! What it knows belongs to one hand, but for where the pointer is: the
//! manager resets it at every hand's start and end, and the pointer stays
//! where it was.

use super::approach::{
    approach, changed_mind, dealt_lead, wait_to_see, Approach, LOOK_MS, LOOK_REACH,
};
use super::lean::{self, Lean, Spot};
use super::queue::{self, Errand, Motion, Queue};
use super::rules::{self, Bound, Sight};
use super::{along, distance, leave, spots, Point};
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::humanize;
use crate::autoplay::platform::{PlanResult, Step};
use crate::schema::MjaiEvent;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub struct Hand {
    ctx: Arc<AutoplayContext>,
    /// Where the pointer was last put, in CSS pixels. `None` until its first
    /// move of the hand.
    at: Option<Point>,
    /// What the bot expects of the seat's next turn, and when the pointer
    /// next looks at it. `None` on the seat's own turn and outside a hand.
    lean: Option<Lean>,
    glance_at: Option<Instant>,
    /// When our draw came, the bot expected to throw it: a tsumogiri then
    /// lifts the pointer straight up off the hand rather than taking it to
    /// the buttons.
    expected_tsumogiri: bool,
    /// The spot the pointer rests on after a look between turns, while it
    /// does: a decision that presses another moves as a changed mind.
    looked_at: Option<Spot>,
    /// The way the pointer last moved, as a unit direction, and when it got
    /// there: a sharp turn from it waits for a stop ([`rules::turn_wait`]).
    last_move: Option<(Point, Instant)>,
    /// What the rules need to know of the table ([`rules::refusal`]).
    sight: Sight,
    /// The rest and the looks waiting their turn, and the one under way.
    queue: Queue,
    moving: Option<Moving>,
}

/// An unhurried move under way, made a frame at a time by [`Hand::step`].
#[derive(Debug)]
struct Moving {
    motion: Motion,
    from: Point,
    started: Instant,
    next_frame: Instant,
    /// The last two frames sent, for the speed a move cut short brakes from.
    before: Option<(Point, Instant)>,
    last: (Point, Instant),
}

impl Hand {
    pub fn new(ctx: Arc<AutoplayContext>) -> Self {
        Self {
            ctx,
            at: None,
            lean: None,
            glance_at: None,
            expected_tsumogiri: false,
            looked_at: None,
            last_move: None,
            sight: Sight::default(),
            queue: Queue::default(),
            moving: None,
        }
    }

    /// Forget the hand: the next one starts with nothing known but where
    /// the pointer is, which a new hand does not move — a rest under way or
    /// waiting goes on; looks at the old hand do not.
    pub fn reset(&mut self) {
        let mut queue = std::mem::take(&mut self.queue);
        queue.drop_looks();
        let moving = self
            .moving
            .take()
            .filter(|m| matches!(m.motion.errand, Errand::Rest { .. }));
        *self = Self {
            at: self.at,
            last_move: self.last_move,
            queue,
            moving,
            ..Self::new(self.ctx.clone())
        };
    }

    /// Where the pointer is, in CSS pixels, once it has moved.
    pub fn at(&self) -> Option<Point> {
        self.at
    }

    /// Go on from where the user's own mouse left the pointer, if it has
    /// since autoplay last moved it: the page's pointer is there now, and a
    /// move from anywhere else would start with a jump.
    pub fn sync_with_user(&mut self) {
        if let Some(at) = crate::autoplay::cdp_input::take_user_left_pointer() {
            info!(
                "autoplay: pointer taken up where the user's mouse left it ({:.0}, {:.0})",
                at.0, at.1
            );
            self.at = Some(at);
            // The user's hand moved it last; there is no way of ours to turn from.
            self.last_move = None;
            self.looked_at = None;
        }
    }

    /// When the pointer next looks at the seat's next turn, while it looks.
    pub fn glance_due(&self) -> Option<Instant> {
        self.glance_at
    }

    /// Plan the pointer's way to the plan's first press and take its time
    /// out of the pause that leads the plan. `None` for a plan that does not
    /// open with a pause and a press: there is no thinking time to move in.
    /// `glance` is the runner-up to look at, as a normalised point; `shown_at`
    /// when what the decision answers came on screen, before which the
    /// pointer cannot have seen it (see [`wait_to_see`]); after a deal the
    /// whole way is made once the hand can have been seen ([`dealt_lead`]).
    /// A decision
    /// that presses another tile than the one the pointer rests on after its
    /// looks between turns is a changed mind ([`changed_mind`]).
    pub fn plan_approach(
        &mut self,
        plan: &mut PlanResult,
        rect: CanvasRect,
        glance: Option<(f64, f64)>,
        shown_at: Option<Instant>,
        action: &MjaiEvent,
    ) -> Option<Approach> {
        self.sync_with_user();
        let looked_at = self.looked_at.take();
        let rest_ms = plan.look_ms.unwrap_or(LOOK_MS);
        let dealt = plan.dealt;
        let [Step::Sleep { duration_ms }, Step::Click { x_norm, y_norm }, ..] =
            plan.steps.as_mut_slice()
        else {
            return None;
        };
        let mut rng = rand::rng();
        // How long from now until what it answers can have been seen.
        let see_ms = shown_at.map(|shown_at| {
            let seen_at =
                shown_at + Duration::from_millis(u64::from(humanize::SEE.draw_ms(&mut rng)));
            let ms = seen_at
                .saturating_duration_since(Instant::now())
                .as_millis();
            u32::try_from(ms).unwrap_or(u32::MAX)
        });
        let lead = see_ms.map_or(0, |see_ms| dealt_lead(dealt, see_ms, *duration_ms));
        let think_ms = *duration_ms - lead;
        // A pointer that has not been anywhere yet starts from where it
        // would have been resting.
        let from = *self.at.get_or_insert_with(|| spots::rest(rect, &mut rng));
        // The runner-up is rested on where a tile is: high on it, as a press.
        let glance = glance.map(|at| spots::on_tile(at, rect, &mut rng));
        let press = rect.pixel(*x_norm, *y_norm);
        let mut approach = match (looked_at, pressed_spot(action)) {
            (Some(old), Some(new)) if old != new => {
                info!("autoplay: a changed mind: the pointer was on {old:?}, pressing {new:?}");
                let think = think_ms + humanize::CHANGED_MIND.draw_ms(&mut rng);
                changed_mind(think, from, press, rest_ms, rect.width / 16.0, &mut rng)
            }
            _ => approach(
                think_ms,
                from,
                press,
                glance,
                rest_ms,
                rect.width / 16.0,
                &mut rng,
            ),
        };
        approach.pause_ms += lead;
        if let Some(see_ms) = see_ms {
            wait_to_see(&mut approach, see_ms);
        }
        *duration_ms = approach.pause_ms;
        Some(approach)
    }

    /// Walk the pointer along `approach`. False when it could not be moved.
    pub async fn walk(&mut self, approach: &Approach) -> bool {
        for leg in &approach.legs {
            if !self.go("approach", leg.to, leg.travel_ms, leg.bend).await {
                return false;
            }
            if leg.dwell_ms > 0 {
                tokio::time::sleep(Duration::from_millis(u64::from(leg.dwell_ms))).await;
            }
        }
        true
    }

    /// A new lean from the bot. The pointer keeps its own pace; only a
    /// changed mind brings its next look forward, by a reaction time.
    pub fn take_lean(&mut self, lean: Lean) {
        let changed = self.lean.as_ref().and_then(|l| l.first()) != lean.first();
        self.lean = Some(lean);
        if let (true, Some(at)) = (changed, self.glance_at) {
            self.glance_at = Some(at.min(after_a_reaction()));
        }
    }

    /// Our draw came: whether the bot expected to throw it, read before the
    /// looks stop for our turn. The tile is on screen: where it lands is no
    /// longer kept clear.
    pub fn drew(&mut self) {
        self.expected_tsumogiri =
            self.lean.as_ref().and_then(|lean| lean.first()) == Some(Spot::Draw);
        self.sight.draw_slot = None;
    }

    /// The seat's hand at rest holds `count` tiles, on the canvas at `rect`:
    /// where its next draw lands is kept clear until the tile is there.
    pub fn saw_hand(&mut self, count: usize, rect: CanvasRect) {
        self.sight.draw_slot = Some(spots::draw_slot(count, rect));
    }

    /// Start looking at the next turn, a while after the pointer was last
    /// put to rest — if the pointer is to lean at all.
    pub fn settle_glances(&mut self) {
        let settle = humanize::SETTLE.draw_ms(&mut rand::rng());
        self.glance_at = Some(Instant::now() + Duration::from_millis(u64::from(settle)));
    }

    pub fn stop_glances(&mut self) {
        self.glance_at = None;
        self.lean = None;
        self.queue.drop_looks();
    }

    /// The spot this look goes to, if it goes anywhere (see
    /// [`lean::glance`]); the look after it is scheduled either way.
    pub fn next_glance(&mut self) -> Option<Spot> {
        let mut rng = rand::rng();
        let spot = self
            .lean
            .as_ref()
            .and_then(|lean| lean::glance(lean, &mut rng));
        let wait = lean::wait_ms(self.lean.as_ref(), &mut rng);
        self.glance_at = Some(Instant::now() + Duration::from_millis(u64::from(wait)));
        spot
    }

    /// Look at `spot` of a hand at rest holding `tehai`, on the canvas at
    /// `rect`: the look waits its turn in the queue.
    pub fn look_at(&mut self, mut spot: Spot, tehai: &[String], rect: CanvasRect) {
        self.sync_with_user();
        let pointer = self.at.map(|(x, y)| rect.norm(x, y));
        let leg = {
            let mut rng = rand::rng();
            let point = |spot: &Spot, rng: &mut rand::rngs::ThreadRng| match spot {
                Spot::Hand(pai) => crate::autoplay::majsoul::hand_point(pai, tehai, pointer, rng),
                Spot::Draw => Some(crate::autoplay::majsoul::draw_point(tehai.len())),
            };
            let Some(mut at) = point(&spot, &mut rng) else {
                return;
            };
            // Weighing two tiles is a look between neighbours: one far from
            // the likeliest is not looked at, the likeliest is.
            let first = self.lean.as_ref().and_then(|lean| lean.first());
            if let Some(likeliest) = first.filter(|main| *main != spot) {
                if let Some(main) = point(&likeliest, &mut rng) {
                    if (at.0 - main.0).abs() > LOOK_REACH {
                        at = main;
                        spot = likeliest;
                    }
                }
            }
            // A look at the tile the hand is already on is a look that stays.
            if crate::autoplay::majsoul::on_tile(at, pointer) {
                self.looked_at = Some(spot);
                return;
            }
            // Not the middle of the tile every time, and high on it, as a press.
            let to = spots::on_tile(at, rect, &mut rng);
            // A look the rules would refuse — where the draw lands, with a
            // tsumogiri the likeliest — is no look, and not queued to be
            // refused again and again; the rules still have the last word
            // when a look sets off.
            if rules::refusal(to, Bound::Errand, &self.sight).is_some() {
                return;
            }
            let from = self.at.unwrap_or(to);
            leave(from, to, rect.width / 16.0, &mut rng)
        };
        // A look is unhurried: half the speed of the way to a press.
        self.queue.push(Motion {
            errand: Errand::Look(spot),
            to: leg.to,
            travel_ms: leg.travel_ms * 2,
            bend: leg.bend,
            not_before: Instant::now(),
        });
    }

    /// Take the pointer off what the plan last pressed, to somewhere the
    /// client draws nothing under it, and look on to the next turn from
    /// there.
    ///
    /// After a tsumogiri the bot expected when the tile came, the hand lifts
    /// straight up off the hand instead ([`Self::queue_lift`]): it stays near,
    /// but never where the next draw lands before the tile is there. Not after
    /// a call: the seat's own discard comes next, and a hand goes from the
    /// button to the tile it throws, not up to wait and straight back down.
    pub fn rest_after(&mut self, rect: CanvasRect, plan: &PlanResult, action: &MjaiEvent) {
        let threw_draw = matches!(
            action,
            MjaiEvent::Dahai {
                tsumogiri: true,
                ..
            }
        );
        let lift = std::mem::take(&mut self.expected_tsumogiri) && threw_draw;
        if discards_next(action) {
            return;
        }
        let Some(pressed) = plan.steps.iter().rev().find_map(|s| match s {
            Step::Click { x_norm, y_norm } => Some((*x_norm, *y_norm)),
            _ => None,
        }) else {
            return;
        };
        let from = self.at.unwrap_or_else(|| rect.pixel(pressed.0, pressed.1));
        if lift {
            self.queue_lift(from, rect);
        } else {
            self.queue_rest(from, rect, true);
        }
    }

    /// Queue the hand lifting straight up off the tile it threw at `from`,
    /// after lingering on it, and the looks at the next turn once it is up.
    fn queue_lift(&mut self, from: Point, rect: CanvasRect) {
        let leg = {
            let mut rng = rand::rng();
            let to = spots::lift(from, rect, &mut rng);
            leave(from, to, rect.width / 16.0, &mut rng)
        };
        self.queue.push(Motion {
            errand: Errand::Rest { settle: true },
            to: leg.to,
            travel_ms: leg.travel_ms,
            bend: leg.bend,
            not_before: Instant::now()
                + Duration::from_millis(u64::from(leg.dwell_ms.min(humanize::LIFT_LINGER_MOST_MS))),
        });
    }

    /// Take the pointer off what it last pressed between decisions, to where it waits.
    pub fn rest(&mut self, rect: CanvasRect) {
        self.sync_with_user();
        if let Some(from) = self.at {
            self.queue_rest(from, rect, false);
        }
    }

    /// Queue the way from `from` to a rest, after the hand has lingered on
    /// what it pressed; with `settle`, the looks at the next turn start once
    /// it is there.
    fn queue_rest(&mut self, from: Point, rect: CanvasRect, settle: bool) {
        let leg = {
            let mut rng = rand::rng();
            let to = spots::rest(rect, &mut rng);
            leave(from, to, rect.width / 16.0, &mut rng)
        };
        self.queue.push(Motion {
            errand: Errand::Rest { settle },
            to: leg.to,
            travel_ms: leg.travel_ms,
            bend: leg.bend,
            not_before: Instant::now() + Duration::from_millis(u64::from(leg.dwell_ms)),
        });
    }

    /// When the next step of the unhurried moves is due: the next frame of
    /// the one under way, else when the first waiting may set off.
    pub fn motion_due(&self) -> Option<Instant> {
        match &self.moving {
            Some(moving) => Some(moving.next_frame),
            None => self.queue.due(),
        }
    }

    /// One step of the unhurried moves: a frame of the one under way, or the
    /// start of the next waiting. Nothing of ours moves while the user's hand
    /// is on the mouse.
    pub async fn step(&mut self) {
        if crate::autoplay::cdp_input::user_active() {
            if let Some(moving) = self.moving.take() {
                info!(
                    "autoplay: pointer {} given up to the user's hand",
                    moving.motion.errand.name()
                );
            }
            self.queue
                .put_off(Instant::now() + Duration::from_millis(humanize::USER_PUT_OFF_MS));
            return;
        }
        if self.moving.is_none() && !self.set_off() {
            return;
        }
        self.frame().await;
    }

    /// Start the first waiting move, if it may go now. False when none does:
    /// none is due, the rules refuse it (it is dropped), or a sharp turn
    /// waits for its stop first.
    fn set_off(&mut self) -> bool {
        self.sync_with_user();
        let now = Instant::now();
        let Some(motion) = self.queue.take(now) else {
            return false;
        };
        if let Some(refusal) = rules::refusal(motion.to, Bound::Errand, &self.sight) {
            info!(
                "autoplay: pointer {} to ({:.0}, {:.0}) not made: {refusal:?}",
                motion.errand.name(),
                motion.to.0,
                motion.to.1
            );
            return false;
        }
        let from = self.at.unwrap_or(motion.to);
        let wait = rules::turn_wait(self.last_move, from, motion.to, now, &mut rand::rng());
        if let Some(wait) = wait {
            let not_before = now + wait;
            self.queue.push_front(Motion {
                not_before,
                ..motion
            });
            return false;
        }
        info!(
            "autoplay: pointer {} ({:.0}, {:.0}) -> ({:.0}, {:.0}) in {} ms",
            motion.errand.name(),
            from.0,
            from.1,
            motion.to.0,
            motion.to.1,
            motion.travel_ms
        );
        self.moving = Some(Moving {
            motion,
            from,
            started: now,
            next_frame: now,
            before: None,
            last: (from, now),
        });
        true
    }

    /// Send the next frame of the move under way; at its end, the move has
    /// arrived.
    async fn frame(&mut self) {
        let Some(moving) = &self.moving else {
            return;
        };
        let now = Instant::now();
        let total = f64::from(moving.motion.travel_ms.max(1));
        let t = now.saturating_duration_since(moving.started).as_secs_f64() * 1000.0 / total;
        let point = if t >= 1.0 {
            moving.motion.to
        } else {
            along(moving.from, moving.motion.to, moving.motion.bend, t)
        };
        if !self.send(point).await {
            self.moving = None;
            return;
        }
        let Some(moving) = &mut self.moving else {
            return;
        };
        moving.before = Some(moving.last);
        moving.last = (point, now);
        self.at = Some(point);
        if t < 1.0 {
            let frame = humanize::frame_ms(&mut rand::rng());
            moving.next_frame = now + Duration::from_millis(frame);
            return;
        }
        let Some(arrived) = self.moving.take() else {
            return;
        };
        if let Some(way) = rules::direction(arrived.from, arrived.motion.to) {
            self.last_move = Some((way, now));
        }
        match arrived.motion.errand {
            Errand::Rest { settle } => {
                self.looked_at = None;
                if settle {
                    self.settle_glances();
                }
            }
            Errand::Look(spot) => self.looked_at = Some(spot),
        }
    }

    /// Give way to a press: the move under way brakes to a stop where it is,
    /// along the way it was going, and the moves waiting are dropped — what
    /// they were for is behind the press. The press's way starts from where
    /// the pointer stopped.
    pub async fn give_way(&mut self) {
        self.queue.clear();
        let Some(moving) = self.moving.take() else {
            return;
        };
        // Cut short, the pointer is on neither what it left nor what it went for.
        self.looked_at = None;
        let (at, then) = moving.last;
        let velocity = match moving.before {
            Some((before, earlier)) => {
                let ms = then.saturating_duration_since(earlier).as_secs_f64() * 1000.0;
                if ms > 0.0 {
                    ((at.0 - before.0) / ms, (at.1 - before.1) / ms)
                } else {
                    (0.0, 0.0)
                }
            }
            None => (0.0, 0.0),
        };
        let (stop, brake_ms) = queue::brake(at, velocity, distance(at, moving.motion.to));
        info!(
            "autoplay: pointer {} cut short at ({:.0}, {:.0}), braking {brake_ms} ms to ({:.0}, {:.0})",
            moving.motion.errand.name(),
            at.0,
            at.1,
            stop.0,
            stop.1
        );
        let started = Instant::now();
        let total = f64::from(brake_ms.max(1));
        loop {
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let u = elapsed / total;
            let point = if brake_ms == 0 || u >= 1.0 {
                stop
            } else {
                queue::braking(at, stop, u)
            };
            if !self.send(point).await {
                break;
            }
            self.at = Some(point);
            if point == stop {
                break;
            }
            let frame = humanize::frame_ms(&mut rand::rng());
            tokio::time::sleep(Duration::from_millis(frame)).await;
        }
        if let Some(way) = rules::direction(moving.from, stop) {
            self.last_move = Some((way, Instant::now()));
        }
    }

    /// Put the pointer at `point` in one frame. False when it could not be.
    async fn send(&self, point: Point) -> bool {
        let page_guard = self.ctx.page.read().await;
        let Some(page) = page_guard.as_ref() else {
            return false;
        };
        match crate::autoplay::cdp_input::step_to(page, point.0, point.1).await {
            Ok(()) => true,
            Err(e) => {
                warn!("autoplay: moving the pointer failed: {e:#}");
                false
            }
        }
    }

    /// Press at `to`, in CSS pixels: the one way any press is made — a
    /// decision's (after its approach), a plan's next press, a press made
    /// again, a press between decisions. The pointer goes there first if it
    /// is not there ([`Self::go`]), then rests on it about `touch.hover_ms`
    /// and holds the button about `touch.hold_ms` (`cdp_input`). False when
    /// the pointer could not be moved there or the press failed.
    pub async fn press(&mut self, why: &str, to: Point, rect: CanvasRect, touch: Touch) -> bool {
        self.press_when(why, to, rect, touch, |_| async { true })
            .await
    }

    /// As `press`, with a final page check after the pointer has arrived
    /// and hovered. Only a dispatched press returns true.
    pub async fn press_when<F, Fut>(
        &mut self,
        why: &str,
        to: Point,
        rect: CanvasRect,
        touch: Touch,
        check: F,
    ) -> bool
    where
        F: FnOnce(chromiumoxide::page::Page) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        if !self.reach(why, to, rect).await {
            return false;
        }
        let page_guard = self.ctx.page.read().await;
        let Some(page) = page_guard.as_ref() else {
            warn!("autoplay: no page handle — cannot press ({why})");
            return false;
        };
        let pressed = crate::autoplay::cdp_input::dispatch_click_guarded(
            page,
            to.0,
            to.1,
            touch.hover_ms,
            touch.hold_ms,
            touch.jiggle,
            check,
        )
        .await;
        drop(page_guard);
        self.at = Some(to);
        match pressed {
            Ok(pressed) => pressed,
            Err(e) => {
                warn!("autoplay: a press ({why}) failed: {e:#}");
                false
            }
        }
    }

    /// Take the pointer to `to` for a press, along an arc ([`super::leave`])
    /// from wherever it is — from where it would have been resting, before
    /// it has been anywhere. Nothing to do where it already is.
    async fn reach(&mut self, why: &str, to: Point, rect: CanvasRect) -> bool {
        self.sync_with_user();
        let leg = {
            let mut rng = rand::rng();
            let from = *self.at.get_or_insert_with(|| spots::rest(rect, &mut rng));
            if distance(from, to) < 1.0 {
                return true;
            }
            leave(from, to, rect.width / 16.0, &mut rng)
        };
        self.go(why, leg.to, leg.travel_ms, leg.bend).await
    }

    /// Move the pointer to `to` over `travel_ms` now, on the way to a press,
    /// a move the log calls `why`: the one way a move is made in-line. A
    /// move under way of the unhurried ones gives way to it; it passes the
    /// rules as a press's ([`Bound::Press`]), and a sharp turn waits for its
    /// stop; a nudge of a few pixels is one step. False when it was refused
    /// or could not be made.
    async fn go(&mut self, why: &str, to: Point, travel_ms: u32, bend: f64) -> bool {
        self.give_way().await;
        self.sync_with_user();
        if let Some(refusal) = rules::refusal(to, Bound::Press, &self.sight) {
            warn!(
                "autoplay: pointer {why} to ({:.0}, {:.0}) not made: {refusal:?}",
                to.0, to.1
            );
            return false;
        }
        let from = self.at.unwrap_or(to);
        if distance(from, to) < humanize::NUDGE_PX {
            if !self.send(to).await {
                return false;
            }
            self.at = Some(to);
            return true;
        }
        let wait = rules::turn_wait(self.last_move, from, to, Instant::now(), &mut rand::rng());
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        info!(
            "autoplay: pointer {why} ({:.0}, {:.0}) -> ({:.0}, {:.0}) in {travel_ms} ms{}",
            from.0,
            from.1,
            to.0,
            to.1,
            wait.map(|w| format!(", after a stop of {} ms to turn", w.as_millis()))
                .unwrap_or_default()
        );
        let page_guard = self.ctx.page.read().await;
        let Some(page) = page_guard.as_ref() else {
            warn!("autoplay: no page handle — cannot move the pointer");
            return false;
        };
        if let Err(e) = crate::autoplay::cdp_input::travel(page, from, to, travel_ms, bend).await {
            warn!("autoplay: moving the pointer failed: {e:#}");
            return false;
        }
        drop(page_guard);
        self.at = Some(to);
        if let Some(way) = rules::direction(from, to) {
            self.last_move = Some((way, Instant::now()));
        }
        true
    }
}

/// How a press is made: about `hover_ms` on the target before pressing it,
/// the button held about `hold_ms` (each drawn about by `humanize::HOVER` and
/// `humanize::HOLD`), and with `jiggle` nudged a pixel while it is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub hover_ms: u32,
    pub hold_ms: u32,
    pub jiggle: bool,
}

/// The spot of the hand a decision presses: for a discard, or a riichi's,
/// the tile drawn when it throws that, else the tile. `None` for a button.
fn pressed_spot(action: &MjaiEvent) -> Option<Spot> {
    let (pai, tsumogiri) = match action {
        MjaiEvent::Dahai { pai, tsumogiri, .. } => (pai, *tsumogiri),
        MjaiEvent::Reach {
            pai: Some(pai),
            tsumogiri,
            ..
        } => (pai, *tsumogiri == Some(true)),
        _ => return None,
    };
    Some(if tsumogiri {
        Spot::Draw
    } else {
        Spot::Hand(pai.clone())
    })
}

/// Whether the seat's own discard follows `action`: a call, after which the
/// seat throws a tile (a kan's or a kita's once its replacement is drawn).
fn discards_next(action: &MjaiEvent) -> bool {
    matches!(
        action,
        MjaiEvent::Chi { .. }
            | MjaiEvent::Pon { .. }
            | MjaiEvent::Daiminkan { .. }
            | MjaiEvent::Kakan { .. }
            | MjaiEvent::Ankan { .. }
            | MjaiEvent::Kita { .. }
    )
}

/// A reaction time from now: how soon a changed plan moves the pointer.
fn after_a_reaction() -> Instant {
    let reaction = humanize::REACTION.draw_ms(&mut rand::rng());
    Instant::now() + Duration::from_millis(u64::from(reaction))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_is_followed_by_the_seats_own_discard_and_a_discard_is_not() {
        let pon = MjaiEvent::Pon {
            actor: 0,
            target: 2,
            pai: "5m".into(),
            consumed: ["5m".into(), "5m".into()],
        };
        let dahai = MjaiEvent::Dahai {
            actor: 0,
            pai: "5m".into(),
            tsumogiri: false,
        };
        assert!(discards_next(&pon));
        assert!(!discards_next(&dahai));
        let reach = MjaiEvent::Reach {
            actor: 0,
            pai: Some("5m".into()),
            tsumogiri: Some(false),
        };
        assert!(!discards_next(&reach));
    }
}
