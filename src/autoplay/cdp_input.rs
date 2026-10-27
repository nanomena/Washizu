//! Thin wrappers around chromiumoxide for the autoplay manager.
//!
//! Centralised so the click + canvas-rect query logic can be unit-mocked
//! and the manager keeps a single dependency on chromiumoxide types.

use crate::autoplay::context::CanvasRect;
use anyhow::{anyhow, Context, Result};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
};
use chromiumoxide::layout::Point;
use chromiumoxide::page::Page;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::broadcast;

/// Whether the user's own mouse is moving over the game, as the shell sees
/// the system cursor (which input sent over CDP never moves). The page takes
/// both kinds of input, and a press of ours made while the user's mouse moves
/// can be read by the client as part of the user's drag, and come to nothing.
static USER_HAND: AtomicBool = AtomicBool::new(false);
static USER_REVISION: AtomicU64 = AtomicU64::new(0);

pub fn user_revision() -> u64 {
    USER_REVISION.load(Ordering::SeqCst)
}

/// Until when a hand that has come to rest still counts as on the mouse:
/// autoplay has the pointer back a while after the user stops, a while of its
/// own each time (`humanize::HAND_BACK`).
static RESTING_UNTIL: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// The shell's word on the user's mouse: moving over the game, or come to rest.
pub fn set_user_active(active: bool) {
    if active && !USER_HAND.load(Ordering::Relaxed) {
        USER_REVISION.fetch_add(1, Ordering::SeqCst);
    }
    let until = (!active).then(|| {
        let wait = crate::autoplay::humanize::HAND_BACK.draw_ms(&mut rand::rng());
        std::time::Instant::now() + Duration::from_millis(u64::from(wait))
    });
    if let Ok(mut resting) = RESTING_UNTIL.lock() {
        *resting = until;
    }
    USER_HAND.store(active, Ordering::Relaxed);
}

/// Whether the user's hand is on the mouse over the game: moving, or not yet
/// rested long enough for autoplay to have the pointer back.
pub fn user_active() -> bool {
    USER_HAND.load(Ordering::Relaxed)
        || RESTING_UNTIL
            .lock()
            .ok()
            .and_then(|until| *until)
            .is_some_and(|until| std::time::Instant::now() < until)
}

/// Where the user's own mouse last left the game's pointer, when it came to
/// rest over the game: the page's pointer is there, wherever autoplay last
/// put it, since the user's input moves it too. Taken once (see
/// [`take_user_left_pointer`]).
static USER_LEFT: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);

pub fn user_left_pointer_at(at: (f64, f64)) {
    if let Ok(mut left) = USER_LEFT.lock() {
        *left = Some(at);
    }
}

/// Where the user's mouse left the pointer since this was last asked, if it did.
/// The next mark then starts a fresh stretch of the path: the pointer got there
/// by the user's hand, not by a move autoplay made.
pub fn take_user_left_pointer() -> Option<(f64, f64)> {
    let left = USER_LEFT.lock().ok().and_then(|mut left| left.take());
    if left.is_some() {
        FRESH.store(true, Ordering::Relaxed);
    }
    left
}

/// The next mark starts a fresh stretch of the path (see [`PointerMark::fresh`]).
static FRESH: AtomicBool = AtomicBool::new(false);

/// Where the pointer autoplay drives is, in the page's CSS pixels, and whether
/// it is pressed. The page shows no cursor for input sent over CDP; these are
/// for whoever wants to draw one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PointerMark {
    pub x: f64,
    pub y: f64,
    pub down: bool,
    /// The path does not come here from the mark before: the user's own
    /// mouse moved the pointer in between.
    pub fresh: bool,
}

static MARKS: OnceLock<broadcast::Sender<PointerMark>> = OnceLock::new();

fn marks() -> &'static broadcast::Sender<PointerMark> {
    MARKS.get_or_init(|| broadcast::channel(256).0)
}

/// Every mark from now on. Marks are sent and forgotten: with no one
/// listening they cost nothing, and a listener that falls behind loses the
/// oldest.
pub fn pointer_marks() -> broadcast::Receiver<PointerMark> {
    marks().subscribe()
}

fn mark(x: f64, y: f64, down: bool) {
    let fresh = FRESH.swap(false, Ordering::Relaxed);
    let _ = marks().send(PointerMark { x, y, down, fresh });
}

/// Put the pointer at `(x, y)` in one step: one frame of a move made a frame
/// at a time (`autoplay::pointer::queue`).
pub async fn step_to(page: &Page, x: f64, y: f64) -> Result<()> {
    if user_active() {
        return Err(anyhow!("manual pointer ownership"));
    }
    move_to(page, x, y).await
}

/// Move the pointer, and say so.
async fn move_to(page: &Page, x: f64, y: f64) -> Result<()> {
    page.move_mouse(Point::new(x, y))
        .await
        .context("CDP move_mouse")?;
    mark(x, y, false);
    Ok(())
}

/// Dispatch a single mouse click at `(x, y)` (CSS pixels) as four CDP
/// events, with mandatory hover before press:
///
/// 1. `mouseMoved` to `(x, y)`
/// 2. sleep `hover_delay_ms` (≥100ms — Laya's input system samples hover
///    state before mousedown registers a hit on a tile sprite)
/// 3. `mousePressed`
/// 4. sleep `click_hold_ms`
/// 5. `mouseReleased`
///
/// `chromiumoxide::Page::click` collapses 3+5 into back-to-back frames
/// without the hover delay, which Majsoul drops on the floor for hand
/// tiles. Hand-rolling the sequence is required.
pub async fn dispatch_click(
    page: &Page,
    x: f64,
    y: f64,
    hover_delay_ms: u32,
    click_hold_ms: u32,
) -> Result<()> {
    dispatch_click_shaped(page, x, y, hover_delay_ms, click_hold_ms, false).await
}

/// As [`dispatch_click`], but able to vary the *shape* of the press.
///
/// `jiggle` nudges the cursor a pixel mid-press and puts it back. It
/// exists for retries: when a press lands on the right control and the
/// action still does not commit, the position is not what is wrong, so
/// the only thing left to change is how the press is made.
pub async fn dispatch_click_shaped(
    page: &Page,
    x: f64,
    y: f64,
    hover_delay_ms: u32,
    click_hold_ms: u32,
    jiggle: bool,
) -> Result<()> {
    dispatch_click_guarded(
        page,
        x,
        y,
        hover_delay_ms,
        click_hold_ms,
        jiggle,
        |_| async { true },
    )
    .await
    .map(|_| ())
}

/// Check the current page after travel, user wait and hover, immediately
/// before mouse-down. A rejected check sends no button events.
pub async fn dispatch_click_guarded<F, Fut>(
    page: &Page,
    x: f64,
    y: f64,
    hover_delay_ms: u32,
    click_hold_ms: u32,
    jiggle: bool,
    check: F,
) -> Result<bool>
where
    F: FnOnce(Page) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    // About the configured times, each press its own: mostly near them, now and then longer.
    let (hover_delay_ms, click_hold_ms) = {
        use crate::autoplay::humanize::{HOLD, HOVER};
        let mut rng = rand::rng();
        (
            HOVER.draw_ms(hover_delay_ms, &mut rng),
            HOLD.draw_ms(click_hold_ms, &mut rng),
        )
    };
    if user_active() {
        return Ok(false);
    }
    guarded_mouse_sequence(
        &mut PageMouse(page),
        (x, y),
        hover_delay_ms,
        click_hold_ms,
        jiggle,
        || check(page.clone()),
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseEvent {
    Move,
    Down,
    Up,
}

#[async_trait::async_trait]
#[allow(clippy::double_must_use)] // async_trait adds this attribute to boxed futures.
pub(crate) trait MouseSink: Send {
    async fn send(&mut self, event: MouseEvent, point: (f64, f64)) -> Result<()>;
}

struct PageMouse<'a>(&'a Page);

#[async_trait::async_trait]
impl MouseSink for PageMouse<'_> {
    async fn send(&mut self, event: MouseEvent, (x, y): (f64, f64)) -> Result<()> {
        if event == MouseEvent::Move {
            return step_to(self.0, x, y).await;
        }
        let ty = match event {
            MouseEvent::Down => DispatchMouseEventType::MousePressed,
            MouseEvent::Up => DispatchMouseEventType::MouseReleased,
            MouseEvent::Move => unreachable!(),
        };
        let params = DispatchMouseEventParams::builder()
            .r#type(ty)
            .x(x)
            .y(y)
            .button(MouseButton::Left)
            .click_count(1)
            .build()
            .map_err(|e| anyhow!("build mouse event: {e}"))?;
        self.0.execute(params).await.context("CDP mouse button")?;
        mark(x, y, event == MouseEvent::Down);
        Ok(())
    }
}

/// Production and isolated tests share the full move/hover/final-check/
/// down/hold/up boundary. A refused check cannot emit either button event.
pub(crate) async fn guarded_mouse_sequence<S, F, Fut>(
    sink: &mut S,
    point: (f64, f64),
    hover_delay_ms: u32,
    click_hold_ms: u32,
    jiggle: bool,
    check: F,
) -> Result<bool>
where
    S: MouseSink,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    sink.send(MouseEvent::Move, point).await?;
    if hover_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(hover_delay_ms as u64)).await;
    }
    if !check().await {
        return Ok(false);
    }
    if let Err(e) = sink.send(MouseEvent::Down, point).await {
        let _ = sink.send(MouseEvent::Up, point).await;
        return Err(e);
    }
    let held: Result<()> = async {
        if jiggle {
            // Split the hold around the nudge so its total stays what the
            // config says. The move is one CSS pixel — enough for the engine
            // to resample the cursor while the button is down, not enough to
            // leave the control.
            let half = u64::from(click_hold_ms) / 2;
            if half > 0 {
                tokio::time::sleep(Duration::from_millis(half)).await;
            }
            sink.send(MouseEvent::Move, (point.0 + 1.0, point.1))
                .await?;
            sink.send(MouseEvent::Move, point).await?;
            let rest = u64::from(click_hold_ms) - half;
            if rest > 0 {
                tokio::time::sleep(Duration::from_millis(rest)).await;
            }
        } else if click_hold_ms > 0 {
            tokio::time::sleep(Duration::from_millis(click_hold_ms as u64)).await;
        }
        Ok(())
    }
    .await;
    // A transport error during a held jiggle must still release the button.
    let released = sink.send(MouseEvent::Up, point).await;
    held?;
    released?;
    Ok(true)
}

/// Move the pointer from `from` to `to` (CSS pixels) over `duration_ms`,
/// along the leg [`crate::autoplay::pointer::along`] describes, as a run of
/// `mouseMoved` events with no button held.
///
/// Paced by the clock and not by a count of events: a page that answers
/// slowly gets fewer points of the same path, and the pointer still arrives
/// when it was meant to. The last event is always exactly `to`, so a
/// duration of zero is the plain jump this replaces.
pub async fn travel(
    page: &Page,
    from: (f64, f64),
    to: (f64, f64),
    duration_ms: u32,
    bend: f64,
) -> Result<()> {
    let total = Duration::from_millis(u64::from(duration_ms));
    let started = tokio::time::Instant::now();
    loop {
        let elapsed = started.elapsed();
        if elapsed >= total {
            break;
        }
        let t = elapsed.as_secs_f64() / total.as_secs_f64();
        let (x, y) = crate::autoplay::pointer::along(from, to, bend, t);
        step_to(page, x, y).await?;
        // About a display frame between steps, as a real mouse's events come: never quite even.
        let frame = Duration::from_millis(crate::autoplay::humanize::frame_ms(&mut rand::rng()));
        tokio::time::sleep(frame.min(total.saturating_sub(started.elapsed()))).await;
    }
    step_to(page, to.0, to.1).await
}

/// Read the game canvas's `getBoundingClientRect()` via `Runtime.evaluate`.
///
/// Majsoul renders into the first `<canvas>` element on the page; Tenhou
/// uses the same selector when running in browser mode. We grab the
/// first canvas indiscriminately — multi-canvas pages aren't a thing on
/// these platforms.
pub async fn evaluate_canvas_rect(page: &Page) -> Result<CanvasRect> {
    // IIFE so `Runtime.evaluate` returns a single value, not a Promise.
    // `is_likely_js_function` in chromiumoxide picks the right CDP call
    // based on whether the expression looks like a function — we wrap
    // in `(()=>{...})()` to ensure plain-expression evaluation.
    let expr = "(()=>{const c=document.getElementsByTagName('canvas')[0];\
                if(!c)return null;\
                const r=c.getBoundingClientRect();\
                return {x:r.x,y:r.y,width:r.width,height:r.height};})()";
    let result = page
        .evaluate(expr)
        .await
        .context("CDP evaluate canvas rect")?;
    let value = result
        .value()
        .ok_or_else(|| anyhow!("canvas rect: no value returned"))?;
    if value.is_null() {
        return Err(anyhow!("canvas rect: page has no <canvas> element"));
    }
    let rect: CanvasRect = serde_json::from_value(value.clone())
        .context("canvas rect: deserialise from page value")?;
    Ok(rect)
}

#[cfg(test)]
mod user_left_tests {
    use super::*;

    /// Where the user's mouse left the pointer is taken once: the next move
    /// goes on from there, and the one after from wherever that move ended.
    #[test]
    fn where_the_user_left_the_pointer_is_taken_once() {
        let _ = take_user_left_pointer();
        user_left_pointer_at((10.0, 20.0));
        user_left_pointer_at((30.0, 40.0));
        assert_eq!(
            take_user_left_pointer(),
            Some((30.0, 40.0)),
            "the last place"
        );
        assert_eq!(take_user_left_pointer(), None);
    }

    /// A hand that comes to rest keeps the pointer a while before autoplay
    /// has it back: half a second at the least.
    #[test]
    fn a_hand_that_comes_to_rest_keeps_the_pointer_a_while() {
        set_user_active(true);
        assert!(user_active());
        set_user_active(false);
        assert!(user_active(), "not handed back at once");
        let until = RESTING_UNTIL.lock().unwrap().expect("a wait drawn");
        let left = until.saturating_duration_since(std::time::Instant::now());
        assert!(
            left >= Duration::from_millis(450) && left <= Duration::from_millis(1000),
            "{left:?}"
        );
    }
}
