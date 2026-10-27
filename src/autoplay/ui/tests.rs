use super::*;
use crate::autoplay::cdp_input::{guarded_mouse_sequence, MouseEvent, MouseSink};
use crate::autoplay::context::CanvasRect;
use crate::autoplay::pixels;
use rand::{rngs::StdRng, SeedableRng};

#[derive(Default)]
struct Mouse {
    events: Vec<MouseEvent>,
    fail: Option<MouseEvent>,
}

#[async_trait::async_trait]
impl MouseSink for Mouse {
    async fn send(&mut self, event: MouseEvent, _: (f64, f64)) -> anyhow::Result<()> {
        self.events.push(event);
        if self.fail == Some(event) {
            anyhow::bail!("isolated transport failure");
        }
        Ok(())
    }
}

impl Mouse {
    fn downs(&self) -> usize {
        self.events
            .iter()
            .filter(|&&e| e == MouseEvent::Down)
            .count()
    }
}

struct Harness {
    ui: Machine,
    env: Environment,
    now: Instant,
    rng: StdRng,
    mouse: Mouse,
}

fn frame(sight: Sight) -> Frame {
    Frame {
        stable_geometry: true,
        sight,
        table_visible: false,
        auto_win: Glyph::Absent,
    }
}

fn table() -> Frame {
    Frame {
        table_visible: true,
        auto_win: Glyph::Unlit,
        ..frame(Sight::Nothing)
    }
}

fn fixture(bytes: &[u8]) -> Frame {
    let im = pixels::decode_png(bytes).unwrap();
    let rect = CanvasRect {
        x: 0.,
        y: 0.,
        width: im.width as f64,
        height: im.height as f64,
    };
    Frame {
        stable_geometry: true,
        sight: pixels::end_sight(&im, 1., rect),
        table_visible: pixels::table_view(&im, 1., rect),
        auto_win: pixels::toggle_in(&im, 1., rect, crate::autoplay::majsoul::coords::AUTO_WIN),
    }
}

impl Harness {
    fn new() -> Self {
        let now = Instant::now();
        let env = Environment {
            context: Some(crate::monitor::DecisionContext {
                game: 1,
                round: 1,
                event_seq: 1,
            }),
            settings: Settings {
                enabled: true,
                quick_confirm: true,
                auto_continue: true,
                auto_win: true,
            },
            generation: Generation::default(),
            manual: false,
            page_available: true,
            window: None,
            confirmed: 4,
            lobby: LobbyObservation {
                sequence: 9,
                phase: LobbyPhase::Unknown,
            },
            input: crate::autoplay::verify::InputWatch::default().ticket(),
        };
        let mut ui = Machine::default();
        ui.synchronize(env, now);
        ui.game_started();
        Self {
            ui,
            env,
            now,
            rng: StdRng::seed_from_u64(42),
            mouse: Mouse::default(),
        }
    }
    fn sync(&mut self) {
        self.ui.synchronize(self.env, self.now);
    }
    fn end(&mut self) {
        self.ui.game_ended(self.now);
    }
    fn window(&mut self, phase: TablePhase) -> Window {
        self.now += Duration::from_millis(1);
        let w = Window {
            opened_at: self.now,
            phase,
            expires_at: self.now + Duration::from_secs(30),
        };
        self.env.window = Some(w);
        self.sync();
        w
    }
    fn next(&mut self) -> Option<Effect> {
        self.now = self.ui.due()?;
        self.sync();
        self.ui.tick(self.now)
    }
    fn ready(&mut self, sight: Sight) -> Ticket {
        for _ in 0..100 {
            match self.next().expect("pending screen observation") {
                Effect::Observe(t) => self.ui.observed(t, sight, self.now, &mut self.rng),
                Effect::Press {
                    ticket,
                    sight: expected,
                } => {
                    assert_eq!(sight, expected);
                    return ticket;
                }
            }
        }
        panic!("reaction did not complete")
    }
    fn observe_only(&mut self, sights: &[Sight], n: usize) {
        for sight in sights.iter().cycle().take(n) {
            let Some(effect) = self.next() else {
                return;
            };
            let Effect::Observe(t) = effect else {
                panic!("unknown screen issued input: {effect:?}");
            };
            self.ui.observed(t, *sight, self.now, &mut self.rng);
        }
    }
    async fn click(&mut self, ticket: Ticket, frame: Frame) -> bool {
        let ui = &mut self.ui;
        let env = self.env;
        let now = self.now;
        guarded_mouse_sequence(&mut self.mouse, (20., 30.), 0, 0, false, || async move {
            ui.synchronize(env, now);
            ui.consume(ticket, frame)
        })
        .await
        .unwrap()
    }
    fn done(&mut self, ticket: Ticket, pressed: bool) {
        self.ui
            .screen_done(ticket, pressed, self.now, &mut self.rng);
    }
    async fn screen(&mut self, frame: Frame) {
        let t = self.ready(frame.sight);
        assert!(self.click(t, frame).await);
        self.done(t, true);
    }
}

#[tokio::test]
async fn historical_result_summary_rewards_rematch_and_real_matching_use_one_boundary() {
    let mut h = Harness::new();
    h.ui.result(4, true, h.now);
    h.end();
    for bytes in [
        include_bytes!("../fixtures/ending/hand-result.png").as_slice(),
        include_bytes!("../fixtures/ending/continue.png").as_slice(),
        include_bytes!("../fixtures/ending/reward-1.png").as_slice(),
        include_bytes!("../fixtures/ending/rank.png").as_slice(),
        include_bytes!("../fixtures/ending/reward-2.png").as_slice(),
        include_bytes!("../fixtures/ending/rematch.png").as_slice(),
        include_bytes!("../fixtures/ending/ask.png").as_slice(),
    ] {
        h.screen(fixture(bytes)).await;
    }
    assert_eq!(h.mouse.downs(), 7);
    h.observe_only(
        &[
            fixture(include_bytes!("../fixtures/ending/table.png")).sight,
            Sight::Unread,
        ],
        20,
    );
    assert_eq!(
        h.ui.phase(),
        Phase::Unknown,
        "absence is not matching success"
    );
    h.env.lobby = LobbyObservation {
        sequence: h.env.lobby.sequence + 1,
        phase: LobbyPhase::Matching,
    };
    h.sync();
    assert_eq!(h.ui.phase(), Phase::Matching);
    assert!(h.ui.due().is_none());
    h.ui.game_ended(h.now); // delayed duplicate terminal event
    assert!(h.ui.due().is_none());
    h.ui.game_started();
    h.window(TablePhase::OwnDecision);
    assert_eq!(h.ui.phase(), Phase::Table(TablePhase::OwnDecision));
}

#[tokio::test]
async fn no_blind_blue_press_or_unknown_rematch_target_even_after_the_old_five_seconds() {
    let mut h = Harness::new();
    h.ui.result(4, false, h.now);
    h.observe_only(&[Sight::Unread, Sight::Nothing], 200);
    assert_eq!(h.mouse.downs(), 0);
    assert_eq!(h.ui.phase(), Phase::Table(TablePhase::Waiting));
    h.ui.game_started();
    h.end();
    h.screen(frame(Sight::Continue)).await;
    h.observe_only(&[Sight::Nothing, Sight::Unread, Sight::Button], 1000);
    assert_eq!(h.mouse.downs(), 1);
    assert_eq!(h.ui.phase(), Phase::WaitingStart);
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::EndTimeout));
    h.end();
    assert!(
        h.ui.due().is_none(),
        "late duplicate end cannot restart an expired plan"
    );
}

#[tokio::test]
async fn unknown_overlay_and_unowned_question_cannot_acquire_a_mouse_ticket() {
    let mut h = Harness::new();
    h.end();
    h.observe_only(
        &[
            Sight::Reward,
            Sight::RematchAsk,
            Sight::Button,
            Sight::Nothing,
        ],
        30,
    );
    let t = h.ready(Sight::Rematch);
    assert!(!h.click(t, frame(Sight::Nothing)).await);
    h.done(t, false);
    h.observe_only(&[Sight::RematchAsk], 20);
    assert_eq!(h.mouse.downs(), 0, "failed rematch cannot own a question");
    h.screen(frame(Sight::Rematch)).await;
    h.screen(frame(Sight::RematchAsk)).await;
    assert_eq!(h.mouse.downs(), 2);
}

#[tokio::test]
async fn each_blue_confirm_is_recaptured_and_consumed_once() {
    let mut h = Harness::new();
    h.ui.result(4, false, h.now);
    let t = h.ready(Sight::Button);
    assert!(!h.click(t, frame(Sight::Continue)).await);
    h.done(t, false);
    let t = h.ready(Sight::Button);
    assert!(h.click(t, frame(Sight::Button)).await);
    assert!(!h.click(t, frame(Sight::Button)).await);
    h.done(t, true);
    let due = h.ui.due();
    h.done(t, true); // duplicate completion cannot reschedule or advance
    assert_eq!(h.ui.due(), due);
    h.env.confirmed += 1;
    h.sync();
    assert_eq!(h.ui.phase(), Phase::Table(TablePhase::Waiting));
    assert_eq!(h.mouse.downs(), 1);
}

#[tokio::test]
async fn matching_manual_settings_profile_capture_restore_and_resize_reject_prepared_presses() {
    for change in 0..11 {
        let mut h = Harness::new();
        h.end();
        let t = h.ready(Sight::Continue);
        let mut f = frame(Sight::Continue);
        match change {
            0 => {
                h.env.lobby = LobbyObservation {
                    sequence: h.env.lobby.sequence + 1,
                    phase: LobbyPhase::Matching,
                }
            }
            1 => h.env.manual = true,
            2 => h.env.generation.manual += 1, // manual interval already ended
            3 => h.env.settings.enabled = false,
            4 => h.env.generation.session += 1, // profile/capture/off-on
            5 => h.env.generation.table += 1,   // authentic reconnect/new round
            6 => f.stable_geometry = false,
            7 => f.sight = Sight::Unread,
            8 => h.env.settings.auto_continue = false,
            9 => h.env.page_available = false,
            10 => h.env.generation.settings += 1, // off/on ended during hover
            _ => unreachable!(),
        }
        assert!(!h.click(t, f).await, "change {change}");
        h.done(t, false);
        assert_eq!(h.mouse.downs(), 0);
        assert!(!h.mouse.events.contains(&MouseEvent::Up));
    }
}

#[tokio::test]
async fn manual_resume_reobserves_and_drops_rematch_question_ownership() {
    let mut h = Harness::new();
    h.end();
    h.screen(frame(Sight::Rematch)).await;
    let t = h.ready(Sight::RematchAsk);
    h.env.manual = true;
    h.env.generation.manual += 1;
    assert!(!h.click(t, frame(Sight::RematchAsk)).await);
    assert_eq!(h.ui.phase(), Phase::Suspended(Suspension::Manual));
    h.env.manual = false;
    h.sync();
    h.observe_only(&[Sight::RematchAsk, Sight::Unread], 20);
    h.screen(frame(Sight::Continue)).await;
    assert_eq!(h.mouse.downs(), 2);
}

#[tokio::test]
async fn repeated_control_has_transition_wait_reaction_and_a_finite_click_limit() {
    let mut h = Harness::new();
    h.end();
    let mut last = None;
    for _ in 0..PRESS_LIMIT {
        let t = h.ready(Sight::Continue);
        if let Some(old) = last {
            assert!(h.now >= old + RETRY_WAIT);
        }
        assert!(h.click(t, frame(Sight::Continue)).await);
        last = Some(h.now);
        h.done(t, true);
    }
    assert!(h.next().is_none());
    assert_eq!(h.ui.phase(), Phase::WaitingStart);
    assert_eq!(h.mouse.downs(), PRESS_LIMIT as usize);
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::PressLimit));
}

#[tokio::test]
async fn delayed_observation_and_old_timer_cannot_restore_previous_round() {
    let mut h = Harness::new();
    h.ui.result(4, false, h.now);
    let Effect::Observe(old) = h.next().unwrap() else {
        panic!()
    };
    h.ui.table_started(None);
    h.ui.observed(old, Sight::Button, h.now, &mut h.rng);
    assert!(h.ui.due().is_none());
    assert!(h.ui.tick(h.now + Duration::from_secs(90)).is_none());
    h.end();
    let t = h.ready(Sight::Continue);
    h.ui.game_started();
    assert!(!h.click(t, frame(Sight::Continue)).await);
    h.done(t, true); // fabricated success from a stale callback is ignored
    assert_eq!(h.ui.phase(), Phase::Table(TablePhase::Waiting));
}

#[tokio::test]
async fn semantic_plans_require_their_window_positive_table_pixels_and_single_consumption() {
    for phase in [TablePhase::OwnDecision, TablePhase::Claim] {
        let mut h = Harness::new();
        assert!(h
            .ui
            .begin_decision(
                request(Window {
                    opened_at: h.now,
                    phase,
                    expires_at: h.now + Duration::from_secs(30),
                }),
                2
            )
            .is_none());
        let w = h.window(phase);
        let run = h.ui.begin_decision(request(w), 2).unwrap();
        assert!(
            h.ui.begin_decision(request(w), 2).is_none(),
            "one selected action per window"
        );
        let t = h.ui.decision_click(run, 0).unwrap();
        assert!(
            !h.click(t, frame(Sight::Nothing)).await,
            "unknown frame is not table evidence"
        );
        assert!(!h.click(t, table()).await, "rejected ticket stays spent");
        h.ui.finish_decision(run);
        let w = h.window(phase);
        let run = h.ui.begin_decision(request(w), 2).unwrap();
        let t = h.ui.decision_click(run, 0).unwrap();
        assert!(h.click(t, table()).await);
        assert!(!h.click(t, table()).await);
        assert!(
            h.ui.decision_click(run, 0).is_none(),
            "duplicate step callback"
        );
        let t2 = h.ui.decision_click(run, 1).unwrap(); // next step, same plan
        assert!(h.click(t2, table()).await);
        let late = h.ui.decision_click(run, 2).unwrap();
        h.window(phase); // actual current window replaced during travel
        assert!(!h.click(late, table()).await);
        assert!(!h.ui.retry(run, 0));
        assert_eq!(h.mouse.downs(), 2);
    }
}

#[tokio::test]
async fn verification_only_retries_the_active_run_and_reload_is_an_fsm_effect() {
    let mut h = Harness::new();
    let w = h.window(TablePhase::Claim);
    let run = h.ui.begin_decision(request(w), 2).unwrap();
    for attempt in 0..3 {
        let t = h.ui.decision_click(run, 0).unwrap();
        assert!(h.click(t, table()).await);
        if !h.ui.retry(run, attempt) {
            break;
        }
        assert!(!h.ui.retry(run, attempt), "duplicate timeout callback");
    }
    assert_eq!(h.mouse.downs(), 3);
    assert!(!h.ui.retry(run, 2));
    assert_eq!(h.ui.verified(run, false, 2), None);
    assert_eq!(
        h.ui.verified(run, false, 2),
        None,
        "duplicate proof is inert"
    );
    h.ui.finish_decision(run);
    h.ui.table_started(None); // failure evidence survives round reset
    let w = h.window(TablePhase::OwnDecision);
    let run2 = h.ui.begin_decision(request(w), 0).unwrap();
    assert_eq!(
        h.ui.verified(run2, false, 2),
        Some(DecisionEffect::ReloadClient)
    );
    assert!(!h.ui.decision_live(run2));
    h.ui.game_started(); // failure evidence resets at a new game
    let w = h.window(TablePhase::OwnDecision);
    let run3 = h.ui.begin_decision(request(w), 0).unwrap();
    assert_eq!(h.ui.verified(run3, false, 2), None);
    assert_eq!(
        h.ui.verified(run2, true, 2),
        None,
        "old proof cannot clear current failures"
    );
}

#[tokio::test]
async fn auto_win_has_no_permission_during_decisions_results_or_unknown_frames() {
    let mut h = Harness::new();
    let t = h.ui.auto_win().unwrap();
    assert!(!h.click(t, frame(Sight::Nothing)).await);
    h.ui.toggle_done(t);
    let t = h.ui.auto_win().unwrap();
    assert!(
        !h.click(
            t,
            Frame {
                auto_win: Glyph::Lit,
                ..table()
            }
        )
        .await
    );
    h.ui.toggle_done(t);
    let t = h.ui.auto_win().unwrap();
    assert!(
        h.click(
            t,
            fixture(include_bytes!("../fixtures/ending/table-glyphs.png"))
        )
        .await
    );
    h.ui.toggle_done(t);
    h.window(TablePhase::Claim);
    assert!(h.ui.auto_win().is_none());
    h.env.window = None;
    h.sync();
    h.ui.result(4, false, h.now);
    assert!(h.ui.auto_win().is_none());
    assert_eq!(h.mouse.downs(), 1);
}

#[tokio::test]
async fn an_input_ack_during_retry_travel_cancels_the_pending_mouse_down() {
    let mut h = Harness::new();
    let w = h.window(TablePhase::OwnDecision);
    let run = h.ui.begin_decision(request(w), 2).unwrap();
    let t = h.ui.decision_click(run, 0).unwrap();
    assert!(h.click(t, table()).await);
    assert!(h.ui.retry(run, 0));
    let retry = h.ui.decision_click(run, 0).unwrap();
    let watch = crate::autoplay::verify::InputWatch::default();
    watch.note_sent(crate::autoplay::verify::InputKind::Discard);
    h.env.input = watch.ticket();
    assert!(!h.click(retry, table()).await);
    assert!(!h.ui.retry(run, 1));
    assert_eq!(h.mouse.downs(), 1);
}

#[tokio::test]
async fn terminal_event_order_does_not_create_two_result_plans() {
    for end_first in [true, false] {
        let mut h = Harness::new();
        if end_first {
            h.end();
            h.ui.result(4, true, h.now);
        } else {
            h.ui.result(4, true, h.now);
            h.end();
        }
        h.ui.result(4, false, h.now); // another winner, same result
        h.screen(frame(Sight::Button)).await;
        h.screen(frame(Sight::Continue)).await;
        let due = h.ui.due();
        h.ui.game_ended(h.now); // duplicate end does not reset limits/timer
        h.ui.result(4, true, h.now); // a late result cannot steal summary
        assert_eq!(h.ui.due(), due);
        h.screen(frame(Sight::Rematch)).await;
        assert_eq!(h.mouse.downs(), 3);
    }
}

#[tokio::test]
async fn an_expired_protocol_clock_cannot_click_even_if_the_bridge_slot_has_not_cleared() {
    let mut h = Harness::new();
    let w = h.window(TablePhase::Claim);
    let run = h.ui.begin_decision(request(w), 1).unwrap();
    let t = h.ui.decision_click(run, 0).unwrap();
    h.now = w.expires_at;
    assert!(!h.click(t, table()).await);
    assert_eq!(h.ui.phase(), Phase::Table(TablePhase::Waiting));
    assert!(!h.ui.retry(run, 0));
    assert_eq!(h.mouse.downs(), 0);
}

#[tokio::test]
async fn lobby_cancel_retires_ending_and_matching_without_inventing_a_lobby_click() {
    let mut h = Harness::new();
    h.end();
    let old = h.ready(Sight::Continue);
    h.env.lobby = LobbyObservation {
        sequence: 10,
        phase: LobbyPhase::WaitingStart,
    };
    assert!(!h.click(old, frame(Sight::Continue)).await);
    assert_eq!(h.ui.phase(), Phase::WaitingStart);
    h.env.lobby = LobbyObservation {
        sequence: 11,
        phase: LobbyPhase::Matching,
    };
    h.sync();
    assert_eq!(h.ui.phase(), Phase::Matching);
    h.env.lobby = LobbyObservation {
        sequence: 12,
        phase: LobbyPhase::WaitingStart,
    };
    h.sync();
    assert_eq!(h.ui.phase(), Phase::WaitingStart);
    assert!(h.ui.due().is_none());
    assert_eq!(h.mouse.downs(), 0);
}

#[tokio::test]
async fn profile_or_restore_change_requires_protocol_reentry_and_new_decision_identity() {
    let mut h = Harness::new();
    let w = h.window(TablePhase::OwnDecision);
    let run = h.ui.begin_decision(request(w), 0).unwrap();
    let t = h.ui.decision_click(run, 0).unwrap();
    h.env.settings.enabled = false;
    h.env.generation.session += 1;
    assert!(!h.click(t, table()).await);
    h.env.settings.enabled = true;
    h.sync();
    assert_eq!(h.ui.phase(), Phase::WaitingStart);
    assert!(h.ui.begin_decision(request(w), 0).is_none());
    h.env.generation.table += 1; // syncGame/authGame boundary
    h.sync();
    h.ui.game_started();
    let w2 = h.window(TablePhase::Claim);
    h.ui.table_started(Some(w2));
    let run2 = h.ui.begin_decision(request(w2), 0).unwrap();
    let fresh = h.ui.decision_click(run2, 0).unwrap();
    assert!(!h.click(t, table()).await);
    assert!(h.click(fresh, table()).await);
    h.ui.table_started(Some(w2)); // duplicate start cannot reconsume a window
    assert!(h.ui.begin_decision(request(w2), 0).is_none());
    assert_eq!(h.mouse.downs(), 1);
}

#[tokio::test]
async fn first_subscription_mid_hand_uses_only_a_verified_current_decision_window() {
    let mut h = Harness::new();
    let w = h.window(TablePhase::OwnDecision);
    h.ui = Machine::default(); // subscribed after the real start_game/kyoku
    h.sync();
    assert_eq!(h.ui.phase(), Phase::WaitingStart);
    let run = h.ui.begin_decision(request(w), 0).unwrap();
    let t = h.ui.decision_click(run, 0).unwrap();
    assert!(h.click(t, table()).await);
    assert_eq!(h.ui.phase(), Phase::Table(TablePhase::OwnDecision));
    assert_eq!(h.mouse.downs(), 1);
}

#[tokio::test]
async fn held_transport_failure_still_attempts_release() {
    let mut mouse = Mouse {
        fail: Some(MouseEvent::Move),
        ..Mouse::default()
    };
    // Fail only the jiggle, after the initial move and button-down.
    struct JiggleFailure(Mouse);
    #[async_trait::async_trait]
    impl MouseSink for JiggleFailure {
        async fn send(&mut self, event: MouseEvent, p: (f64, f64)) -> anyhow::Result<()> {
            if self.0.downs() == 0 {
                self.0.events.push(event);
                Ok(())
            } else {
                self.0.send(event, p).await
            }
        }
    }
    let mut sink = JiggleFailure(std::mem::take(&mut mouse));
    assert!(
        guarded_mouse_sequence(&mut sink, (0., 0.), 0, 0, true, || async { true })
            .await
            .is_err()
    );
    assert_eq!(
        sink.0.events,
        [
            MouseEvent::Move,
            MouseEvent::Down,
            MouseEvent::Move,
            MouseEvent::Up
        ]
    );
}

fn request(w: Window) -> crate::bot::control::DecisionRequest {
    use crate::autoplay::{BudgetSource, TimeBudget};
    crate::bot::control::DecisionRequest {
        context: crate::monitor::DecisionContext {
            game: 1,
            round: 1,
            event_seq: 1,
        },
        window: Some(crate::bot::control::InputWindow::Majsoul(TimeBudget {
            opened_at: w.opened_at,
            fixed_ms: w.expires_at.duration_since(w.opened_at).as_millis() as u32,
            add_ms: 0,
            source: if w.phase == TablePhase::Claim {
                BudgetSource::DiscardTile
            } else {
                BudgetSource::DealTile
            },
        })),
    }
}

#[tokio::test]
async fn original_context_and_tracker_fault_are_checked_at_final_mouse_down() {
    for changed in [
        None,
        Some(crate::monitor::DecisionContext {
            game: 2,
            round: 1,
            event_seq: 1,
        }),
        Some(crate::monitor::DecisionContext {
            game: 1,
            round: 2,
            event_seq: 1,
        }),
        Some(crate::monitor::DecisionContext {
            game: 1,
            round: 1,
            event_seq: 2,
        }),
    ] {
        let mut h = Harness::new();
        let w = h.window(TablePhase::OwnDecision);
        let original = request(w);
        let run = h.ui.begin_decision(original, 1).unwrap();
        let ticket = h.ui.decision_click(run, 0).unwrap();
        h.env.context = changed; // same window, visible board and input counters
        assert!(!h.click(ticket, table()).await);
        assert!(!h.ui.retry(run, 0));
        assert!(h.ui.begin_decision(original, 0).is_none());
        assert_eq!(h.mouse.downs(), 0);
    }
}
