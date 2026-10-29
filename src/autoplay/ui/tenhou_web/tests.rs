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
                Effect::PressWeb { ticket, control } => return (ticket, control),
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
        account: None,
        disconnect: None,
        title: None,
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
        account: None,
        disconnect: None,
        title: None,
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
        r#"{"client":"web3","popup":null,"lobby":null}"#,
        r#"{"client":"web4","popup":null,"lobby":null}"#,
        r#"{"client":"web3","popup":{"identity":"x","tag":"AGARI","enabled":true,"closing":false},"lobby":null}"#,
    ] {
        assert!(
            serde_json::from_str::<Web3Observation>(invalid).is_err(),
            "{invalid}"
        );
    }
    let unknown: Web3Observation = serde_json::from_str(r#"{"client":"web3","account":null,"disconnect":null,"title":null,"popup":{"identity":"login","tag":"LOGIN","enabled":true,"closing":false,"queued":0},"lobby":null}"#).unwrap();
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

fn title(identity: &str, account: &str) -> Web3Observation {
    Web3Observation {
        client: ClientTag::Web3,
        account: Some(account.into()),
        disconnect: None,
        title: Some(Title {
            identity: identity.into(),
            enabled: true,
        }),
        popup: None,
        lobby: None,
    }
}

fn authenticated() -> Harness {
    let mut h = Harness::new(Some(target()));
    h.env.context = Some(crate::monitor::DecisionContext {
        game: 1,
        round: 1,
        event_seq: 1,
    });
    h.ui.table_started(None);
    let mut observation = title("unused", "ID1234ABCD");
    observation.title = None;
    h.observe_only(&observation, 1);
    h
}

fn disconnected(identity: &str, account: &str) -> Web3Observation {
    let mut observation = title(identity, account);
    observation.title = None;
    observation.disconnect = Some(DisconnectDialog {
        identity: identity.into(),
        enabled: true,
    });
    observation
}

#[test]
fn disconnect_confirmation_and_title_resume_share_one_deadline_across_reload() {
    let mut h = authenticated();
    h.env.settings.quick_confirm = false;
    h.env.settings.auto_continue = false;
    let dialog = disconnected("closed-dialog", "ID1234ABCD");
    assert!(matches!(
        h.press(&dialog),
        Control::ConfirmDisconnect { .. }
    ));
    let confirmed_at = h.now;
    h.observe_only(&dialog, 4);
    assert_eq!(h.sent.len(), 1);
    h.now += Duration::from_secs(10);
    h.env.generation.session += 1;
    h.sync();
    assert!(matches!(
        h.press(&title("reloaded-title", "ID1234ABCD")),
        Control::Resume { .. }
    ));
    h.observe_only(&title("reloaded-title", "ID1234ABCD"), 3);
    assert_eq!(h.sent.len(), 2);
    h.now = confirmed_at + RESULT_LIMIT + Duration::from_secs(1);
    h.sync();
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::ReconnectTimeout));
    assert!(h.ui.due().is_none());
}

#[test]
fn disconnect_title_restore_returns_to_table_without_a_second_start_timeout() {
    let mut h = authenticated();
    h.press(&disconnected("closed-dialog", "ID1234ABCD"));
    h.env.generation.session += 1;
    h.sync();
    h.press(&title("reloaded-title", "ID1234ABCD"));
    h.ui.web_game_started(Some(0), Some(target()), h.now);
    h.ui.table_started(None);
    h.now += RESULT_LIMIT + Duration::from_secs(1);
    h.sync();
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Table));
    h.press(&popup("real-result", PopupTag::Win, 0));
    assert_eq!(h.sent.len(), 3);
}

#[test]
fn disconnect_requires_known_own_account_and_fresh_manual_ownership() {
    let dialog = disconnected("closed-dialog", "ID1234ABCD");
    let mut fresh = Harness::new(Some(target()));
    fresh.observe_only(&dialog, 5);
    assert!(fresh.sent.is_empty());
    let mut changed = authenticated();
    changed.observe_only(&disconnected("other-account", "ID87654321"), 1);
    assert_eq!(
        changed.ui.waiting_reason(),
        Some(WaitingReason::OwnershipChanged)
    );
    for disabled in [false, true] {
        let mut h = authenticated();
        let (ticket, control) = h.ready(&dialog);
        if disabled {
            h.env.settings.enabled = false;
        } else {
            h.env.manual = true;
        }
        assert!(!h.invoke(ticket, &control, &dialog, Dispatch::Sent));
        assert!(h.sent.is_empty());
    }
    let mut contradictory = authenticated();
    let mut observation = dialog.clone();
    observation.title = title("hidden-underlay", "ID1234ABCD").title;
    contradictory.observe_only(&observation, 5);
    assert!(contradictory.sent.is_empty());
}

#[test]
fn own_title_resume_is_once_per_control_and_real_restore_clears_its_deadline() {
    let mut h = authenticated();
    let observation = title("saved-title", "ID1234ABCD");
    assert!(matches!(h.press(&observation), Control::Resume { .. }));
    h.observe_only(&observation, 5);
    assert_eq!(h.sent.len(), 1);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Reconnecting));
    // The bridge publishes authentic StartGame + RestoreKyoku, not a new deal.
    h.ui.web_game_started(Some(0), Some(target()), h.now);
    h.ui.table_started(None);
    h.now += RESULT_LIMIT + Duration::from_secs(1);
    h.sync();
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Table));
    h.press(&popup("real-result", PopupTag::Win, 0));
}

#[test]
fn title_cannot_log_in_a_fresh_client_changed_account_or_finished_game() {
    let observation = title("title", "ID1234ABCD");
    let mut fresh = Harness::new(Some(target()));
    fresh.observe_only(&observation, 5);
    assert!(fresh.sent.is_empty());
    let mut changed = authenticated();
    changed.observe_only(&title("other-account", "ID87654321"), 1);
    assert_eq!(
        changed.ui.waiting_reason(),
        Some(WaitingReason::OwnershipChanged)
    );
    assert!(changed.sent.is_empty());
    let mut finished = authenticated();
    finished.ui.game_ended(finished.now);
    finished.observe_only(&observation, 5);
    assert!(finished.sent.is_empty());
}

#[test]
fn title_final_permission_obeys_manual_disable_account_and_observation_changes() {
    let observation = title("title", "ID1234ABCD");
    for change in 0..4 {
        let mut h = authenticated();
        let (ticket, control) = h.ready(&observation);
        let mut final_observation = observation.clone();
        match change {
            0 => h.env.manual = true,
            1 => h.env.settings.enabled = false,
            2 => final_observation.account = Some("ID87654321".into()),
            _ => final_observation.title.as_mut().unwrap().identity = "replaced".into(),
        }
        assert!(!h.invoke(ticket, &control, &final_observation, Dispatch::Sent));
        assert!(h.sent.is_empty());
    }
}

#[test]
fn same_game_reload_retains_only_resume_identity_and_unavailable_reads_are_bounded() {
    let mut h = authenticated();
    let (old_ticket, old_control) = h.ready(&title("old-document", "ID1234ABCD"));
    h.env.generation.session += 1;
    h.sync();
    assert!(!h.invoke(
        old_ticket,
        &old_control,
        &title("old-document", "ID1234ABCD"),
        Dispatch::Sent
    ));
    let Effect::ObserveWeb(ticket) = h.next().unwrap() else {
        panic!()
    };
    h.ui.web_observed(ticket, None, h.now, &mut h.rng);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::Reconnecting));
    h.press(&title("new-document", "ID1234ABCD"));
    h.now += RESULT_LIMIT + Duration::from_secs(1);
    h.sync();
    assert_eq!(h.ui.waiting_reason(), Some(WaitingReason::ReconnectTimeout));
    assert!(h.ui.due().is_none());
    let mut different_game = authenticated();
    different_game.env.generation.session += 1;
    different_game.env.context.as_mut().unwrap().game += 1;
    different_game.observe_only(&title("new-game", "ID1234ABCD"), 5);
    assert!(different_game.sent.is_empty());
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
    assert_eq!(h.ui.phase(), Phase::Web(Stage::ReservationConfirming));
    h.observe_only(&observation, 20); // local reservation flag may still be false
    let mut requested = observation.clone();
    requested.lobby.as_mut().unwrap().reservation_requested = true;
    h.observe_only(&requested, 5);
    assert_eq!(h.ui.phase(), Phase::Web(Stage::ReservationPending));
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
fn real_rank_certificate_after_final_does_not_block_or_duplicate_the_rematch() {
    let mut h = Harness::new(Some(target()));
    h.final_sent();
    let notice = popup("rank-nine", PopupTag::RankNotice, 0);
    assert!(matches!(
        h.press(&notice),
        Control::Popup {
            tag: PopupTag::RankNotice,
            ..
        }
    ));
    assert_eq!(h.ui.phase(), Phase::Web(Stage::WaitingLobby));
    h.observe_only(&notice, 8);
    assert_eq!(h.sent.len(), 2);
    assert!(matches!(
        h.press(&lobby()),
        Control::Reserve { mode: 169, .. }
    ));
    assert_eq!(h.sent.len(), 3);
}

#[test]
fn a_mouse_reservation_without_a_client_request_times_out_without_repeating() {
    let mut h = Harness::new(Some(target()));
    h.final_sent();
    h.press(&lobby());
    assert_eq!(h.ui.phase(), Phase::Web(Stage::ReservationConfirming));
    h.now += END_LIMIT + Duration::from_millis(1);
    h.sync();
    assert_eq!(
        h.ui.phase(),
        Phase::Web(Stage::Stopped(WaitingReason::EndTimeout))
    );
    assert_eq!(h.sent.len(), 2);
    h.observe_only(&lobby(), 5);
    assert_eq!(h.sent.len(), 2);
}

#[test]
fn rank_notice_requires_completed_match_target_and_continue_permission() {
    for change in 0..5 {
        let mut h = Harness::new(if change == 3 { None } else { Some(target()) });
        if change >= 2 {
            h.final_sent();
        }
        if change == 1 {
            h.ui.table_started(None);
        }
        if change == 2 {
            h.env.settings.auto_continue = false;
        }
        if change == 4 {
            h.ui.web_game_started(None, Some(target()), h.now);
        }
        let before = h.sent.len();
        h.observe_only(&popup("rank-notice", PopupTag::RankNotice, 0), 8);
        assert_eq!(h.sent.len(), before, "change {change}");
    }
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
