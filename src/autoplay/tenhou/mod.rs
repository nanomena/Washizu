//! Tenhou Web3 autoplay drives verified native handlers with a single-use
//! permit tied to the real bridge decision, full hand and native input window.
use crate::autoplay::delay::{BudgetSnapshot, DecisionKind, DelayInput};
use crate::autoplay::platform::{ActionContext, PlanResult, PlatformAutoplay, Step};
use crate::schema::MjaiEvent;
use tracing::warn;

pub(crate) mod client;
pub(crate) mod input;
pub mod web3_rewrite;

const LOG: &str = "akagi::autoplay::tenhou";
const READY_TIMEOUT_MS: u32 = 8_000;
#[derive(Default)]
pub struct TenhouAutoplay;
impl TenhouAutoplay {
    pub fn new() -> Self {
        Self
    }
}

/// Map the bot's action onto the delay model's decision taxonomy. `None` for
/// events that are not ours to perform.
fn decision_kind(action: &MjaiEvent) -> Option<DecisionKind> {
    Some(match action {
        MjaiEvent::Dahai { .. } => DecisionKind::Dahai,
        MjaiEvent::Reach { .. } => DecisionKind::Reach,
        MjaiEvent::Chi { .. } => DecisionKind::Chi,
        MjaiEvent::Pon { .. } => DecisionKind::Pon,
        MjaiEvent::Daiminkan { .. } => DecisionKind::Daiminkan,
        MjaiEvent::Ankan { .. } => DecisionKind::Ankan,
        MjaiEvent::Kakan { .. } => DecisionKind::Kakan,
        MjaiEvent::Hora { .. } => DecisionKind::Hora,
        MjaiEvent::Ryukyoku { .. } => DecisionKind::Ryukyoku,
        MjaiEvent::Kita { .. } => DecisionKind::Kita,
        MjaiEvent::None => DecisionKind::Pass,
        _ => return None,
    })
}

/// Push the pre-send "thinking" pause.
///
/// The model's target is a *total* for the window, and here the window starts
/// when the client does — `Step::AwaitReady` has already waited out the
/// animation — so the target is the sleep with nothing to deduct.
fn push_pre_delay(steps: &mut Vec<Step>, ctx: &ActionContext, kind: DecisionKind) {
    let (is_tsumogiri, tile_class) = match ctx.action {
        MjaiEvent::Dahai { tsumogiri, pai, .. } => {
            (*tsumogiri, crate::autoplay::delay::TileClass::of_mjai(pai))
        }
        _ => (false, None),
    };
    // The only discard with no just-drawn tile is the one a call bought us.
    let is_post_call =
        matches!(ctx.action, MjaiEvent::Dahai { .. }) && ctx.tenhou.is_some_and(|t| !t.is_tsumo);
    let opponent_riichi = ctx
        .snapshot
        .players
        .iter()
        .any(|p| p.seat != ctx.our_seat && p.riichi_declared);

    let legacy = ctx.delay_cfg.mode == crate::config::DelayMode::Legacy;
    let mut delay_cfg = ctx.delay_cfg.clone();
    if legacy {
        delay_cfg.distribution = crate::config::DelayDistribution::Uniform;
    }

    let input = DelayInput {
        kind,
        is_tsumogiri,
        is_post_call,
        first_action_of_kyoku: ctx.last_kawa_tile.is_none(),
        // Nothing is being pressed, so no animation has to finish first.
        opening_animation: false,
        can_riichi: ctx
            .legal_actions
            .iter()
            .any(|a| a.action_type == riichienv_core::action::ActionType::Riichi),
        in_riichi: ctx.self_riichi_accepted,
        opponent_riichi,
        tile_class,
        junme: ctx
            .snapshot
            .players
            .get(ctx.our_seat as usize)
            .map(|p| p.river.len() as u32 + 1)
            .unwrap_or(0),
        legal_action_count: ctx.legal_actions.len(),
        probs: ctx.probs,
        reading: ctx.reading,
        // Tenhou never states its allowance on the wire, but it very much
        // has one: overrun it and the client auto-discards, which is how the
        // first live run lost every turn. Feed the lobby's standard clock in
        // so the model's budget caps apply as they do on Majsoul.
        // Elapsed is zero on purpose: the plan does not run until the
        // client's clock is actually up (`Step::AwaitReady`), and that clock
        // starts then, not when the frame reached us. Deducting the time
        // spent animating would spend a budget that had not started.
        budget: ctx
            .tenhou
            .and_then(|t| t.window)
            .and_then(|w| w.clock)
            .map(|clock| BudgetSnapshot {
                fixed_ms: clock.fixed_ms,
                // The wire supplies the initial bank cap, not its current balance.
                // Do not spend an unobserved remaining balance on artificial delay.
                add_ms: 0,
                elapsed_ms: 0,
            }),
        click_overhead_ms: 0,
        cfg: ctx.cfg,
        delay_cfg: &delay_cfg,
    };

    let decision = ctx
        .delay_script
        .filter(|_| !legacy)
        .and_then(|s| s.try_decide(&input))
        .unwrap_or_else(|| crate::autoplay::delay::decide(&input, &mut rand::rng()));
    // The whole target is the sleep. It is measured from the moment the
    // client became ready, which `Step::AwaitReady` has already waited for —
    // nothing of the turn's own clock has been spent before that point.
    let sleep = decision.total_target_ms;
    if sleep > 0 {
        steps.push(Step::Sleep { duration_ms: sleep });
    }
}

impl PlatformAutoplay for TenhouAutoplay {
    fn plan(&self, ctx: &ActionContext) -> PlanResult {
        let mut result = PlanResult::default();
        let Some(state) = ctx.tenhou else {
            return result;
        };
        let Some(window) = state.window else {
            return result;
        };
        if ctx.num_players != 4 || window.clock.is_none() {
            return result;
        }
        let Some(kind) = decision_kind(ctx.action) else {
            return result;
        };
        if kind == DecisionKind::Dahai && ctx.self_riichi_accepted {
            return result;
        }
        if kind == DecisionKind::Pass && !window.has_declinable_claim() {
            return result;
        }
        let resolve = |pai: &str, tsumogiri: bool| {
            let index =
                crate::bridge::tenhou::encode::dahai_index(state.hand_view(), pai, tsumogiri)?;
            // An exact discard, including its red/plain meaning and drawn copy.
            if crate::bridge::tenhou::tile::tenhou_to_mjai_one(index) != pai {
                return None;
            }
            if tsumogiri && (!state.is_tsumo || state.hand.last() != Some(&index)) {
                return None;
            }
            Some(index)
        };
        let reach_tile = match ctx.action {
            MjaiEvent::Reach {
                pai: Some(pai),
                tsumogiri: Some(tsumogiri),
                ..
            } => {
                let Some(tile) = resolve(pai, *tsumogiri) else {
                    warn!(target: LOG, "Tenhou riichi discard does not match the full tracked hand");
                    return result;
                };
                Some(tile)
            }
            MjaiEvent::Reach { .. } => {
                warn!(target: LOG, "Tenhou riichi needs an exact tile and tsumogiri flag");
                return result;
            }
            _ => None,
        };
        let step = if let MjaiEvent::Dahai { pai, tsumogiri, .. } = ctx.action {
            let Some(tile_index) = resolve(pai, *tsumogiri) else {
                return result;
            };
            Step::Discard { tile_index }
        } else {
            let Some(payload) = client::choice_payload(ctx.action, state) else {
                return result;
            };
            Step::ClientChoice {
                payload,
                label: format!("{kind:?}"),
            }
        };
        result.steps.push(Step::AwaitReady {
            timeout_ms: READY_TIMEOUT_MS,
        });
        push_pre_delay(&mut result.steps, ctx, kind);
        result.steps.push(step);
        if let Some(tile_index) = reach_tile {
            result.steps.push(Step::Sleep {
                duration_ms: ctx.cfg.inter_click_delay_ms,
            });
            // Web3 raises a new legal native input window after the real own
            // REACH echo. The original permit admits only that transition.
            result.steps.push(Step::AwaitReady {
                timeout_ms: READY_TIMEOUT_MS,
            });
            result.steps.push(Step::Discard { tile_index });
        }
        result
    }
}

#[cfg(test)]
mod tests;
