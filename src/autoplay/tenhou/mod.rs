//! Tenhou adapts selected semantic actions to observed Web3 mouse targets.
//! Pacing, readiness polling, retries and input belong to the shared turn runtime.
use crate::autoplay::context::AutoplayContext;
use crate::autoplay::platform::{
    window_identity, ActionContext, Confirmation, DeclarationFacts, InputTarget, PressIntent,
    PressRole, SiteAdapter, SiteObservation, TargetIdentity, TargetKind, TurnClockFacts, TurnPlan,
};
use crate::autoplay::pointer::{PointerLayout, TileTarget};
use crate::autoplay::tenhou_state::TenhouState;
use crate::schema::MjaiEvent;
use anyhow::{anyhow, Result};
use chromiumoxide::page::Page;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) mod client;
pub mod web3_rewrite;

pub struct TenhouAutoplay {
    context: Arc<AutoplayContext>,
    clock: Mutex<Option<(String, TurnClockFacts)>>,
}
impl TenhouAutoplay {
    pub fn new(context: Arc<AutoplayContext>) -> Self {
        Self {
            context,
            clock: Mutex::new(None),
        }
    }
    fn state(&self) -> Option<TenhouState> {
        self.context.tenhou_state.read().ok()?.clone()
    }
    fn clock(
        &self,
        key: &str,
        raw: &client::ClockFacts,
        observed_at: Instant,
    ) -> Option<TurnClockFacts> {
        let mut clock = self.clock.lock().ok()?;
        if let Some((old, facts)) = &*clock {
            if old == key {
                return Some(*facts);
            }
        }
        let facts = TurnClockFacts {
            started_at: raw.started_at(observed_at)?,
            fixed_ms: raw.fixed_ms,
            bank_at_start_ms: Some(raw.granted_bank_ms),
        };
        *clock = Some((key.to_owned(), facts));
        Some(facts)
    }
    fn facts(
        &self,
        state: &TenhouState,
        input: &client::InputSnapshot,
        action: &MjaiEvent,
        observed_at: Instant,
    ) -> SiteObservation {
        let mut observation = SiteObservation {
            ready: false,
            target_kind: TargetKind::Commit,
            target: None,
            layout: layout(input),
            clock: None,
            continuation: None,
        };
        if !input.ready || !same_hand(input, state) {
            return observation;
        }
        let (Some(protocol_window), Some(native_window)) = (state.window, input.window.as_ref())
        else {
            return observation;
        };
        let proof =
            window_identity(crate::bot::control::InputWindow::Tenhou(protocol_window).proof());
        let key = format!("{proof}:{native_window}");
        observation.clock = input
            .clock
            .as_ref()
            .and_then(|raw| self.clock(&key, raw, observed_at));
        if matches!(action, MjaiEvent::Reach { .. })
            && state.in_riichi
            && state.is_tsumo
            && protocol_window.ops == 0
        {
            observation.continuation = Some(DeclarationFacts {
                window: proof,
                hand: input.hand.iter().map(u32::to_string).collect(),
            });
        }
        observation
    }
}

fn discard_tile(action: &MjaiEvent, state: &TenhouState) -> Option<(u32, bool)> {
    let (pai, tsumogiri) = match action {
        MjaiEvent::Dahai { pai, tsumogiri, .. } => (pai, *tsumogiri),
        MjaiEvent::Reach {
            pai: Some(pai),
            tsumogiri: Some(tsumogiri),
            ..
        } => (pai, *tsumogiri),
        _ => return None,
    };
    let index = crate::bridge::tenhou::encode::dahai_index(state.hand_view(), pai, tsumogiri)?;
    if crate::bridge::tenhou::tile::tenhou_to_mjai_one(index) != *pai
        || (tsumogiri && (!state.is_tsumo || state.hand.last() != Some(&index)))
    {
        return None;
    }
    Some((index, tsumogiri))
}

fn semantic_plan(ctx: &ActionContext<'_>, state: &TenhouState) -> TurnPlan {
    let mut plan = TurnPlan::default();
    let Some(window) = state.window else {
        return plan;
    };
    if ctx.num_players != 4 || window.clock.is_none() {
        return plan;
    }
    let actor = match ctx.action {
        MjaiEvent::Dahai { actor, .. }
        | MjaiEvent::Reach { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Pon { actor, .. }
        | MjaiEvent::Daiminkan { actor, .. }
        | MjaiEvent::Ankan { actor, .. }
        | MjaiEvent::Kakan { actor, .. }
        | MjaiEvent::Hora { actor, .. }
        | MjaiEvent::Kita { actor, .. } => Some(*actor),
        MjaiEvent::None | MjaiEvent::Ryukyoku { .. } => None,
        _ => return plan,
    };
    if actor.is_some_and(|actor| actor != ctx.our_seat) {
        return plan;
    }
    let mut press = |role, confirmation| {
        plan.presses.push(PressIntent {
            role,
            confirmation,
            action: ctx.action.clone(),
        })
    };
    match ctx.action {
        MjaiEvent::Dahai { .. } => {
            if ctx.self_riichi_accepted || discard_tile(ctx.action, state).is_none() {
                return plan;
            }
            press(PressRole::Discard, Confirmation::ClientCommand);
        }
        MjaiEvent::Reach { .. } => {
            if discard_tile(ctx.action, state).is_none()
                || client::choice_payload(ctx.action, state).is_none()
            {
                return plan;
            }
            press(PressRole::Declaration, Confirmation::DeclarationEcho);
            press(PressRole::Discard, Confirmation::ClientCommand);
        }
        MjaiEvent::None if !window.has_declinable_claim() => {}
        _ => {
            if client::choice_payload(ctx.action, state).is_some() {
                press(PressRole::Action, Confirmation::ClientCommand);
            }
        }
    }
    plan
}

fn layout(input: &client::InputSnapshot) -> Option<PointerLayout> {
    let layout = PointerLayout {
        viewport: input.viewport,
        tiles: input
            .tiles
            .iter()
            .map(|tile| TileTarget {
                tile: crate::bridge::tenhou::tile::tenhou_to_mjai_one(tile.tile),
                drawn: tile.drawn,
                bounds: tile.bounds,
            })
            .collect(),
        next_draw: input.next_draw,
        rest: input.rest?,
    };
    layout.valid().then_some(layout)
}

/// A full physical hand match is required even for buttons and candidates.
fn same_hand(input: &client::InputSnapshot, state: &TenhouState) -> bool {
    let mut expected = state.hand.clone();
    expected.sort_unstable();
    let mut actual = input.hand.clone();
    actual.sort_unstable();
    actual == expected
}

#[async_trait::async_trait]
impl SiteAdapter for TenhouAutoplay {
    fn plan(&self, ctx: &ActionContext<'_>) -> TurnPlan {
        self.state()
            .map_or_else(TurnPlan::default, |state| semantic_plan(ctx, &state))
    }

    async fn pointer_layout(
        &self,
        page: &Page,
        _ctx: &ActionContext<'_>,
    ) -> Result<Option<PointerLayout>> {
        Ok(layout(&client::read_input(page).await?))
    }

    async fn observe(
        &self,
        page: &Page,
        ctx: &ActionContext<'_>,
        intent: &PressIntent,
        point: Option<(f64, f64)>,
    ) -> Result<SiteObservation> {
        let state = self
            .state()
            .ok_or_else(|| anyhow!("Tenhou bridge state unavailable"))?;
        let input = client::read_input(page).await?;
        let observed_at = Instant::now();
        let mut observation = self.facts(&state, &input, &intent.action, observed_at);
        if !input.ready || !same_hand(&input, &state) || intent.action != *ctx.action {
            return Ok(observation);
        }
        let window = input
            .window
            .as_ref()
            .ok_or_else(|| anyhow!("ready Tenhou input lacks a window"))?;
        let Some(_) = state.window else {
            return Ok(observation);
        };
        let request = if intent.role == PressRole::Discard {
            let Some((tile, tsumogiri)) = discard_tile(&intent.action, &state) else {
                return Ok(observation);
            };
            if !input
                .tiles
                .iter()
                .any(|actual| actual.tile == tile && actual.legal && actual.drawn == tsumogiri)
            {
                return Ok(observation);
            }
            client::TargetRequest::Discard {
                window: window.clone(),
                hand: input.hand.clone(),
                tile,
                tsumogiri,
            }
        } else {
            let Some(wanted) = client::choice_payload(&intent.action, &state) else {
                return Ok(observation);
            };
            let Some(offer) = input.matching_offer(&wanted) else {
                return Ok(observation);
            };
            client::TargetRequest::Offer {
                window: window.clone(),
                identity: offer.identity.clone(),
            }
        };
        let probe = client::probe_target(page, &request).await?;
        let Some(mut target) = probe.target else {
            return Ok(observation);
        };
        if let Some(point) = point {
            let checked = client::validate_target(page, &request, &target.identity, point).await?;
            let Some(validated) = checked.target else {
                return Ok(observation);
            };
            if validated.identity != target.identity
                || validated.kind != target.kind
                || validated.bounds != target.bounds
                || validated.viewport != target.viewport
            {
                return Ok(observation);
            }
            target = validated;
        }
        observation.target_kind = if target.kind == client::TargetKind::Reveal {
            TargetKind::Reveal
        } else {
            TargetKind::Commit
        };
        observation.target = Some(InputTarget {
            identity: TargetIdentity {
                window: window.clone(),
                hand: input.hand.iter().map(u32::to_string).collect(),
                control: target.identity,
            },
            bounds: target.bounds,
            viewport: target.viewport,
        });
        observation.ready = true;
        Ok(observation)
    }
}

#[cfg(test)]
mod tests;
