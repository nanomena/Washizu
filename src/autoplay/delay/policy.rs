//! One pacing decision for a public turn transaction. Site adapters supply
//! clock/readiness facts; this object has no site-specific timing branches.

use super::{
    BudgetSnapshot, DecisionKind, DecisionProbs, DelayInput, DelayScript, Reading, TileClass,
};
use crate::autoplay::platform::TurnClockFacts;
use crate::config::{DelayDistribution, DelayMode, DelayModelConfig, InputConfig};
use crate::game_state::snapshot::GameStateSnapshot;
use crate::schema::MjaiEvent;
use rand::Rng;
use riichienv_core::action::{Action, ActionType};
use std::time::{Duration, Instant};

/// Facts for a selected action. The caller samples `now` after observing
/// readiness, and uses the same clock origin for inference and input costs.
pub struct PacingContext<'a> {
    pub action: &'a MjaiEvent,
    pub snapshot: &'a GameStateSnapshot,
    pub legal_actions: &'a [Action],
    pub our_seat: u8,
    pub kind: DecisionKind,
    pub first_action_of_kyoku: bool,
    pub in_riichi: bool,
    pub opening_animation: bool,
    pub probs: Option<DecisionProbs>,
    pub reading: Reading,
    pub clock: Option<TurnClockFacts>,
    pub now: Instant,
    /// An observed readiness constraint, in the same monotonic clock domain.
    pub earliest_press_at: Option<Instant>,
    /// Complete remaining input sequence: movement, hover, hold and gaps.
    pub click_overhead_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacingPlan {
    pub total_target_ms: u32,
    /// Absolute start time: later stages must not restart the sampled wait.
    pub start_at: Instant,
    pub wait_ms: u32,
    pub allow_bank: bool,
    pub hesitate: bool,
    pub look_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacingError {
    ClockStartsInFuture,
    Deadline {
        elapsed_ms: u32,
        required_ms: u32,
        available_ms: u32,
    },
}

impl std::fmt::Display for PacingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClockStartsInFuture => f.write_str("turn clock starts after the pacing observation"),
            Self::Deadline { elapsed_ms, required_ms, available_ms } => write!(f,
                "pacing cannot fit the turn deadline: elapsed {elapsed_ms}ms, earliest completion {required_ms}ms, available {available_ms}ms"),
        }
    }
}
impl std::error::Error for PacingError {}

pub struct PacingPolicy<'a> {
    cfg: &'a InputConfig,
    model: &'a DelayModelConfig,
    script: Option<&'a DelayScript>,
}

impl<'a> PacingPolicy<'a> {
    pub fn new(
        cfg: &'a InputConfig,
        model: &'a DelayModelConfig,
        script: Option<&'a DelayScript>,
    ) -> Self {
        Self { cfg, model, script }
    }

    pub fn decide<R: Rng + ?Sized>(
        &self,
        ctx: &PacingContext<'_>,
        rng: &mut R,
    ) -> Result<PacingPlan, PacingError> {
        let origin = ctx.clock.as_ref().map_or(ctx.now, |clock| clock.started_at);
        let elapsed = ctx
            .now
            .checked_duration_since(origin)
            .ok_or(PacingError::ClockStartsInFuture)?;
        let elapsed_ms = millis(elapsed);
        let mut model = self.model.clone();
        // A measured ready target replaces the old estimated render floors.
        // It must not start a second one-second animation wait.
        if ctx.earliest_press_at.is_some() {
            model.min_delay_ms = 0;
            model.min_button_delay_ms = 0;
        }
        let legacy = model.mode == DelayMode::Legacy;
        if legacy {
            model.distribution = DelayDistribution::Uniform;
        }
        let player = ctx.snapshot.players.iter().find(|p| p.seat == ctx.our_seat);
        let (is_tsumogiri, tile_class) = match (ctx.kind, ctx.action) {
            (DecisionKind::Dahai, MjaiEvent::Dahai { tsumogiri, pai, .. }) => {
                (*tsumogiri, TileClass::of_mjai(pai))
            }
            _ => (false, None),
        };
        // There is only one production construction of DelayInput. Both sites
        // use the tracker's meld/draw facts, never a website's hand-size guess.
        let input = DelayInput {
            kind: ctx.kind,
            is_tsumogiri,
            is_post_call: ctx.kind == DecisionKind::Dahai
                && player.is_some_and(|p| !p.melds.is_empty() && p.drawn_tile.is_none()),
            first_action_of_kyoku: ctx.first_action_of_kyoku,
            opening_animation: ctx.opening_animation && ctx.earliest_press_at.is_none(),
            can_riichi: ctx
                .legal_actions
                .iter()
                .any(|a| a.action_type == ActionType::Riichi),
            in_riichi: ctx.in_riichi,
            opponent_riichi: ctx
                .snapshot
                .players
                .iter()
                .any(|p| p.seat != ctx.our_seat && p.riichi_declared),
            tile_class,
            junme: player.map_or(0, |p| p.river.len() as u32 + 1),
            legal_action_count: ctx.legal_actions.len(),
            probs: ctx.probs,
            reading: ctx.reading,
            budget: ctx.clock.as_ref().map(|clock| BudgetSnapshot {
                fixed_ms: clock.fixed_ms,
                // An initial bank capacity is not its observed current balance.
                add_ms: clock.remaining_bank_ms.unwrap_or(0),
                elapsed_ms,
            }),
            click_overhead_ms: ctx.click_overhead_ms,
            cfg: self.cfg,
            delay_cfg: &model,
        };
        let decision = self
            .script
            .filter(|_| !legacy)
            .and_then(|script| script.try_decide(&input))
            .unwrap_or_else(|| super::decide(&input, rng));
        let readiness_ms = ctx
            .earliest_press_at
            .map_or(0, |at| millis(at.saturating_duration_since(origin)))
            .max(super::functional_floor(&input));
        let start_ms = decision
            .total_target_ms
            .saturating_sub(ctx.click_overhead_ms)
            .max(readiness_ms)
            .max(elapsed_ms);
        let completion_ms = start_ms.saturating_add(ctx.click_overhead_ms);
        // Safety margin/bank style caps trim the sampled thought, not a late
        // legal action. Only impossible physical completion is an error.
        let available_ms = ctx.clock.as_ref().map_or(u32::MAX, |clock| {
            clock
                .fixed_ms
                .saturating_add(clock.remaining_bank_ms.unwrap_or(0))
        });
        if ctx.clock.is_some() && completion_ms > available_ms {
            return Err(PacingError::Deadline {
                elapsed_ms,
                required_ms: completion_ms,
                available_ms,
            });
        }
        let start_at = origin + Duration::from_millis(u64::from(start_ms));
        Ok(PacingPlan {
            total_target_ms: decision.total_target_ms,
            start_at,
            wait_ms: millis(start_at.saturating_duration_since(ctx.now)),
            allow_bank: decision.allow_bank,
            hesitate: decision.hesitate,
            look_ms: decision.look_ms,
        })
    }
}

fn millis(duration: Duration) -> u32 {
    duration.as_millis().min(u128::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_state::snapshot::{MeldKind, MeldSnapshot, Phase, PlayerSnapshot};
    use rand::SeedableRng;

    fn snapshot() -> GameStateSnapshot {
        GameStateSnapshot {
            bakaze: "E".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 1,
            current_player: 0,
            turn_count: 1,
            phase: Phase::WaitAct,
            is_done: false,
            num_players: 4,
            dora_markers: vec![],
            our_seat: Some(0),
            players: (0..4)
                .map(|seat| PlayerSnapshot {
                    seat,
                    tehai: vec![],
                    melds: vec![],
                    river: vec![],
                    score: 25000,
                    riichi_declared: false,
                    riichi_stage: false,
                    double_riichi: false,
                    riichi_declaration_index: None,
                    kita_tiles: vec![],
                    drawn_tile: None,
                })
                .collect(),
        }
    }
    fn cfg() -> InputConfig {
        InputConfig {
            pre_click_delay_min_ms: 3000,
            pre_click_delay_max_ms: 3000,
            dealer_first_discard_extra_delay_ms: 0,
            ..Default::default()
        }
    }
    fn model() -> DelayModelConfig {
        DelayModelConfig {
            min_delay_ms: 0,
            min_button_delay_ms: 0,
            distribution: DelayDistribution::Uniform,
            safety_margin_ms: 500,
            bank_use_fraction: 0.5,
            bank_max_single_ms: 5000,
            ..Default::default()
        }
    }
    fn context<'a>(
        action: &'a MjaiEvent,
        snapshot: &'a GameStateSnapshot,
        origin: Instant,
    ) -> PacingContext<'a> {
        PacingContext {
            action,
            snapshot,
            legal_actions: &[],
            our_seat: 0,
            kind: DecisionKind::Dahai,
            first_action_of_kyoku: false,
            in_riichi: false,
            opening_animation: false,
            probs: None,
            reading: Reading::default(),
            clock: Some(TurnClockFacts {
                started_at: origin,
                fixed_ms: 5000,
                remaining_bank_ms: None,
            }),
            now: origin,
            earliest_press_at: None,
            click_overhead_ms: 300,
        }
    }
    fn rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(7)
    }

    #[test]
    fn real_clock_origins_share_one_formula_and_overhead_is_reserved_once() {
        let action = MjaiEvent::None;
        let snapshot = snapshot();
        let cfg = cfg();
        let model = model();
        let policy = PacingPolicy::new(&cfg, &model, None);
        for origin in [Instant::now(), Instant::now() - Duration::from_secs(10)] {
            let mut ctx = context(&action, &snapshot, origin);
            ctx.now = origin + Duration::from_millis(700);
            let plan = policy.decide(&ctx, &mut rng()).unwrap();
            assert_eq!(plan.total_target_ms, 3000);
            assert_eq!(plan.start_at, origin + Duration::from_millis(2700));
            assert_eq!(plan.wait_ms, 2000);
        }
    }
    #[test]
    fn elapsed_time_counts_without_restarting_a_wait_and_expired_input_is_refused() {
        let action = MjaiEvent::None;
        let snapshot = snapshot();
        let cfg = cfg();
        let model = model();
        let policy = PacingPolicy::new(&cfg, &model, None);
        let origin = Instant::now();
        let mut ctx = context(&action, &snapshot, origin);
        ctx.now = origin + Duration::from_millis(3500);
        assert_eq!(policy.decide(&ctx, &mut rng()).unwrap().wait_ms, 0);
        ctx.now = origin + Duration::from_millis(4800);
        assert!(matches!(
            policy.decide(&ctx, &mut rng()),
            Err(PacingError::Deadline {
                available_ms: 5000,
                ..
            })
        ));
        ctx.now = origin - Duration::from_millis(1);
        assert_eq!(
            policy.decide(&ctx, &mut rng()),
            Err(PacingError::ClockStartsInFuture)
        );
    }
    #[test]
    fn readiness_is_a_real_constraint_and_cannot_push_completion_past_the_deadline() {
        let action = MjaiEvent::None;
        let snapshot = snapshot();
        let cfg = cfg();
        let model = model();
        let policy = PacingPolicy::new(&cfg, &model, None);
        let origin = Instant::now();
        let mut ctx = context(&action, &snapshot, origin);
        ctx.earliest_press_at = Some(origin + Duration::from_millis(4000));
        assert_eq!(policy.decide(&ctx, &mut rng()).unwrap().wait_ms, 4000);
        ctx.earliest_press_at = Some(origin + Duration::from_millis(4800));
        assert!(matches!(
            policy.decide(&ctx, &mut rng()),
            Err(PacingError::Deadline { .. })
        ));
    }
    #[test]
    fn unknown_bank_is_not_spent_and_known_remaining_balance_uses_the_same_cap() {
        let action = MjaiEvent::None;
        let snapshot = snapshot();
        let cfg = cfg();
        let model = model();
        let script = DelayScript::compile(
            "function decide_delay(ctx) return {delay_ms=99999,allow_bank=true} end",
            "test",
        )
        .unwrap();
        let policy = PacingPolicy::new(&cfg, &model, Some(&script));
        let mut ctx = context(&action, &snapshot, Instant::now());
        let plan = policy.decide(&ctx, &mut rng()).unwrap();
        assert_eq!((plan.total_target_ms, plan.wait_ms), (4500, 4200));
        ctx.clock.as_mut().unwrap().remaining_bank_ms = Some(6000);
        let plan = policy.decide(&ctx, &mut rng()).unwrap();
        assert_eq!((plan.total_target_ms, plan.wait_ms), (7500, 7200));
    }
    #[test]
    fn legacy_mode_ignores_lua_while_lua_preserves_hesitation_and_look() {
        let action = MjaiEvent::None;
        let snapshot = snapshot();
        let cfg = cfg();
        let mut model = model();
        let script = DelayScript::compile(
            "function decide_delay(ctx) return {delay_ms=1234,hesitate=true,look_ms=321} end",
            "test",
        )
        .unwrap();
        let ctx = context(&action, &snapshot, Instant::now());
        let plan = PacingPolicy::new(&cfg, &model, Some(&script))
            .decide(&ctx, &mut rng())
            .unwrap();
        assert_eq!(
            (plan.total_target_ms, plan.hesitate, plan.look_ms),
            (1234, true, Some(321))
        );
        model.mode = DelayMode::Legacy;
        let plan = PacingPolicy::new(&cfg, &model, Some(&script))
            .decide(&ctx, &mut rng())
            .unwrap();
        assert_eq!(
            (plan.total_target_ms, plan.hesitate, plan.look_ms),
            (3000, false, None)
        );
    }
    #[test]
    fn common_tracker_facts_detect_post_call_red_tiles_and_defence_for_lua() {
        let action = MjaiEvent::Dahai {
            actor: 0,
            pai: "5pr".into(),
            tsumogiri: false,
        };
        let mut snapshot = snapshot();
        let cfg = cfg();
        let model = model();
        snapshot.players[0].melds.push(MeldSnapshot {
            kind: MeldKind::Pon,
            tiles: vec!["P".into(); 3],
            from_who: 1,
            called_tile: Some("P".into()),
        });
        snapshot.players[1].riichi_declared = true;
        let script = DelayScript::compile("function decide_delay(ctx) assert(ctx.post_call and ctx.opponent_riichi and ctx.tile_class=='middle' and not ctx.tsumogiri); return {delay_ms=3456} end", "facts").unwrap();
        let ctx = context(&action, &snapshot, Instant::now());
        assert_eq!(
            PacingPolicy::new(&cfg, &model, Some(&script))
                .decide(&ctx, &mut rng())
                .unwrap()
                .total_target_ms,
            3456
        );
    }

    #[test]
    fn late_ready_target_drops_style_wait_and_uses_remaining_physical_headroom() {
        let action = MjaiEvent::None;
        let snapshot = snapshot();
        let cfg = cfg();
        let mut model = model();
        model.min_delay_ms = 1000;
        let origin = Instant::now();
        let mut ctx = context(&action, &snapshot, origin);
        ctx.now = origin + Duration::from_millis(4700);
        ctx.earliest_press_at = Some(ctx.now);
        ctx.click_overhead_ms = 200;
        let plan = PacingPolicy::new(&cfg, &model, None)
            .decide(&ctx, &mut rng())
            .unwrap();
        assert_eq!(plan.start_at, ctx.now);
        assert_eq!(plan.wait_ms, 0);
        // A short real clock plus an already observed ready control cannot
        // inherit the obsolete estimated one-second render floor.
        ctx.now = origin;
        ctx.earliest_press_at = Some(origin);
        ctx.clock.as_mut().unwrap().fixed_ms = 500;
        assert_eq!(
            PacingPolicy::new(&cfg, &model, None)
                .decide(&ctx, &mut rng())
                .unwrap()
                .wait_ms,
            0
        );
    }

    #[test]
    fn fast_reach_clips_style_before_the_two_physical_presses() {
        let action = MjaiEvent::Reach {
            actor: 0,
            pai: Some("5m".into()),
            tsumogiri: Some(false),
        };
        let snapshot = snapshot();
        let cfg = cfg();
        let mut model = model();
        model.min_button_delay_ms = 1600;
        model.safety_margin_ms = 1000;
        let origin = Instant::now();
        let mut ctx = context(&action, &snapshot, origin);
        ctx.kind = DecisionKind::Reach;
        ctx.clock.as_mut().unwrap().fixed_ms = 3000;
        ctx.earliest_press_at = Some(origin);
        ctx.click_overhead_ms = 900;
        let plan = PacingPolicy::new(&cfg, &model, None)
            .decide(&ctx, &mut rng())
            .unwrap();
        assert_eq!((plan.total_target_ms, plan.wait_ms), (2000, 1100));
        ctx.now = origin + Duration::from_millis(2000);
        ctx.earliest_press_at = Some(ctx.now);
        assert_eq!(
            PacingPolicy::new(&cfg, &model, None)
                .decide(&ctx, &mut rng())
                .unwrap()
                .wait_ms,
            0
        );
    }
}
