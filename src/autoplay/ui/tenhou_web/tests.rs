use super::*;
use crate::autoplay::ui::{Generation, Settings};
use crate::autoplay::verify::{InputWatch, LobbyObservation};
use rand::{rngs::StdRng, SeedableRng};

struct Harness {
    ui: Machine,
    env: Environment,
    now: Instant,
    rng: StdRng,
    sent: Vec<Control>,
}

impl Harness {
    fn new(target: Option<TableSpec>) -> Self {
        let now = Instant::now();
        let env = Environment {
            client: Client::TenhouWeb,
            context: None,
            settings: Settings {
                enabled: true,
                quick_confirm: true,
                auto_continue: true,
                auto_win: false,
            },
            generation: Generation::default(),
            manual: false,
            page_available: true,
            window: None,
            confirmed: 0,
            lobby: LobbyObservation::default(),
            input: InputWatch::default().ticket(),
        };
        let mut ui = Machine::default();
        ui.synchronize(env, now);
        ui.web_game_started(Some(0), target, now);
        Self {
            ui,
            env,
            now,
            rng: StdRng::seed_from_u64(71),
            sent: Vec::new(),
        }
    }

    fn sync(&mut self) {
        self.ui.synchronize(self.env, self.now);
    }

    fn next(&mut self) -> Option<Effect> {
        self.sync(); // the manager synchronizes before deriving its next timer
        self.now = self.ui.due()?;
        self.sync();
        self.ui.tick(self.now)
    }

    fn ready(&mut self, observation: &Web3Observation) -> (Ticket, Control) {
        for _ in 0..100 {
            match self.next().expect("pending Web observation") {
                Effect::ObserveWeb(ticket) => {
                    self.ui
                        .web_observed(ticket, Some(observation.clone()), self.now, &mut self.rng)
                }
                Effect::InvokeWeb { ticket, control } => return (ticket, control),
                other => panic!("Web route emitted a pixel effect: {other:?}"),
            }
        }
        panic!("reaction did not complete")
    }

    fn observe_only(&mut self, observation: &Web3Observation, n: usize) {
        for _ in 0..n {
            let Some(effect) = self.next() else { return };
            let Effect::ObserveWeb(ticket) = effect else {
                panic!("unexpected input: {effect:?}")
            };
            self.ui
                .web_observed(ticket, Some(observation.clone()), self.now, &mut self.rng);
        }
    }

    fn invoke(
        &mut self,
        ticket: Ticket,
        control: &Control,
        observation: &Web3Observation,
        result: Dispatch,
    ) -> bool {
        self.sync();
        let allowed = self.ui.consume_web(ticket, control, Some(observation));
        if allowed && result == Dispatch::Sent {
            self.sent.push(control.clone());
        }
        self.ui.web_done(
            ticket,
            if allowed { result } else { Dispatch::Changed },
            self.now,
        );
        allowed
    }

    fn press(&mut self, observation: &Web3Observation) -> Control {
        let (ticket, control) = self.ready(observation);
        assert!(self.invoke(ticket, &control, observation, Dispatch::Sent));
        control
    }

    fn final_sent(&mut self) {
        self.ui.table_started(None);
        self.press(&popup("final", PopupTag::Final, 0));
        assert_eq!(self.ui.phase(), Phase::Web(Stage::WaitingLobby));
    }
}

use crate::autoplay::ui::Phase;

fn target() -> TableSpec {
    TableSpec {
        lobby: 0,
        mode: 169,
    }
}

fn popup(identity: &str, tag: PopupTag, queued: u32) -> Web3Observation {
    Web3Observation {
        client: ClientTag::Web3,
        popup: Some(Popup {
            identity: identity.into(),
            tag,
            queued,
            enabled: true,
            closing: false,
        }),
        lobby: None,
    }
}

fn lobby() -> Web3Observation {
    Web3Observation {
        client: ClientTag::Web3,
        popup: None,
        lobby: Some(Lobby {
            identity: "lobby-instance".into(),
            lobby: 0,
            reservation_requested: false,
            offers: vec![
                Offer {
                    identity: "wrong-first".into(),
                    mode: 9,
                    enabled: true,
                },
                Offer {
                    identity: "exact-target".into(),
                    mode: 169,
                    enabled: true,
                },
            ],
        }),
    }
}

#[test]
fn strict_dto_rejects_missing_facts_wrong_client_and_accepts_explicit_unknown_popup() {
    for invalid in [
        r#"{"client":"web3"}"#,
        r#"{"client":"web4","popup":null,"lobby":null}"#,
        r#"{"client":"web3","popup":{"identity":"x","tag":"AGARI","enabled":true,"closing":false},"lobby":null}"#,
    ] {
        assert!(
            serde_json::from_str::<Web3Observation>(invalid).is_err(),
            "{invalid}"
        );
    }
    let unknown: Web3Observation = serde_json::from_str(r#"{"client":"web3","popup":{"identity":"login","tag":"LOGIN","enabled":true,"closing":false,"queued":0},"lobby":null}"#).unwrap();
    assert_eq!(unknown.popup.as_ref().unwrap().tag, PopupTag::Other);
    let mut h = Harness::new(Some(target()));
    h.observe_only(&unknown, 10);
    assert!(h.sent.is_empty());
    assert_eq!(
        serde_json::from_str::<Dispatch>(r#""sent""#).unwrap(),
        Dispatch::Sent
    );
    assert!(serde_json::from_str::<Dispatch>(r#""ok""#).is_err());
}

#[test]
fn real_popup_results_advance_without_waiting_for_buffered_hora_and_each_instance_sends_once() {
    let mut h = Harness::new(Some(target()));
    h.ui.table_started(None);
    let first = popup("ron-one", PopupTag::Win, 1);
    h.press(&first);
    h.observe_only(&first, 8);
    assert_eq!(h.sent.len(), 1);
    h.press(&popup("ron-two", PopupTag::Win, 0));
    h.ui.result(0, true, h.now); // delayed real multi-ron MJAI is not another UI plan
    h.ui.game_ended(h.now);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::RoundResult));
    h.ui.table_started(None); // authentic INIT finally enters the next hand
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Table));
    h.observe_only(&first, 4); // late old popup cannot be dispatched again
    assert_eq!(h.sent.len(), 2);
}

#[test]
fn start_confirmation_does_not_manufacture_a_dealt_hand() {
    let mut h = Harness::new(Some(target()));
    let start = popup("start", PopupTag::Start, 0);
    h.press(&start);
    h.observe_only(&start, 5);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Starting));
    assert_eq!(h.sent.len(), 1);
    h.ui.table_started(None);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Table));
}

#[test]
fn final_returns_to_lobby_then_only_the_exact_real_table_can_be_reserved() {
    let mut h = Harness::new(Some(target()));
    h.final_sent();
    let observation = lobby();
    let control = h.press(&observation);
    assert!(
        matches!(control, Control::Reserve { mode: 169, lobby: 0, ref identity, .. } if identity == "exact-target")
    );
    assert_eq!(control.dispatch_key(), ("join", "exact-target"));
    assert_eq!(h.ui.phase(), Phase::Web(Stage::ReservationPending));
    h.observe_only(&observation, 20); // local reservation flag may still be false
    let mut requested = observation.clone();
    requested.lobby.as_mut().unwrap().reservation_requested = true;
    h.observe_only(&requested, 5);
    h.observe_only(&observation, 5); // cancellation or rejection never silently requeues
    assert_eq!(h.sent.len(), 2);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::ReservationPending));
    h.ui.web_game_started(Some(0), Some(target()), h.now);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Starting));
    h.press(&popup("next-start", PopupTag::Start, 0));
    h.ui.table_started(None);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Table));
}

#[test]
fn absent_target_or_changed_ambiguous_and_disabled_offers_never_choose_another_mode() {
    for change in 0..5 {
        let mut h = Harness::new(if change == 0 { None } else { Some(target()) });
        h.final_sent();
        let mut observation = lobby();
        let lobby = observation.lobby.as_mut().unwrap();
        match change {
            0 => {}
            1 => lobby.lobby = 123,
            2 => {
                lobby.offers.pop();
            }
            3 => lobby.offers.push(lobby.offers[1].clone()),
            4 => lobby.offers[1].enabled = false,
            _ => unreachable!(),
        }
        h.observe_only(&observation, 20);
        assert_eq!(h.sent.len(), 1);
        assert_eq!(
            h.ui.waiting_reason(),
            Some(if change == 0 {
                WaitingReason::NoKnownTable
            } else {
                WaitingReason::TargetUnavailable
            })
        );
    }
}

#[test]
fn popup_and_ownership_changes_withhold_and_spend_the_old_permission() {
    for change in 0..13 {
        let mut h = Harness::new(Some(target()));
        h.ui.table_started(None);
        let original = popup("result", PopupTag::Draw, 0);
        let (ticket, control) = h.ready(&original);
        let mut current = original.clone();
        match change {
            0 => current.popup.as_mut().unwrap().identity = "replacement".into(),
            1 => current.popup.as_mut().unwrap().tag = PopupTag::Final,
            2 => current.popup.as_mut().unwrap().queued += 1,
            3 => current.popup.as_mut().unwrap().enabled = false,
            4 => current.popup.as_mut().unwrap().closing = true,
            5 => h.env.settings.quick_confirm = false,
            6 => h.env.generation.settings += 1,
            7 => h.env.manual = true,
            8 => h.env.generation.manual += 1,
            9 => h.env.generation.session += 1,
            10 => h.env.generation.table += 1,
            11 => h.env.page_available = false,
            12 => h.env.client = Client::Majsoul,
            _ => unreachable!(),
        }
        assert!(
            !h.invoke(ticket, &control, &current, Dispatch::Sent),
            "change {change}"
        );
        assert!(!h.ui.consume_web(ticket, &control, Some(&original)));
        assert!(h.sent.is_empty());
    }
}

#[test]
fn the_lobby_render_and_selected_offer_are_rechecked_at_the_final_boundary() {
    for change in 0..5 {
        let mut h = Harness::new(Some(target()));
        h.final_sent();
        let original = lobby();
        let (ticket, control) = h.ready(&original);
        let mut current = original.clone();
        let lobby = current.lobby.as_mut().unwrap();
        match change {
            0 => lobby.identity = "new-lobby".into(),
            1 => lobby.offers[1].mode = 41,
            2 => lobby.offers[1].identity = "new-offer".into(),
            3 => lobby.reservation_requested = true,
            4 => lobby.offers[1].enabled = false,
            _ => unreachable!(),
        }
        assert!(!h.invoke(ticket, &control, &current, Dispatch::Sent));
        assert_eq!(h.sent.len(), 1);
    }
}

#[test]
fn changed_before_invoke_requires_new_observation_and_old_callbacks_cannot_complete_it() {
    let mut h = Harness::new(Some(target()));
    h.ui.table_started(None);
    let observation = popup("same-instance", PopupTag::Win, 0);
    let (old, control) = h.ready(&observation);
    assert!(h.invoke(old, &control, &observation, Dispatch::Changed));
    let (fresh, fresh_control) = h.ready(&observation);
    h.ui.web_done(old, Dispatch::Sent, h.now);
    assert!(h.invoke(fresh, &fresh_control, &observation, Dispatch::Sent));
    assert!(!h.ui.consume_web(fresh, &fresh_control, Some(&observation)));
    h.observe_only(&observation, 5);
    assert_eq!(h.sent.len(), 1);
}

#[test]
fn fabricated_success_and_a_different_control_cannot_consume_or_advance_a_web_effect() {
    let mut h = Harness::new(Some(target()));
    let observation = popup("final", PopupTag::Final, 0);
    let (ticket, control) = h.ready(&observation);
    h.ui.web_done(ticket, Dispatch::Sent, h.now); // no consumed permission
    assert_eq!(h.ui.phase(), Phase::Web(Stage::FinalResult));
    let wrong = Control::Popup {
        identity: "another".into(),
        tag: PopupTag::Final,
        queued: 0,
    };
    assert!(!h.ui.consume_web(ticket, &wrong, Some(&observation)));
    assert!(!h.ui.consume_web(ticket, &control, Some(&observation)));
    h.ui.web_done(ticket, Dispatch::Sent, h.now);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::FinalResult));
    h.ui.web_done(ticket, Dispatch::Changed, h.now);
    h.press(&observation);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::WaitingLobby));
    assert_eq!(h.sent.len(), 1);
}

#[test]
fn unconfirmed_transport_failure_stops_without_repeating_the_control() {
    let mut h = Harness::new(Some(target()));
    let observation = popup("final", PopupTag::Final, 0);
    let (ticket, control) = h.ready(&observation);
    assert!(h.invoke(ticket, &control, &observation, Dispatch::Unavailable));
    assert_eq!(
        h.ui.waiting_reason(),
        Some(WaitingReason::DispatchUnavailable)
    );
    h.ui.web_done(ticket, Dispatch::Sent, h.now);
    h.observe_only(&observation, 20);
    assert!(h.sent.is_empty());
    assert!(h.ui.due().is_none());
}

#[test]
fn observer_mode_and_a_first_lobby_without_a_real_game_have_no_input_authority() {
    let mut h = Harness::new(None);
    h.ui.web_game_started(None, Some(target()), h.now);
    h.observe_only(&popup("spectator-result", PopupTag::Win, 0), 10);
    h.observe_only(&popup("spectator-final", PopupTag::Final, 0), 10);
    assert!(h.sent.is_empty());
    let mut fresh = Machine::default();
    fresh.synchronize(h.env, h.now);
    h.ui = fresh;
    h.observe_only(&lobby(), 10);
    assert!(h.sent.is_empty());
}

#[test]
fn lobby_return_without_own_final_dispatch_cannot_authorize_a_reservation() {
    let mut h = Harness::new(Some(target()));
    let observation = popup("final", PopupTag::Final, 0);
    let (ticket, control) = h.ready(&observation);
    assert!(h.invoke(ticket, &control, &observation, Dispatch::Changed));
    h.observe_only(&lobby(), 5); // user or another native path returned to the lobby
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::Lobby));
    assert!(h.sent.is_empty());
}

#[test]
fn unavailable_or_contradictory_observation_never_picks_a_more_convenient_control() {
    let mut h = Harness::new(Some(target()));
    let Effect::ObserveWeb(ticket) = h.next().unwrap() else {
        panic!()
    };
    h.ui.web_observed(ticket, None, h.now, &mut h.rng);
    assert_eq!(
        h.ui.waiting_reason(),
        Some(WaitingReason::ObservationUnavailable)
    );
    assert!(h.ui.due().is_none());
    let mut h = Harness::new(Some(target()));
    let mut contradictory = popup("start", PopupTag::Start, 0);
    contradictory.lobby = lobby().lobby;
    h.observe_only(&contradictory, 20);
    assert!(h.sent.is_empty());
}

#[test]
fn endings_cannot_survive_ownership_changes_and_a_new_game_rebuilds_the_target() {
    let mut h = Harness::new(Some(target()));
    h.final_sent();
    h.env.generation.manual += 1;
    h.sync();
    h.observe_only(&lobby(), 10);
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::OwnershipChanged));
    assert_eq!(h.sent.len(), 1);
    h.ui.web_game_started(Some(0), Some(target()), h.now);
    h.press(&popup("new-start", PopupTag::Start, 0));
    h.ui.table_started(None);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Table));
}

#[test]
fn deadlines_and_changed_dispatch_limits_are_finite_without_a_requeue_loop() {
    let mut h = Harness::new(Some(target()));
    let observation = popup("result", PopupTag::Win, 0);
    let (ticket, control) = h.ready(&observation);
    h.now += RESULT_LIMIT;
    assert!(!h.invoke(ticket, &control, &observation, Dispatch::Sent));
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::EndTimeout));
    let mut h = Harness::new(Some(target()));
    let observation = popup("result", PopupTag::Win, 0);
    for _ in 0..PRESS_LIMIT {
        let (ticket, control) = h.ready(&observation);
        assert!(h.invoke(ticket, &control, &observation, Dispatch::Changed));
    }
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::PressLimit));
    assert!(h.ui.due().is_none());
    assert!(h.sent.is_empty());
}
