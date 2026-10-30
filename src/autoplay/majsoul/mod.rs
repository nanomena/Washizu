//! Majsoul implementation of [`SiteAdapter`].
//!
//! Target matching combines protocol identity with the current rendered
//! controls. Coordinate tables only delimit where a control may appear:
//!
//! - The current `legal_actions` from the riichi engine (which buttons
//!   are visible, plus chi/pon/kan candidate enumeration).
//! - The current hand from `GameStateSnapshot` (sort-aware tile lookup).
//! - The dispatch flow from the original Akagi Python autoplay
//!   (`autoplay_majsoul.py`, the main action handler).

mod candidates;
pub mod coords;

use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::platform::{window_identity, TargetKind};
use crate::autoplay::platform::{
    ActionContext, Confirmation, InputTarget, PressIntent, PressRole, SiteAdapter, SiteObservation,
    TargetIdentity, TurnClockFacts, TurnPlan,
};
use crate::autoplay::pointer::{PointerLayout, TileTarget};
use crate::bridge::majsoul::tile::compare_pai;
use crate::schema::MjaiEvent;
use anyhow::{anyhow, Result};
use chromiumoxide::page::Page;
#[cfg(test)]
use coords::TSUMO_SPACE;
use coords::{
    action_button_pos, get_pai_coord, kan_candidate_pos, MajsoulOpType, ACTION_PRIORITY, TILES,
};
use rand::Rng;
use riichienv_core::action::{Action, ActionType};
use riichienv_core::parser::tid_to_mjai;
use serde::Deserialize;
use std::sync::Arc;

pub struct MajsoulAutoplay {
    context: Arc<AutoplayContext>,
}

#[derive(Debug, Clone, PartialEq)]
struct MappedClick {
    x_norm: f64,
    y_norm: f64,
}

#[derive(Debug, Default)]
struct TargetMapping {
    targets: Vec<MappedClick>,
    dealt: bool,
    choice: Option<MeldChoice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MeldChoice {
    index: usize,
    count: usize,
}

impl MajsoulAutoplay {
    pub fn new(context: Arc<AutoplayContext>) -> Self {
        Self { context }
    }
}

/// A measured meld face as this frame shows it: the whole face is the hit
/// region, its glyph-centred interior the aim. Whether it is at rest is the
/// turn controller's judgement across reads.
fn meld_target(identity: TargetIdentity, viewport: CanvasRect, face: CanvasRect) -> InputTarget {
    InputTarget {
        identity,
        viewport,
        bounds: CanvasRect {
            x: face.x + face.width * 0.22,
            y: face.y + face.height * 0.28,
            width: face.width * 0.56,
            height: face.height * 0.38,
        },
        hit: face,
    }
}

#[derive(Deserialize)]
struct ClientView {
    viewport: CanvasRect,
    clear: bool,
}

const READ_SURFACE: &str = include_str!("surface.js");

async fn read_view(page: &Page, point: Option<(f64, f64)>) -> Result<Option<ClientView>> {
    #[derive(Deserialize)]
    struct Surface {
        view: Option<ClientView>,
    }
    // The wrapper preserves a real null as data through CDP's RemoteObject.
    let request = serde_json::to_string(&point)?;
    let result = page
        .evaluate_expression(format!("({READ_SURFACE})({request})"))
        .await?;
    let value = result
        .value()
        .ok_or_else(|| anyhow!("Majsoul surface observation returned no value"))?;
    Ok(serde_json::from_value::<Surface>(value.clone())?.view)
}

async fn sorted_table(page: &Page, rect: CanvasRect) -> Result<bool> {
    use crate::autoplay::pixels::{self, Glyph};
    let (image, scale) = pixels::shot(page).await?;
    Ok(pixels::toggle_in(&image, scale, rect, coords::AUTO_SORT) == Glyph::Lit)
}

fn target_bounds(rect: CanvasRect, point: (f64, f64)) -> CanvasRect {
    let reach = reach_of(point);
    let (x, y) = rect.pixel(point.0 - reach.half_x, point.1 - reach.half_y);
    CanvasRect {
        x,
        y,
        width: rect.width * 2.0 * reach.half_x / 16.0,
        height: rect.height * 2.0 * reach.half_y / 9.0,
    }
}

fn tile_bounds(rect: CanvasRect, point: (f64, f64)) -> CanvasRect {
    target_bounds(rect, point)
}

/// The already confirmed protocol hand, in the client's established sorted
/// rack order. This does not claim Unity exposes native hand objects.
fn protocol_tiles(ctx: &ActionContext<'_>) -> Option<Vec<(String, bool)>> {
    let player = ctx.snapshot?.players.get(ctx.our_seat as usize)?;
    let mut hand = player.tehai.clone();
    if hand.iter().any(|tile| tile == "?") {
        return None;
    }
    hand.sort_by(|a, b| compare_pai(a, b));
    if is_dealer_opening(ctx) {
        return Some(hand.into_iter().map(|tile| (tile, false)).collect());
    }
    let drawn = player
        .drawn_tile
        .as_deref()
        .or(ctx.last_self_tsumo)
        .filter(|_| hand.len() % 3 == 2);
    if let Some(tile) = drawn {
        let index = hand.iter().rposition(|t| t == tile)?;
        let tile = hand.remove(index);
        let mut tiles: Vec<_> = hand.into_iter().map(|tile| (tile, false)).collect();
        tiles.push((tile, true));
        Some(tiles)
    } else {
        Some(hand.into_iter().map(|tile| (tile, false)).collect())
    }
}

fn layout(view: &ClientView, ctx: &ActionContext<'_>) -> Option<PointerLayout> {
    if !view.clear {
        return None;
    }
    let hand = protocol_tiles(ctx)?;
    let opening = is_dealer_opening(ctx);
    let rack = hand.iter().filter(|(_, drawn)| !drawn).count();
    let tiles = hand
        .into_iter()
        .enumerate()
        .map(|(index, (tile, drawn))| {
            let point = if opening {
                *TILES.get(index)?
            } else {
                get_pai_coord(if drawn { 13 } else { index }, rack)
            };
            Some(TileTarget {
                tile,
                drawn,
                bounds: tile_bounds(view.viewport, point),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let (x, y) = view.viewport.pixel(7.5, 7.55);
    let result = PointerLayout {
        viewport: view.viewport,
        tiles,
        next_draw: None,
        rest: CanvasRect {
            x,
            y,
            width: view.viewport.width * 0.3 / 16.0,
            height: view.viewport.height * 0.15 / 9.0,
        },
    };
    result.valid().then_some(result)
}

#[async_trait::async_trait]
impl SiteAdapter for MajsoulAutoplay {
    fn plan(&self, ctx: &ActionContext<'_>) -> TurnPlan {
        if ctx.snapshot.is_none() {
            return TurnPlan::default();
        }
        let normalized = ActionContext {
            pointer: None,
            ..*ctx
        };
        let mapping = mapped_targets(&normalized);
        let mut plan = TurnPlan {
            presses: Vec::new(),
            dealt: mapping.dealt,
        };
        let roles = match ctx.action {
            MjaiEvent::Reach { .. } if mapping.targets.len() == 2 => {
                vec![PressRole::Declaration, PressRole::Discard]
            }
            MjaiEvent::Reach { .. } => return TurnPlan::default(),
            MjaiEvent::Dahai { .. } if !ctx.self_riichi_accepted => vec![PressRole::Discard],
            _ if mapping.targets.len() == 2 => vec![PressRole::Action, PressRole::Candidate],
            _ if mapping.targets.len() == 1 => vec![PressRole::Action],
            _ => return TurnPlan::default(),
        };
        if mapping.targets.is_empty() {
            return TurnPlan::default();
        }
        for (index, role) in roles.iter().enumerate() {
            plan.presses.push(PressIntent {
                role: *role,
                action: ctx.action.clone(),
                confirmation: if index + 1 == roles.len() {
                    Confirmation::ClientCommand
                } else {
                    Confirmation::Dispatch
                },
            });
        }
        plan
    }

    async fn pointer_layout(
        &self,
        page: &Page,
        ctx: &ActionContext<'_>,
    ) -> Result<Option<PointerLayout>> {
        let Some(view) = read_view(page, None).await? else {
            return Ok(None);
        };
        if !sorted_table(page, view.viewport).await? {
            return Ok(None);
        }
        Ok(layout(&view, ctx))
    }

    async fn observe(
        &self,
        page: &Page,
        ctx: &ActionContext<'_>,
        intent: &PressIntent,
        actual: Option<(f64, f64)>,
    ) -> Result<SiteObservation> {
        let budget = *self
            .context
            .time_budget
            .read()
            .map_err(|_| anyhow!("Majsoul window lock poisoned"))?;
        let mut observation = SiteObservation {
            ready: false,
            blocked: None,
            target_kind: TargetKind::Commit,
            target: None,
            layout: None,
            clock: budget.map(|b| TurnClockFacts {
                started_at: b.opened_at,
                fixed_ms: b.fixed_ms,
                bank_at_start_ms: Some(b.add_ms),
            }),
            continuation: None,
        };
        if intent.action != *ctx.action {
            return Ok(observation.because("intent is for another action"));
        }
        let Some(view) = read_view(page, actual).await? else {
            return Ok(observation.because("game view unavailable"));
        };
        observation.layout = layout(&view, ctx);
        if !view.clear {
            return Ok(observation.because("table is covered"));
        }
        if observation.layout.is_none() {
            return Ok(observation.because("hand layout unknown"));
        }
        let Some(budget) = budget else {
            return Ok(observation.because("no decision time budget"));
        };
        let normalized = ActionContext {
            pointer: ctx.pointer.map(|p| view.viewport.norm(p.0, p.1)),
            ..*ctx
        };
        let mapping = mapped_targets(&normalized);
        if let Some(choice) = mapping.choice {
            let (image, scale) = crate::autoplay::pixels::shot(page).await?;
            let (face, control, kind) =
                if let Some(row) = candidates::read(&image, scale, view.viewport, choice.count) {
                    let Some(face) = row.target(choice.index) else {
                        return Ok(observation.because("candidate row lacks the selected choice"));
                    };
                    (
                        face,
                        format!("meld:candidate:{}:{}", choice.index, choice.count),
                        TargetKind::Commit,
                    )
                } else {
                    let Some(button) = mapping.targets.first() else {
                        return Ok(observation.because("no mapped call button"));
                    };
                    let Some(face) = candidates::action(
                        &image,
                        scale,
                        view.viewport,
                        (button.x_norm, button.y_norm),
                    ) else {
                        return Ok(observation.because("call button not visible"));
                    };
                    (
                        face,
                        format!("meld:button:{:?}", ctx.action),
                        if choice.count > 1 {
                            TargetKind::Reveal
                        } else {
                            TargetKind::Commit
                        },
                    )
                };
            if *self
                .context
                .time_budget
                .read()
                .map_err(|_| anyhow!("Majsoul window lock poisoned"))?
                != Some(budget)
            {
                return Ok(observation.because("decision window changed"));
            }
            let identity = TargetIdentity {
                window: window_identity(crate::bot::control::InputWindow::Majsoul(budget).proof()),
                hand: protocol_tiles(ctx)
                    .unwrap()
                    .into_iter()
                    .map(|(tile, drawn)| format!("{tile}:{drawn}"))
                    .collect(),
                control,
            };
            observation.target_kind = kind;
            observation.target = Some(meld_target(identity, view.viewport, face));
            observation.ready = true;
            return Ok(observation);
        }
        let selected = match intent.role {
            PressRole::Candidate => mapping.targets.get(1),
            PressRole::Discard if matches!(ctx.action, MjaiEvent::Reach { .. }) => {
                mapping.targets.get(1)
            }
            _ => mapping.targets.first(),
        };
        let Some(point) = selected else {
            return Ok(observation.because("no mapped target"));
        };
        let point = (point.x_norm, point.y_norm);
        let bounds = target_bounds(view.viewport, point);
        if intent.role == PressRole::Discard {
            let Some(tile) = observation
                .layout
                .as_ref()
                .and_then(|l| l.tiles.iter().find(|t| t.bounds == bounds))
            else {
                return Ok(observation.because("no hand tile at the mapped slot"));
            };
            let (pai, drawn) = match ctx.action {
                MjaiEvent::Dahai { pai, tsumogiri, .. } => (pai, Some(*tsumogiri)),
                MjaiEvent::Reach {
                    pai: Some(pai),
                    tsumogiri,
                    ..
                } => (pai, *tsumogiri),
                _ => return Ok(observation.because("action has no discard tile")),
            };
            if tile.tile != *pai
                || (!is_dealer_opening(ctx) && drawn.is_some_and(|d| d != tile.drawn))
            {
                return Ok(observation.because("mapped slot holds a different tile"));
            }
        }
        if actual.is_some_and(|p| !bounds.contains(p.0, p.1)) {
            return Ok(observation.because("pointer is outside the target"));
        }
        // Unity keeps the rack inside WASM. The protocol mapping is usable
        // only while the actual table shows automatic sorting enabled.
        // A covered table or manual rack never gets final input permission.
        if actual.is_some() && !sorted_table(page, view.viewport).await? {
            return Ok(observation.because("rack is not auto-sorted"));
        }
        if *self
            .context
            .time_budget
            .read()
            .map_err(|_| anyhow!("Majsoul window lock poisoned"))?
            != Some(budget)
        {
            return Ok(observation.because("decision window changed"));
        }
        observation.target = Some(InputTarget {
            identity: TargetIdentity {
                window: window_identity(crate::bot::control::InputWindow::Majsoul(budget).proof()),
                hand: protocol_tiles(ctx)
                    .unwrap()
                    .into_iter()
                    .map(|(tile, drawn)| format!("{tile}:{drawn}"))
                    .collect(),
                control: format!("{:?}:{}:{}", intent.role, point.0, point.1),
            },
            bounds,
            hit: bounds,
            viewport: view.viewport,
        });
        observation.ready = true;
        Ok(observation)
    }
}

fn mapped_targets(ctx: &ActionContext) -> TargetMapping {
    let mut result = TargetMapping::default();

    match ctx.action {
        // ----- Dahai (打牌) ---------------------------------------------
        MjaiEvent::Dahai {
            actor,
            pai,
            tsumogiri,
        } if *actor == ctx.our_seat => {
            // While in riichi, Majsoul auto-discards, so ours would be a
            // second one. (The riichi-declaring tile goes out inside the
            // Reach plan below, before acceptance, so it is unaffected.)
            if ctx.self_riichi_accepted {
                // …with one exception: the auto-discard is held back
                // while an own-draw operation prompt is open — kita on
                // a drawn North (sanma), a riichi-legal ankan, or a
                // tsumo agari. Majsoul waits for an answer, so a bot
                // decision to tsumogiri must decline the prompt via
                // the X button; the client then discards the draw on
                // its own. Returning with no click here left the game
                // hanging until the turn timer and the entire time
                // bank drained (the server eventually auto-declines).
                if riichi_prompt_pending(ctx, pai) {
                    if let Some(button) = action_button_for(MajsoulOpType::None, ctx) {
                        result.targets.push(MappedClick {
                            x_norm: button.0,
                            y_norm: button.1,
                        });
                    }
                }
                return result;
            }

            let whence = if *tsumogiri {
                Whence::Drawn
            } else {
                Whence::Hand
            };
            if let Some(click) = plan_dahai_click(pai, whence, ctx) {
                result.targets.push(click);
            }
        }

        // ----- Reach (立直) — declaration + tile in one plan ----------
        MjaiEvent::Reach {
            actor,
            pai,
            tsumogiri,
        } if *actor == ctx.our_seat => {
            // Majsoul fuses declaring + discarding into one action, so the
            // tile must be known up front. It normally is — the bot fills
            // `Reach.pai` (natively or via the manager's autoplay reach
            // follow-up, #257, which logs when it cannot). A bare reach
            // here means that resolution failed; declare nothing rather
            // than press the button and leave the client sitting on an
            // owed discard until timeout.
            let Some(tile) = pai else {
                return TargetMapping::default();
            };

            if let Some(button) = action_button_for(MajsoulOpType::Reach, ctx) {
                result.targets.push(MappedClick {
                    x_norm: button.0,
                    y_norm: button.1,
                });
            } else {
                // Reach not in legal_actions — bridge desync; bail.
                return TargetMapping::default();
            }
            // The copy the bot named, as for a discard; a bot that names
            // none leaves it to `Whence::Unsaid`.
            let whence = match tsumogiri {
                Some(true) => Whence::Drawn,
                Some(false) => Whence::Hand,
                None => Whence::Unsaid,
            };
            if let Some(click) = plan_dahai_click(tile, whence, ctx) {
                result.targets.push(click);
            }
        }

        // ----- Chi / Pon / Daiminkan / Ankan / Kakan -------------------
        // (action button + optional candidate disambiguation)
        MjaiEvent::Chi { actor, .. } if *actor == ctx.our_seat => {
            plan_meld(MajsoulOpType::Chi, ActionType::Chi, &mut result, ctx);
        }
        MjaiEvent::Pon { actor, .. } if *actor == ctx.our_seat => {
            plan_meld(MajsoulOpType::Pon, ActionType::Pon, &mut result, ctx);
        }
        MjaiEvent::Daiminkan { actor, .. } if *actor == ctx.our_seat => {
            plan_meld(
                MajsoulOpType::Daiminkan,
                ActionType::Daiminkan,
                &mut result,
                ctx,
            );
        }
        MjaiEvent::Ankan { actor, .. } if *actor == ctx.our_seat => {
            plan_kan(MajsoulOpType::Ankan, ActionType::Ankan, &mut result, ctx);
        }
        MjaiEvent::Kakan { actor, .. } if *actor == ctx.our_seat => {
            plan_kan(MajsoulOpType::Kakan, ActionType::Kakan, &mut result, ctx);
        }

        // ----- Hora — zimo button on own draw, ron on opponent ---------
        MjaiEvent::Hora { actor, .. } if *actor == ctx.our_seat => {
            let op = if hora_is_tsumo(ctx) {
                MajsoulOpType::Zimo
            } else {
                MajsoulOpType::Ron
            };

            if let Some(button) = action_button_for(op, ctx) {
                result.targets.push(MappedClick {
                    x_norm: button.0,
                    y_norm: button.1,
                });
            }
        }

        // ----- Ryukyoku (九種九牌) -------------------------------------
        MjaiEvent::Ryukyoku { .. } => {
            if let Some(button) = action_button_for(MajsoulOpType::Ryukyoku, ctx) {
                result.targets.push(MappedClick {
                    x_norm: button.0,
                    y_norm: button.1,
                });
            }
        }

        // ----- Kita (3p 北抜き) ----------------------------------------
        MjaiEvent::Kita { actor, .. } if *actor == ctx.our_seat => {
            // On the opening draw of a kyoku, Majsoul plays a tile-
            // dealing animation; clicks issued during it land on the
            // wrong target. Folded into the delay model as
            // `opening_animation` (same wait as dealer first discard).

            if let Some(button) = action_button_for(MajsoulOpType::Nukidora, ctx) {
                result.targets.push(MappedClick {
                    x_norm: button.0,
                    y_norm: button.1,
                });
            }
        }

        // ----- None — pass / cancel button -----------------------------
        //
        // The bot emits `None` on every mjai event it has nothing to say
        // about — including pure echoes of other players' tsumo/dahai
        // notifies, where Majsoul is showing no buttons at all. Without
        // a gate we'd loop-click the lobby/preview UI's rightmost button
        // on every other-player turn.
        //
        // riichienv only adds `ActionType::Pass` to legal_actions in
        // `Phase::WaitResponse` (`riichienv-core/src/state/legal_actions.rs:249`)
        // — i.e. exactly when Majsoul is showing the Pass button after a
        // claimable discard. Use that as the visibility gate.
        MjaiEvent::None => {
            if !pass_button_visible(ctx) {
                return result;
            }
            // Extra guard: never click Pass during WaitAct (own draw turn).
            // Stale legal_actions from the previous round can falsely expose
            // Pass before the tracker has processed start_kyoku.
            if !ctx
                .snapshot
                .is_some_and(|s| s.phase == crate::game_state::snapshot::Phase::WaitResponse)
            {
                return result;
            }
            // Extra guard: only click Skip when there's an actual claim option
            // (pon/ron/daiminkan). In 3p there's no chi, so many WaitResponse
            // windows have no claimable actions — Majsoul shows no buttons at all
            // and a ghost click would land on the wrong UI element.
            let has_claim = ctx.legal_actions.iter().any(|a| {
                matches!(
                    a.action_type,
                    riichienv_core::action::ActionType::Pon
                        | riichienv_core::action::ActionType::Daiminkan
                        | riichienv_core::action::ActionType::Ron
                        | riichienv_core::action::ActionType::Chi
                )
            });
            if !has_claim {
                return result;
            }

            if let Some(button) = action_button_for(MajsoulOpType::None, ctx) {
                result.targets.push(MappedClick {
                    x_norm: button.0,
                    y_norm: button.1,
                });
            }
        }

        // Everything else (StartGame, Tsumo, Dora, ReachAccepted,
        // EndKyoku, EndGame, events from other seats) doesn't drive
        // a click.
        _ => {}
    }

    // Anything the dealer presses on its first turn answers a hand that
    // is still being dealt when the window opens.
    result.dealt = !result.targets.is_empty() && is_dealer_opening(ctx);
    result
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Where `pai` sits in our hand, as a normalised 16:9 point — the place a
/// discard of it would be pressed.
pub fn discard_point(pai: &str, ctx: &ActionContext) -> Option<(f64, f64)> {
    let MappedClick { x_norm, y_norm } = plan_dahai_click(pai, Whence::Unsaid, ctx)?;
    Some((x_norm, y_norm))
}

/// Where a tile of a hand at rest — between turns, nothing drawn — lies: the
/// copy `pointer` is already on, else one of its copies at random. For the
/// pointer's lean (`autoplay::pointer::lean`).
pub fn hand_point(
    pai: &str,
    tehai: &[String],
    pointer: Option<(f64, f64)>,
    rng: &mut impl Rng,
) -> Option<(f64, f64)> {
    let mut sorted = tehai.to_vec();
    sorted.sort_by(|a, b| compare_pai(a, b));
    let copies: Vec<usize> = (0..sorted.len()).filter(|&i| sorted[i] == pai).collect();
    let at = |i: usize| (i < TILES.len() - 1).then(|| get_pai_coord(i, sorted.len()));
    at(pick_copy(&copies, at, pointer, rng)?)
}

/// The part of the target at `at` a press may land on (see
/// `autoplay::pointer::aim`): the middle six tenths of a hand tile, of an
/// action button, of a call candidate. A point that is none of these is
/// given a small reach.
pub fn reach_of(at: (f64, f64)) -> crate::autoplay::pointer::aim::Reach {
    use crate::autoplay::pointer::aim::Reach;
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    let is = |table: &[(f64, f64)]| table.iter().any(|p| near(p.0, at.0) && near(p.1, at.1));
    if near(at.1, TILES[0].1) {
        // A tile is a slot's width (0.79) by about 1.15 high.
        Reach {
            half_x: 0.24,
            half_y: 0.35,
            from_top: true,
        }
    } else if is(&coords::ACTIONS) {
        Reach {
            half_x: 0.5,
            half_y: 0.2,
            from_top: false,
        }
    } else if is(&coords::CANDIDATES) || is(&coords::CANDIDATES_KAN) {
        Reach {
            half_x: 0.22,
            half_y: 0.3,
            from_top: false,
        }
    } else if is(&[coords::CONFIRM]) {
        // The button is about 1.45 wide and 0.53 high.
        Reach {
            half_x: 0.45,
            half_y: 0.16,
            from_top: false,
        }
    } else if is(&[coords::GAME_CONFIRM, coords::REMATCH]) {
        // Each is about 2.08 wide and 0.6 high.
        Reach {
            half_x: 0.6,
            half_y: 0.18,
            from_top: false,
        }
    } else if is(&[coords::REMATCH_ASK_CONFIRM]) {
        // About 2.55 wide and 0.6 high.
        Reach {
            half_x: 0.7,
            half_y: 0.18,
            from_top: false,
        }
    } else if is(&[
        coords::AUTO_SORT,
        coords::AUTO_WIN,
        coords::NO_CALLS,
        coords::AUTO_TSUMOGIRI,
    ]) {
        Reach {
            half_x: 0.08,
            half_y: 0.1,
            from_top: false,
        }
    } else {
        Reach {
            half_x: 0.12,
            half_y: 0.12,
            from_top: false,
        }
    }
}

/// How far up or down from a tile's middle the pointer is still on it.
const ON_TILE_HEIGHT: f64 = 0.75;

/// Whether `pointer` is on the tile whose middle is `tile`: within half the
/// rack's pitch across, and most of a tile's height up or down. Both are
/// normalised 16:9 points.
pub fn on_tile(tile: (f64, f64), pointer: Option<(f64, f64)>) -> bool {
    let half_pitch = (TILES[1].0 - TILES[0].0) / 2.0;
    pointer.is_some_and(|(x, y)| {
        (x - tile.0).abs() <= half_pitch && (y - tile.1).abs() <= ON_TILE_HEIGHT
    })
}

/// Where the next draw will land on a hand of `count` tiles at rest.
pub fn draw_point(count: usize) -> (f64, f64) {
    get_pai_coord(13, count)
}

/// Which copy of a tile a discard presses when the rack shows more than one.
/// The game records a press of the drawn tile as a tsumogiri and any other
/// as a tedashi, and the other players see which it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Whence {
    /// The tile just drawn: a tsumogiri.
    Drawn,
    /// One already in the hand: a tedashi. The drawn tile only when the hand
    /// holds no other copy.
    Hand,
    /// Not said (a reach from a bot that names only its tile; a glance has
    /// no copy): the drawn tile when it is that tile, else one from the hand.
    Unsaid,
}

/// One of `places` at random.
fn any_of(places: &[usize], rng: &mut impl Rng) -> Option<usize> {
    (!places.is_empty()).then(|| places[crate::autoplay::humanize::one_of(places.len(), rng)])
}

/// Which of the copies at `places` to press, each lying where `at` says: the
/// one the pointer is already on, for a hand resting on a tile does not leave
/// it for its twin; else any of them at random. Identical tiles are
/// interchangeable to the game, and a hand that always took the leftmost
/// would be a pattern.
fn pick_copy(
    places: &[usize],
    at: impl Fn(usize) -> Option<(f64, f64)>,
    pointer: Option<(f64, f64)>,
    rng: &mut impl Rng,
) -> Option<usize> {
    places
        .iter()
        .copied()
        .find(|&i| at(i).is_some_and(|tile| on_tile(tile, pointer)))
        .or_else(|| any_of(places, rng))
}

/// Plan a hand-tile click for a discard or riichi-declaring discard.
fn plan_dahai_click(pai: &str, whence: Whence, ctx: &ActionContext) -> Option<MappedClick> {
    let snapshot = ctx.snapshot?;
    {
        use rand::SeedableRng;
        use std::hash::{Hash, Hasher};
        let mut key = std::collections::hash_map::DefaultHasher::new();
        (pai, snapshot.kyoku, snapshot.honba, snapshot.turn_count).hash(&mut key);
        if let Some(player) = snapshot.players.get(ctx.our_seat as usize) {
            player.tehai.hash(&mut key);
        }
        plan_dahai_click_with(
            pai,
            whence,
            ctx,
            &mut rand::rngs::StdRng::seed_from_u64(key.finish()),
        )
    }
}

fn plan_dahai_click_with(
    pai: &str,
    whence: Whence,
    ctx: &ActionContext,
    rng: &mut impl Rng,
) -> Option<MappedClick> {
    let snapshot = ctx.snapshot?;
    let our_seat = ctx.our_seat as usize;
    if our_seat >= snapshot.players.len() {
        return None;
    }

    // Dealer's first discard: Majsoul lays all 14 tiles continuously on
    // the rack (sorted) — there's no "tsumohai" gap. Click position is
    // the index in the fully-sorted 14-tile array, using TILES[i]
    // directly (not get_pai_coord, which would add TSUMO_SPACE for i=13).
    if is_dealer_opening(ctx) {
        let mut sorted = snapshot.players[our_seat].tehai.clone();
        sorted.sort_by(|a, b| compare_pai(a, b));
        let copies: Vec<usize> = (0..sorted.len()).filter(|&i| sorted[i] == pai).collect();
        let idx = pick_copy(&copies, |i| TILES.get(i).copied(), ctx.pointer, rng)?;
        let (x, y) = TILES.get(idx).copied()?;
        return Some(MappedClick {
            x_norm: x,
            y_norm: y,
        });
    }

    let tehai: Vec<String> = snapshot.players[our_seat].tehai.clone();
    // Prefer snapshot drawn_tile (always accurate) over last_self_tsumo
    // (None after pon/chi/kita since no Tsumo event fires for those draws).
    // In 3p: if snapshot_drawn is "N" but last_self_tsumo is set to something
    // else, the snapshot predates the Kita; use last_self_tsumo (rinshan).
    let snapshot_drawn = snapshot
        .players
        .get(our_seat)
        .and_then(|p| p.drawn_tile.as_deref());
    let tsumohai = match (snapshot_drawn, ctx.last_self_tsumo) {
        (Some("N"), Some(rinshan)) => Some(rinshan),
        (Some(t), _) => Some(t),
        (None, fallback) => fallback,
    };

    // Hand sizes that include the just-drawn tile: 2/5/8/11/14 (mod 3 = 2).
    let has_tsumohai = matches!(tehai.len(), 14 | 11 | 8 | 5 | 2) && tsumohai.is_some();

    // Sort the full tehai to match Majsoul visual order.
    // riichienv always sorts hand after every draw, so this is accurate.
    let mut sorted_tehai = tehai.clone();
    sorted_tehai.sort_by(|a, b| compare_pai(a, b));

    if has_tsumohai {
        let t = tsumohai.unwrap();
        // The tsumohai sits apart on the far right. Exclude the last
        // occurrence of the tsumohai tile from sorted_tehai; what remains
        // is the rack left of the gap, in order.
        let tsumo_excl = sorted_tehai.iter().rposition(|x| x.as_str() == t);
        let copies: Vec<usize> = sorted_tehai
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != tsumo_excl)
            .enumerate()
            .filter(|(_, (_, x))| x.as_str() == pai)
            .map(|(visual_idx, _)| visual_idx)
            .collect();
        // The drawn tile itself: for a tsumogiri, and for a tedashi only
        // when the hand holds no other copy of it.
        if pai == t && (whence != Whence::Hand || copies.is_empty()) {
            let (x, y) = get_pai_coord(13, tehai.len() - 1);
            return Some(MappedClick {
                x_norm: x,
                y_norm: y,
            });
        }
        let rack = tehai.len() - 1;
        let at = |i: usize| (i < TILES.len() - 1).then(|| get_pai_coord(i, rack));
        let idx = pick_copy(&copies, at, ctx.pointer, rng)?;
        if idx >= TILES.len() - 1 {
            return None;
        }
        let (x, y) = get_pai_coord(idx, rack);
        return Some(MappedClick {
            x_norm: x,
            y_norm: y,
        });
    }

    // No tsumohai: all tiles closed, click by sorted index directly.
    let copies: Vec<usize> = (0..sorted_tehai.len())
        .filter(|&i| sorted_tehai[i] == pai)
        .collect();
    let at = |i: usize| (i < TILES.len() - 1).then(|| get_pai_coord(i, tehai.len()));
    let idx = pick_copy(&copies, at, ctx.pointer, rng)?;
    if idx >= TILES.len() - 1 {
        return None;
    }
    let (x, y) = get_pai_coord(idx, tehai.len());
    Some(MappedClick {
        x_norm: x,
        y_norm: y,
    })
}
/// True on the dealer's first turn of a kyoku, before it has done
/// anything — the turn whose window opens as Mahjong Soul deals 14
/// tiles and plays the hand-sort animation (presses during it are
/// dropped), after which all 14 tiles sit continuously on the rack (no
/// tsumohai offset). Detected by: we're oya, our hand size is 14, and
/// we have no discards or melds yet.
///
/// 3p caveat: a kita (北抜き) declared on the opening hand keeps the
/// hand at 14 (North removed → rinshan drawn) with an empty river and
/// empty `melds` (the North goes to the kita pool, not `melds`). But
/// after a kita the rinshan sits as a *separated* tsumohai on the far
/// right — the layout is no longer the continuous 14-tile deal. So a
/// non-empty kita pool must fall through to the normal tsumohai-offset
/// path, otherwise every closed tile sorting after the rinshan gets
/// clicked one slot too far right.
fn is_dealer_opening(ctx: &ActionContext) -> bool {
    let Some(snapshot) = ctx.snapshot else {
        return false;
    };
    let our_seat = ctx.our_seat as usize;
    let Some(player) = snapshot.players.get(our_seat) else {
        return false;
    };
    snapshot.oya == ctx.our_seat
        && player.tehai.len() == 14
        && player.river.is_empty()
        && player.melds.is_empty()
        && player.kita_tiles.is_empty()
}

/// Bind chi/pon, including a single choice, to the rendered action button.
/// A multi-choice row is read only after it appears; the common controller
/// owns reveal/commit rather than advancing on an unconfirmed first click.
fn plan_meld(op: MajsoulOpType, at: ActionType, result: &mut TargetMapping, ctx: &ActionContext) {
    let Some(button) = action_button_for(op, ctx) else {
        return;
    };
    result.targets.push(MappedClick {
        x_norm: button.0,
        y_norm: button.1,
    });

    // The measured active colours are qualified for chi/pon. A daiminkan
    // has a unique consumed set and keeps the existing kan button path.
    if op == MajsoulOpType::Daiminkan {
        return;
    }

    let consumed: Vec<String> = match ctx.action {
        MjaiEvent::Chi { consumed, .. } => consumed.to_vec(),
        MjaiEvent::Pon { consumed, .. } => consumed.to_vec(),
        MjaiEvent::Daiminkan { consumed, .. } => consumed.to_vec(),
        _ => return,
    };

    let candidates = collect_candidate_consumes(ctx.legal_actions, at);
    let mut sorted_consumed = consumed;
    sorted_consumed.sort_by(|a, b| compare_pai(a, b));
    if let Some(idx) = candidates
        .iter()
        .position(|c| same_consumed(c, &sorted_consumed))
    {
        result.choice = Some(MeldChoice {
            index: idx,
            count: candidates.len(),
        });
    } else {
        result.targets.clear();
    }
}

/// Plan an ankan/kakan: kan button click + optional kan-row candidate.
///
/// Special case: when both ankan and kakan are simultaneously legal,
/// Majsoul shows ONE kan button whose candidate row contains the union
/// of both, ordered `[kakan…, ankan…]`. The candidate index for the
/// bot's chosen tile is computed against the unified list.
fn plan_kan(op: MajsoulOpType, at: ActionType, result: &mut TargetMapping, ctx: &ActionContext) {
    let Some(button) = action_button_for(op, ctx) else {
        return;
    };
    result.targets.push(MappedClick {
        x_norm: button.0,
        y_norm: button.1,
    });

    // Collect both ankan and kakan candidates. When the bot is doing
    // a kakan, kakan candidates are listed first; for ankan, ankan
    // first. Reference: `autoplay_majsoul.py:262-280`.
    let kakans = collect_candidate_consumes(ctx.legal_actions, ActionType::Kakan);
    let ankans = collect_candidate_consumes(ctx.legal_actions, ActionType::Ankan);
    let unified: Vec<Vec<String>> = kakans.iter().chain(ankans.iter()).cloned().collect();
    if unified.len() <= 1 {
        return; // single option → Majsoul auto-confirms
    }

    // Identify the consumed tile of the bot's action.
    let consumed_pai = match ctx.action {
        MjaiEvent::Ankan { consumed, .. } => consumed.first().cloned(),
        MjaiEvent::Kakan { pai, .. } => Some(pai.clone()),
        _ => None,
    };
    let Some(consumed_pai) = consumed_pai else {
        return;
    };
    // Strip the red-five marker for matching (kan candidate row uses the
    // base tile name; the engine's consume_tiles include the red-five).
    let base = if consumed_pai.ends_with('r') {
        consumed_pai[..consumed_pai.len() - 1].to_string()
    } else {
        consumed_pai
    };

    // Find the candidate index by matching on the first non-red-five
    // tile of each candidate's consume list. For ankan all four are
    // copies of the same suit/rank; for kakan the consume is a triplet
    // of the same tile.
    let idx = unified.iter().position(|c| {
        let any = c
            .iter()
            .map(|t| {
                if t.ends_with('r') {
                    &t[..t.len() - 1]
                } else {
                    t.as_str()
                }
            })
            .next();
        any.map(|t| t == base).unwrap_or(false)
    });
    let Some(idx) = idx else {
        return;
    };

    if let Some(p) = kan_candidate_pos(idx, unified.len()) {
        // Suppress unused-variable warning when the action type is unused
        // — kept in the signature so future logic can branch on it.
        let _ = at;
        result.targets.push(MappedClick {
            x_norm: p.0,
            y_norm: p.1,
        });
    }
}

/// Look up the on-screen position of the action button for `op`, given
/// the currently-legal actions.
fn action_button_for(op: MajsoulOpType, ctx: &ActionContext) -> Option<(f64, f64)> {
    let ops = legal_op_set(ctx.legal_actions, ctx.snapshot?, ctx.our_seat);
    action_button_pos(&ops, op)
}

/// Build the deduplicated set of Majsoul op-types currently legal,
/// always including [`MajsoulOpType::None`] (the "pass / cancel"
/// button is always shown when any decision is required).
fn legal_op_set(
    legal: &[Action],
    snapshot: &crate::game_state::snapshot::GameStateSnapshot,
    our_seat: u8,
) -> Vec<MajsoulOpType> {
    use std::collections::HashSet;
    let mut set: HashSet<MajsoulOpType> = HashSet::new();

    // Pass is always present alongside any prompt.
    set.insert(MajsoulOpType::None);

    let mut hora_seen_tsumo = false;
    let mut hora_seen_ron = false;

    for a in legal {
        match a.action_type {
            ActionType::Discard => { /* no button */ }
            ActionType::Tsumo => {
                hora_seen_tsumo = true;
                set.insert(MajsoulOpType::Zimo);
            }
            ActionType::Ron => {
                hora_seen_ron = true;
                set.insert(MajsoulOpType::Ron);
            }
            other => {
                if let Some(op) = MajsoulOpType::from_engine(other) {
                    if op != MajsoulOpType::None {
                        set.insert(op);
                    }
                }
            }
        }
    }

    // 3p Nukidora isn't always exposed via legal_actions in the engine
    // (the Python reference checks tehai_vec34 for an N tile directly).
    // Mirror that: if we have N tiles in hand and the kita meld is
    // legal in the rules path, surface the button. Conservative: only
    // add when not already set and we're playing 3p.
    if snapshot.num_players == 3 {
        if let Some(player) = snapshot.players.get(our_seat as usize) {
            if player.tehai.iter().any(|t| t == "N") {
                set.insert(MajsoulOpType::Nukidora);
            }
        }
    }

    let _ = (hora_seen_tsumo, hora_seen_ron);
    let mut v: Vec<MajsoulOpType> = set.into_iter().collect();
    v.sort_by_key(|op| ACTION_PRIORITY[*op as usize]);
    v
}

/// Pull all consume-tile combinations for one action type out of the
/// legal-action list, normalised to mjai tile strings and reduced to
/// what Majsoul actually renders. The engine enumerates one action per
/// physical tile copy — a hand holding two identical 6s yields two
/// `[4s, 6s]` chi entries — while the on-screen candidate row is
/// deduplicated by tile kind. The row is laid out left-to-right in
/// ascending tile order with a red five directly left of its normal
/// five, so after sorting and deduping, an index into the returned
/// list is the on-screen slot index.
fn collect_candidate_consumes(legal: &[Action], at: ActionType) -> Vec<Vec<String>> {
    let mut candidates: Vec<Vec<String>> = legal
        .iter()
        .filter(|a| a.action_type == at)
        .map(|a| {
            let mut tiles: Vec<String> = a.consume_tiles.iter().copied().map(tid_to_mjai).collect();
            tiles.sort_by(|a, b| compare_pai(a, b));
            tiles
        })
        .collect();
    candidates.sort_by(|a, b| compare_consumed(a, b));
    candidates.dedup();
    candidates
}

/// Lexicographic order over consumed-tile lists in canonical mjai tile
/// order (`compare_pai`: red five sorts before its normal five), ties
/// broken by length. Matches Majsoul's left-to-right candidate order.
fn compare_consumed(a: &[String], b: &[String]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = compare_pai(x, y);
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// Equality on consumed-tile lists. Both sides expected pre-sorted with
/// `compare_pai`, but use a length-aware element check so the comparison
/// doesn't silently succeed on mismatched lengths.
fn same_consumed(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
}

/// True when the bot's hora is on its own draw — in mjai both tsumo
/// agari and ron are emitted as `MjaiEvent::Hora`, but Majsoul's button
/// position differs. Distinguish by consulting the engine's legal
/// actions: if Tsumo is legal for our seat, the agari is on our draw.
fn hora_is_tsumo(ctx: &ActionContext) -> bool {
    ctx.legal_actions
        .iter()
        .any(|a| a.action_type == ActionType::Tsumo)
}

/// True iff Majsoul is currently showing the Pass button — that is, we
/// are in `Phase::WaitResponse` for our seat and have at least one
/// claim option (or just the bare Pass entry that riichienv always
/// emits in WaitResponse).
fn pass_button_visible(ctx: &ActionContext) -> bool {
    ctx.legal_actions
        .iter()
        .any(|a| a.action_type == ActionType::Pass)
}

/// True when our own riichi draw has an operation prompt open — the
/// engine offers something beyond the forced tsumogiri. While such a
/// prompt is showing, Majsoul does NOT auto-discard; it waits for an
/// answer, so a bot decision to tsumogiri must be executed by clicking
/// the X (decline), never by doing nothing.
///
/// `dahai_pai` is the tile the bot wants to tsumogiri — during riichi
/// this is always the drawn tile. It gates the kita case: the engine
/// generates a Kita action for *every* North in hand, including one
/// locked into the riichi'd hand's structure where Majsoul shows no
/// prompt (riichi only allows pulling the just-drawn North). Ankan and
/// tsumo need no such gate — the engine only emits them for the drawn
/// tile.
fn riichi_prompt_pending(ctx: &ActionContext, dahai_pai: &str) -> bool {
    ctx.legal_actions.iter().any(|a| match a.action_type {
        ActionType::Kita => dahai_pai == "N",
        ActionType::Ankan | ActionType::Tsumo => true,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_unity_table_qualifies_the_actual_sorting_glyph() {
        use crate::autoplay::pixels::{self, Glyph};
        let image = pixels::decode_png(include_bytes!(
            "../../../tests/fixtures/majsoul-unity-table-20261027.png"
        ))
        .unwrap();
        let rect = CanvasRect {
            x: 7.0,
            y: 0.0,
            width: 1762.0,
            height: 1000.0,
        };
        assert_eq!(
            pixels::toggle_in(&image, 1.0, rect, coords::AUTO_SORT),
            Glyph::Lit
        );
    }
    #[test]
    fn semantic_reach_has_no_site_timing_and_only_the_discard_finishes_input() {
        let snapshot = snapshot_with_oya(
            0,
            1,
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        let action = MjaiEvent::Reach {
            actor: 0,
            pai: Some("5p".into()),
            tsumogiri: Some(true),
        };
        let legal = [Action::new(ActionType::Riichi, None, vec![], Some(0))];
        let context = ctx_for(&action, &snapshot, &legal, Some("1m"), Some("5p"), false);
        let adapter = MajsoulAutoplay::new(Arc::new(AutoplayContext::new()));
        let plan = adapter.plan(&context);
        assert_eq!(
            plan.presses
                .iter()
                .map(|p| (p.role, p.confirmation))
                .collect::<Vec<_>>(),
            vec![
                (PressRole::Declaration, Confirmation::Dispatch),
                (PressRole::Discard, Confirmation::ClientCommand)
            ]
        );
    }
    #[test]
    fn a_measured_meld_face_is_the_hit_region_and_its_interior_the_varied_aim() {
        let identity = TargetIdentity {
            window: "current".into(),
            hand: vec!["4m".into(), "5mr".into()],
            control: "candidate:0:3".into(),
        };
        let viewport = CanvasRect {
            x: 0.,
            y: 0.,
            width: 1280.,
            height: 720.,
        };
        let face = CanvasRect {
            x: 400.,
            y: 460.,
            width: 55.,
            height: 85.,
        };
        let target = meld_target(identity, viewport, face);
        assert_eq!(target.hit, face);
        let mut rng = rand::rng();
        let points: Vec<_> = (0..200)
            .map(|_| crate::autoplay::pointer::aim::target(target.bounds, None, &mut rng))
            .collect();
        assert!(points.iter().all(|p| face.contains(p.0, p.1)));
        assert!(points.windows(2).any(|p| (p[0].0 - p[1].0).abs() > 1.));
        // Hover lifts the face; a point aimed before it is still on it.
        let hovered = meld_target(
            target.identity.clone(),
            viewport,
            CanvasRect {
                y: face.y - 10.,
                ..face
            },
        );
        assert!(points.iter().all(|p| hovered.hit.contains(p.0, p.1)));
    }
    #[test]
    fn actual_hand_layout_keeps_red_and_drawn_identity_in_viewport_css_pixels() {
        let mut snapshot = snapshot_with_oya(0, 1, vec!["5mr", "5m"]);
        snapshot.players[0].drawn_tile = Some("5m".into());
        let context = ctx_for(
            &MjaiEvent::None,
            &snapshot,
            &[],
            Some("1m"),
            Some("5m"),
            false,
        );
        let mut view = ClientView {
            viewport: CanvasRect {
                x: 100.0,
                y: 50.0,
                width: 1280.0,
                height: 720.0,
            },
            clear: true,
        };
        let actual = layout(&view, &context).unwrap();
        assert_eq!(actual.tiles[0].tile, "5mr");
        assert!(!actual.tiles[0].drawn);
        assert_eq!(actual.tiles[1].tile, "5m");
        assert!(actual.tiles[1].drawn);
        let expected = tile_bounds(view.viewport, get_pai_coord(13, 1));
        assert_eq!(actual.tiles[1].bounds, expected);
        view.clear = false;
        assert!(layout(&view, &context).is_none());
    }

    /// Each kind of target has its own reach, and the hand's drawn tile,
    /// set apart from the rest, is a tile like any other.
    #[test]
    fn every_kind_of_target_has_its_own_reach() {
        let tile = reach_of(TILES[3]);
        assert_eq!((tile.half_x, tile.half_y), (0.24, 0.35));
        let drawn = reach_of((TILES[12].0 + coords::TSUMO_SPACE + 0.790_625, TILES[12].1));
        assert_eq!(drawn, tile);
        let button = reach_of(coords::ACTIONS[1]);
        assert_eq!((button.half_x, button.half_y), (0.5, 0.2));
        let candidate = reach_of(coords::CANDIDATES_KAN[2]);
        assert_eq!((candidate.half_x, candidate.half_y), (0.22, 0.3));
        let unknown = reach_of((1.0, 1.0));
        assert_eq!((unknown.half_x, unknown.half_y), (0.12, 0.12));
        // Half a tile slot across: a press within the reach stays on its tile.
        assert!(tile.half_x < (TILES[1].0 - TILES[0].0) / 2.0);
        // The game's end screens: a press on "play again" stays clear of the
        // yellow confirm beside it.
        let again = reach_of(coords::REMATCH);
        assert_eq!(reach_of(coords::GAME_CONFIRM), again);
        assert!(coords::REMATCH.0 + again.half_x < coords::GAME_CONFIRM.0 - again.half_x);
        assert!(
            coords::REMATCH.0 + again.half_x < 13.21
                && coords::GAME_CONFIRM.0 - again.half_x > 13.49
        );
        // The question's confirm: a press stays clear of the cancel beside it.
        let ask = reach_of(coords::REMATCH_ASK_CONFIRM);
        assert!(coords::REMATCH_ASK_CONFIRM.0 + ask.half_x < 8.16);
        assert!(coords::REMATCH_ASK_CONFIRM.0 - ask.half_x > 5.18);
    }
    use crate::autoplay::context::CanvasRect;

    use crate::game_state::snapshot::{GameStateSnapshot, Phase, PlayerSnapshot};
    use coords::ACTIONS;

    fn snapshot_with_oya(seat: u8, oya: u8, tehai: Vec<&str>) -> GameStateSnapshot {
        let mut s = snapshot_with_hand(seat, tehai);
        s.oya = oya;
        s
    }

    fn snapshot_with_hand(seat: u8, tehai: Vec<&str>) -> GameStateSnapshot {
        let players = (0..4u8)
            .map(|i| PlayerSnapshot {
                seat: i,
                tehai: if i == seat {
                    tehai.iter().map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                },
                melds: Vec::new(),
                river: Vec::new(),
                score: 25000,
                riichi_declared: false,
                riichi_stage: false,
                double_riichi: false,
                riichi_declaration_index: None,
                kita_tiles: Vec::new(),
                drawn_tile: None,
            })
            .collect();
        GameStateSnapshot {
            bakaze: "E".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            current_player: seat,
            turn_count: 0,
            phase: Phase::WaitAct,
            is_done: false,
            num_players: 4,
            players,
            dora_markers: Vec::new(),
            our_seat: Some(seat),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ctx_for<'a>(
        action: &'a MjaiEvent,
        snapshot: &'a GameStateSnapshot,
        legal: &'a [Action],
        last_kawa: Option<&'a str>,
        last_tsumo: Option<&'a str>,
        riichi_accepted: bool,
    ) -> ActionContext<'a> {
        ActionContext {
            action,
            snapshot: Some(snapshot),
            legal_actions: legal,
            our_seat: snapshot.our_seat.unwrap_or(0),
            last_kawa_tile: last_kawa,
            last_self_tsumo: last_tsumo,
            self_riichi_accepted: riichi_accepted,
            num_players: snapshot.num_players,

            pointer: None,
        }
    }

    #[test]
    fn dahai_simple_click() {
        // Non-dealer (oya = seat 1, we are seat 0) so the dealer-first-
        // discard layout doesn't apply — this test exercises the normal
        // tsumohai-offset path.
        let snap = snapshot_with_oya(
            0,
            1,
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        };

        let ctx = ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false);
        let result = mapped_targets(&ctx);
        // one click target
        assert_eq!(result.targets.len(), 1);
        let MappedClick { x_norm, .. } = &result.targets[0];
        // Tsumohai (5p) → idx 13, with TSUMO_SPACE offset.
        let expected = TILES[13].0 + TSUMO_SPACE;
        assert!(
            (*x_norm - expected).abs() < 1e-9,
            "expected tsumohai at {expected}, got {x_norm}"
        );
    }

    /// The click of a discard planned with a fixed source of chance.
    fn dahai_x(pai: &str, whence: Whence, ctx: &ActionContext, seed: u64) -> f64 {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        match plan_dahai_click_with(pai, whence, ctx, &mut rng) {
            Some(MappedClick { x_norm, .. }) => x_norm,
            other => panic!("expected a click, got {other:?}"),
        }
    }

    /// A non-dealer hand of 14 holding two 5p besides the drawn 5p, and two 1m.
    fn doubled_hand() -> GameStateSnapshot {
        snapshot_with_oya(
            0,
            1,
            vec![
                "1m", "1m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "5p", "5p", "7p", "5p",
            ],
        )
    }

    #[test]
    fn a_tedashi_of_the_drawn_tile_presses_a_copy_in_the_hand() {
        let snap = doubled_hand();
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        };

        let ctx = ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false);
        // Left of the gap the rack is 1m 1m 3m 4m 5m 6m 7m 8m 9m 1p 5p 5p 7p:
        // the hand's 5p are at 10 and 11, never the drawn one at 13.
        let hand = [get_pai_coord(10, 13).0, get_pai_coord(11, 13).0];
        let drawn = get_pai_coord(13, 13).0;
        for seed in 0..32 {
            let x = dahai_x("5p", Whence::Hand, &ctx, seed);
            assert!(
                hand.iter().any(|h| (x - h).abs() < 1e-9),
                "seed {seed}: {x} is not a hand 5p"
            );
            assert!((x - drawn).abs() > 1e-9);
        }
    }

    #[test]
    fn a_tsumogiri_presses_the_drawn_tile_even_with_copies_in_the_hand() {
        let snap = doubled_hand();
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: true,
        };

        let ctx = ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false);
        let drawn = TILES[13].0 + TSUMO_SPACE;
        for seed in 0..8 {
            assert!((dahai_x("5p", Whence::Drawn, &ctx, seed) - drawn).abs() < 1e-9);
        }
        // The plan reads the choice off the event itself.
        let MappedClick { x_norm, .. } = &mapped_targets(&ctx).targets[0];
        assert!((*x_norm - drawn).abs() < 1e-9)
    }

    #[test]
    fn a_hand_at_rest_puts_a_tile_at_one_of_its_copies_and_the_draw_beyond_it() {
        use rand::SeedableRng;
        let hand: Vec<String> = [
            "1m", "1m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "5p", "7p", "7p",
        ]
        .iter()
        .map(|t| t.to_string())
        .collect();
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        let places: Vec<f64> = (0..40)
            .map(|_| hand_point("7p", &hand, None, &mut rng).unwrap().0)
            .collect();
        let (a, b) = (get_pai_coord(11, 13).0, get_pai_coord(12, 13).0);
        assert!(places
            .iter()
            .all(|x| (x - a).abs() < 1e-9 || (x - b).abs() < 1e-9));
        assert!(
            places.iter().any(|x| (x - a).abs() < 1e-9)
                && places.iter().any(|x| (x - b).abs() < 1e-9)
        );
        assert_eq!(hand_point("E", &hand, None, &mut rng), None);
        assert!((draw_point(13).0 - (TILES[13].0 + TSUMO_SPACE)).abs() < 1e-9);
    }

    #[test]
    fn a_tedashi_with_no_other_copy_presses_the_drawn_tile() {
        let snap = doubled_hand();
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "3m".into(),
            tsumogiri: false,
        };

        // Drawn 3m, the only one: the drawn tile is the only copy to press.
        let ctx = ctx_for(&act, &snap, &[], Some("1m"), Some("3m"), false);
        let drawn = get_pai_coord(13, 13).0;
        assert!((dahai_x("3m", Whence::Hand, &ctx, 0) - drawn).abs() < 1e-9);
    }

    #[test]
    fn identical_copies_are_pressed_in_turn_not_always_the_first() {
        let snap = doubled_hand();
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "1m".into(),
            tsumogiri: false,
        };

        let ctx = ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false);
        let places = [get_pai_coord(0, 13).0, get_pai_coord(1, 13).0];
        let mut seen = [false, false];
        for seed in 0..64 {
            let x = dahai_x("1m", Whence::Hand, &ctx, seed);
            let at = places.iter().position(|p| (x - p).abs() < 1e-9);
            seen[at.unwrap_or_else(|| panic!("seed {seed}: {x} is not a 1m"))] = true;
        }
        assert_eq!(seen, [true, true]);
    }

    /// A point on the tile at `idx` of a rack of `count`, a little off its
    /// middle as a resting hand would be.
    fn on(idx: usize, count: usize) -> Option<(f64, f64)> {
        let (x, y) = get_pai_coord(idx, count);
        Some((x + 0.18, y - 0.27))
    }

    #[test]
    fn a_discard_presses_the_copy_the_pointer_is_already_on() {
        let snap = doubled_hand();

        for (pai, places) in [("1m", [0, 1]), ("5p", [10, 11])] {
            let act = MjaiEvent::Dahai {
                actor: 0,
                pai: pai.into(),
                tsumogiri: false,
            };
            for at in places {
                let ctx = ActionContext {
                    pointer: on(at, 13),
                    ..ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false)
                };
                let want = get_pai_coord(at, 13).0;
                for seed in 0..32 {
                    let x = dahai_x(pai, Whence::Hand, &ctx, seed);
                    assert!(
                        (x - want).abs() < 1e-9,
                        "{pai} seed {seed}: {x}, not {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_tsumogiri_presses_the_drawn_tile_whichever_copy_the_pointer_is_on() {
        let snap = doubled_hand();
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: true,
        };

        let ctx = ActionContext {
            pointer: on(10, 13),
            ..ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false)
        };
        let drawn = get_pai_coord(13, 13).0;
        assert!((dahai_x("5p", Whence::Drawn, &ctx, 0) - drawn).abs() < 1e-9);
    }

    #[test]
    fn a_pointer_off_the_hand_leaves_the_copy_to_chance() {
        let snap = doubled_hand();
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "1m".into(),
            tsumogiri: false,
        };

        // By the buttons, where the pointer rests between turns.
        let ctx = ActionContext {
            pointer: Some(ACTIONS[0]),
            ..ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), false)
        };
        let places = [get_pai_coord(0, 13).0, get_pai_coord(1, 13).0];
        let mut seen = [false, false];
        for seed in 0..64 {
            let x = dahai_x("1m", Whence::Hand, &ctx, seed);
            let at = places.iter().position(|p| (x - p).abs() < 1e-9);
            seen[at.unwrap_or_else(|| panic!("seed {seed}: {x} is not a 1m"))] = true;
        }
        assert_eq!(seen, [true, true]);
    }

    #[test]
    fn a_hand_at_rest_keeps_to_the_copy_it_is_looking_at() {
        use rand::SeedableRng;
        let hand: Vec<String> = [
            "1m", "1m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "5p", "7p", "7p",
        ]
        .iter()
        .map(|t| t.to_string())
        .collect();
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        for at in [11, 12] {
            let want = get_pai_coord(at, 13);
            for _ in 0..20 {
                assert_eq!(hand_point("7p", &hand, on(at, 13), &mut rng), Some(want));
            }
            assert!(on_tile(want, on(at, 13)));
        }
        // On one 7p is not on the other.
        assert!(!on_tile(get_pai_coord(12, 13), on(11, 13)));
        assert!(!on_tile(get_pai_coord(11, 13), None));
    }

    #[test]
    fn dahai_suppressed_under_riichi() {
        let snap = snapshot_with_hand(
            0,
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: true,
        };

        let ctx = ctx_for(&act, &snap, &[], Some("1m"), Some("5p"), true);
        let result = mapped_targets(&ctx);
        assert!(result.targets.is_empty(), "no click while riichi accepted");
    }

    /// Regression (2026-08-22, West 1): a bot `hora` on a robbed kan must
    /// click the Ron button. The engine's legal set used to be empty after
    /// an opponent kakan, so the button never resolved and the window hung
    /// until a human pressed Ron.
    #[test]
    fn chankan_hora_clicks_the_ron_button() {
        let mut snap = snapshot_with_hand(
            0,
            vec![
                "2m", "3m", "4m", "5m", "6m", "7m", "2p", "3p", "4p", "5p", "6p", "7p", "5s",
            ],
        );
        snap.phase = Phase::WaitResponse;
        let act = MjaiEvent::Hora {
            score: None,
            actor: 0,
            target: 1,
            deltas: None,
            ura_markers: None,
            pai: None,
            tehai: None,
            scores: None,
        };
        // What the tracker now offers on a robable kakan: the ron plus the
        // always-present pass.
        let legal = vec![
            Action::new(ActionType::Ron, Some(88), vec![], Some(0)), // 5s
            Action::new(ActionType::Pass, None, vec![], Some(0)),
        ];

        let ctx = ctx_for(&act, &snap, &legal, Some("4z"), None, false);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1, "ron target, got {result:?}");
        let MappedClick { x_norm, y_norm } = &result.targets[0];
        // [Ron, Pass] sorted by priority → Ron in slot 1, one left
        // of the always-rightmost pass.
        assert_eq!(*x_norm, 8.637_5);
        assert_eq!(*y_norm, 7.0);
    }

    /// The decline side of the same window: a bot `none` on a robbed kan
    /// must click the pass button rather than leave the client hanging.
    #[test]
    fn chankan_decline_clicks_the_pass_button() {
        let mut snap = snapshot_with_hand(
            0,
            vec![
                "2m", "3m", "4m", "5m", "6m", "7m", "2p", "3p", "4p", "5p", "6p", "7p", "5s",
            ],
        );
        snap.phase = Phase::WaitResponse;
        let act = MjaiEvent::None;
        let legal = vec![
            Action::new(ActionType::Ron, Some(88), vec![], Some(0)), // 5s
            Action::new(ActionType::Pass, None, vec![], Some(0)),
        ];

        let ctx = ctx_for(&act, &snap, &legal, Some("4z"), None, false);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1, "pass target, got {result:?}");
        let MappedClick { x_norm, y_norm } = &result.targets[0];
        assert_eq!(*x_norm, 10.875);
        assert_eq!(*y_norm, 7.0);
    }

    /// Regression: in sanma, drawing a North while in riichi opens the
    /// kita prompt and Majsoul holds the auto-discard until it is
    /// answered. The bot deciding to tsumogiri the North must decline
    /// the prompt via the X button — the old unconditional suppression
    /// returned no steps and the game hung until the turn timer and the
    /// entire time bank drained.
    #[test]
    fn dahai_of_drawn_north_under_riichi_declines_kita_prompt() {
        let snap = snapshot_3p_with_kita(
            0,
            1,
            vec![
                "1m", "2m", "3m", "4p", "5p", "6p", "7s", "8s", "9s", "5s", "5s", "E", "E", "N",
            ],
            vec![],
            Some("N"),
        );
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "N".into(),
            tsumogiri: true,
        };

        // What the engine offers on a drawn North during riichi: the
        // forced tsumogiri plus the kita (N = tile ids 120..=123).
        let legal = vec![
            Action::new(ActionType::Discard, Some(120), vec![], Some(0)),
            Action::new(ActionType::Kita, Some(120), vec![], Some(0)),
        ];
        let ctx = ctx_for(&act, &snap, &legal, Some("1z"), Some("N"), true);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1, "pass target, got {result:?}");
        let MappedClick { x_norm, y_norm } = &result.targets[0];
        // The X (pass / cancel) is always the rightmost button —
        // slot 0 — regardless of what else is on the row.
        assert_eq!(*x_norm, 10.875);
        assert_eq!(*y_norm, 7.0);
    }

    /// A structural North locked inside a riichi'd hand makes the engine
    /// offer Kita on every subsequent draw, but Majsoul only prompts
    /// when the drawn tile itself is the North — an ordinary tsumogiri
    /// must stay suppressed (a ghost X click would land on empty UI).
    #[test]
    fn dahai_under_riichi_with_hand_north_but_other_draw_stays_suppressed() {
        let snap = snapshot_3p_with_kita(
            0,
            1,
            vec![
                "1m", "2m", "3m", "4p", "5p", "6p", "7s", "8s", "9s", "5s", "5s", "N", "N", "5p",
            ],
            vec![],
            Some("5p"),
        );
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: true,
        };

        let legal = vec![
            Action::new(ActionType::Discard, Some(53), vec![], Some(0)),
            Action::new(ActionType::Kita, Some(120), vec![], Some(0)),
        ];
        let ctx = ctx_for(&act, &snap, &legal, Some("1z"), Some("5p"), true);
        let result = mapped_targets(&ctx);
        assert!(
            result.targets.is_empty(),
            "no prompt on screen — stay suppressed, got {result:?}"
        );
    }

    /// The same trap with a riichi-legal ankan (3p and 4p alike): the
    /// kan prompt holds the auto-discard, so a bot decision to keep the
    /// hand closed must decline via the X button.
    #[test]
    fn dahai_under_riichi_with_ankan_prompt_declines_via_x() {
        let snap = snapshot_with_oya(
            0,
            1,
            vec![
                "1m", "1m", "1m", "4p", "5p", "6p", "7s", "8s", "9s", "5s", "5s", "E", "E", "1m",
            ],
        );
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "1m".into(),
            tsumogiri: true,
        };

        let legal = vec![
            Action::new(ActionType::Discard, Some(3), vec![], Some(0)),
            Action::new(ActionType::Ankan, Some(0), vec![0, 1, 2, 3], Some(0)),
        ];
        let ctx = ctx_for(&act, &snap, &legal, Some("1z"), Some("1m"), true);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1, "pass target, got {result:?}");
        let MappedClick { x_norm, y_norm } = &result.targets[0];
        assert_eq!(*x_norm, 10.875);
        assert_eq!(*y_norm, 7.0);
    }

    #[test]
    fn reach_path_a_two_clicks() {
        let snap = snapshot_with_hand(
            0,
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        let act = MjaiEvent::Reach {
            actor: 0,
            pai: Some("5p".into()),
            tsumogiri: None,
        };

        // Reach must be in legal_actions for the button position to resolve.
        let legal = vec![Action::new(ActionType::Riichi, None, vec![], Some(0))];
        let ctx = ctx_for(&act, &snap, &legal, Some("1m"), Some("5p"), false);
        let result = mapped_targets(&ctx);
        // declaration and discard targets
        assert_eq!(result.targets.len(), 2);
        assert!(matches!(result.targets[0], MappedClick { .. }));
        assert!(matches!(result.targets[1], MappedClick { .. }));
    }

    #[test]
    fn a_reach_presses_the_copy_it_names() {
        let snap = doubled_hand();
        let legal = vec![Action::new(ActionType::Riichi, None, vec![], Some(0))];

        let pressed = |tsumogiri: Option<bool>| {
            let act = MjaiEvent::Reach {
                actor: 0,
                pai: Some("5p".into()),
                tsumogiri,
            };
            let ctx = ctx_for(&act, &snap, &legal, Some("1m"), Some("5p"), false);
            let MappedClick { x_norm, .. } = mapped_targets(&ctx).targets[1];
            x_norm
        };
        let drawn = TILES[13].0 + TSUMO_SPACE;
        assert!((pressed(Some(true)) - drawn).abs() < 1e-9);
        let hand = [get_pai_coord(10, 13).0, get_pai_coord(11, 13).0];
        let x = pressed(Some(false));
        assert!(
            hand.iter().any(|h| (x - h).abs() < 1e-9),
            "{x} is not a hand 5p"
        );
        // A bot that does not say which keeps the drawn one.
        assert!((pressed(None) - drawn).abs() < 1e-9);
    }

    /// A reach that still names no tile (the manager's follow-up failed to
    /// resolve it, #257) declares nothing — pressing the button alone would
    /// leave the client owing a discard until timeout.
    #[test]
    fn reach_without_pai_declines() {
        let snap = snapshot_with_hand(0, vec!["1m"]);
        let act = MjaiEvent::Reach {
            actor: 0,
            pai: None,
            tsumogiri: None,
        };

        let legal = vec![Action::new(ActionType::Riichi, None, vec![], Some(0))];
        let ctx = ctx_for(&act, &snap, &legal, Some("1m"), None, false);
        let result = mapped_targets(&ctx);
        assert!(
            result.targets.is_empty(),
            "a tile-less reach must not press anything"
        );
    }

    #[test]
    fn pass_button_clicks_at_slot_zero() {
        // Pass is in legal_actions only when riichienv is in WaitResponse
        // and there's an actual claim option. Synthesise that state:
        let mut snap = snapshot_with_hand(0, vec!["1m"]);
        snap.phase = Phase::WaitResponse;
        let act = MjaiEvent::None;

        let legal = vec![
            Action::new(ActionType::Pass, None, vec![], Some(0)),
            Action::new(ActionType::Pon, None, vec![], Some(0)),
        ];
        let ctx = ctx_for(&act, &snap, &legal, Some("1m"), None, false);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1);
        let MappedClick { x_norm, y_norm } = &result.targets[0];
        // ACTIONS[0] is the pass slot (rightmost top row).
        assert_eq!(*x_norm, 10.875);
        assert_eq!(*y_norm, 7.0);
    }

    #[test]
    fn none_does_not_click_when_no_pass_in_legal_actions() {
        // Bot emits None on every event from other players (purely
        // informational echoes). Without the gate we'd loop-click the
        // cancel button on every other-player turn.
        let snap = snapshot_with_hand(0, vec!["1m"]);
        let act = MjaiEvent::None;

        let ctx = ctx_for(&act, &snap, &[], Some("1m"), None, false);
        let result = mapped_targets(&ctx);
        assert!(
            result.targets.is_empty(),
            "None must not click when Pass is not in legal_actions"
        );
    }

    #[test]
    fn none_does_not_click_during_our_discard_turn() {
        // WaitAct phase: legal_actions has Discard but no Pass — a bot
        // emitting None here is buggy, but the gate must hold.
        let snap = snapshot_with_hand(0, vec!["1m", "2m"]);
        let act = MjaiEvent::None;

        let legal = vec![
            Action::new(ActionType::Discard, Some(0), vec![], Some(0)),
            Action::new(ActionType::Discard, Some(4), vec![], Some(0)),
        ];
        let ctx = ctx_for(&act, &snap, &legal, Some("1m"), Some("1m"), false);
        let result = mapped_targets(&ctx);
        assert!(
            result.targets.is_empty(),
            "None must not click during a discard turn (no Pass button shown)"
        );
    }

    #[test]
    fn dealer_first_discard_uses_continuous_layout_no_tsumo_offset() {
        // Dealer with 14 tiles, empty river, no melds. Mahjong Soul lays
        // all 14 sorted on the rack — no tsumohai gap. Discarding the
        // sorted-last tile (5p) must click TILES[13] directly, NOT
        // TILES[13] + TSUMO_SPACE.
        let snap = snapshot_with_oya(
            0,
            0,
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        };

        // Even with last_self_tsumo set, dealer-first-discard layout
        // must override the tsumohai-offset path.
        let ctx = ctx_for(&act, &snap, &[], None, Some("5p"), false);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1);
        let MappedClick { x_norm, .. } = &result.targets[0];
        assert!(
            (*x_norm - TILES[13].0).abs() < 1e-9,
            "expected raw TILES[13] (no TSUMO_SPACE), got {x_norm}"
        );
    }

    /// Whatever the dealer does first waits out the deal, not only a
    /// discard — a double riichi, a concealed kan, a win off the deal, nine
    /// terminals — and its plan says the window opened with a deal. Past
    /// the dealer's first turn, or not the dealer, neither.
    #[test]
    fn dealer_opening_is_reported_for_every_first_action() {
        let hand = vec![
            "1m", "1m", "1m", "1m", "9m", "1p", "9p", "1s", "9s", "E", "S", "W", "N", "P",
        ];
        let cases = [
            (
                MjaiEvent::Reach {
                    actor: 0,
                    pai: Some("9m".into()),
                    tsumogiri: None,
                },
                Action::new(ActionType::Riichi, None, vec![], Some(0)),
            ),
            (
                MjaiEvent::Ankan {
                    actor: 0,
                    consumed: ["1m".into(), "1m".into(), "1m".into(), "1m".into()],
                },
                Action::new(ActionType::Ankan, None, vec![0, 1, 2, 3], Some(0)),
            ),
            (
                MjaiEvent::Hora {
                    score: None,
                    actor: 0,
                    target: 0,
                    deltas: None,
                    ura_markers: None,
                    pai: None,
                    tehai: None,
                    scores: None,
                },
                Action::new(ActionType::Tsumo, None, vec![], Some(0)),
            ),
            (
                MjaiEvent::Ryukyoku {
                    deltas: None,
                    tehais: None,
                    scores: None,
                    reason: None,
                },
                Action::new(ActionType::KyushuKyuhai, None, vec![], Some(0)),
            ),
        ];

        let opening = snapshot_with_oya(0, 0, hand.clone());
        let mut later = opening.clone();
        later.players[0]
            .river
            .push(crate::game_state::snapshot::DiscardEntry {
                tile: "S".into(),
                tedashi: true,
                is_riichi: false,
                called: false,
            });
        let not_dealer = snapshot_with_oya(0, 1, hand);
        for (act, legal) in &cases {
            let legal = std::slice::from_ref(legal);
            let ctx = ctx_for(act, &opening, legal, None, None, false);
            let result = mapped_targets(&ctx);
            assert!(result.dealt, "{act:?}: {result:?}");

            for snap in [&later, &not_dealer] {
                let ctx = ctx_for(act, snap, legal, None, None, false);
                let result = mapped_targets(&ctx);
                assert!(!result.dealt, "{act:?}: {result:?}");
            }
        }
    }

    #[test]
    fn dealer_second_discard_uses_normal_tsumohai_path() {
        // After dealer's first discard, future turns are 13 closed + 1
        // tsumohai with the standard offset — same as non-dealer.
        let mut snap = snapshot_with_oya(
            0,
            0,
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        // Mark a prior discard so river is non-empty (not first discard).
        snap.players[0]
            .river
            .push(crate::game_state::snapshot::DiscardEntry {
                tile: "9m".into(),
                tedashi: true,
                is_riichi: false,
                called: false,
            });
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        };

        let ctx = ctx_for(&act, &snap, &[], Some("9m"), Some("5p"), false);
        let result = mapped_targets(&ctx);
        let MappedClick { x_norm, .. } = &result.targets.last().unwrap();
        // Tsumohai click → TILES[13] + TSUMO_SPACE.
        let expected = TILES[13].0 + TSUMO_SPACE;
        assert!((*x_norm - expected).abs() < 1e-9);
    }

    #[test]
    fn non_dealer_first_discard_does_not_pad_or_relayout() {
        // Non-dealer's first turn: 14 tiles too, but the layout is
        // 13 closed + 1 tsumohai with TSUMO_SPACE — Majsoul does not
        // run the dealer-only sort animation.
        let snap = snapshot_with_oya(
            1,
            0, // we're seat 1, dealer is seat 0
            vec![
                "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
            ],
        );
        let act = MjaiEvent::Dahai {
            actor: 1,
            pai: "5p".into(),
            tsumogiri: false,
        };

        let ctx = ctx_for(&act, &snap, &[], None, Some("5p"), false);
        let result = mapped_targets(&ctx);
        // No dealer pad: just [pre-delay, click].
        assert_eq!(result.targets.len(), 1);
    }

    /// Build a 3-player snapshot for our seat with optional kita pool and
    /// an explicit drawn (tsumohai) tile.
    fn snapshot_3p_with_kita(
        seat: u8,
        oya: u8,
        tehai: Vec<&str>,
        kita: Vec<&str>,
        drawn: Option<&str>,
    ) -> GameStateSnapshot {
        let players = (0..3u8)
            .map(|i| PlayerSnapshot {
                seat: i,
                tehai: if i == seat {
                    tehai.iter().map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                },
                melds: Vec::new(),
                river: Vec::new(),
                score: 35000,
                riichi_declared: false,
                riichi_stage: false,
                double_riichi: false,
                riichi_declaration_index: None,
                kita_tiles: if i == seat {
                    kita.iter().map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                },
                drawn_tile: if i == seat {
                    drawn.map(|s| s.to_string())
                } else {
                    None
                },
            })
            .collect();
        GameStateSnapshot {
            bakaze: "E".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya,
            current_player: seat,
            turn_count: 0,
            phase: Phase::WaitAct,
            is_done: false,
            num_players: 3,
            players,
            dora_markers: Vec::new(),
            our_seat: Some(seat),
        }
    }

    #[test]
    fn dealer_opening_kita_then_discard_is_not_off_by_one() {
        // 3p regression: dealer (oya == our seat) declares kita on the
        // opening hand, draws a rinshan replacement, then discards. The
        // North goes to the kita pool — NOT to `melds` and NOT to the
        // river — so `is_dealer_opening`'s naive (oya + 14 tiles +
        // empty river + empty melds) test still matched, forcing the
        // "continuous 14-tile" dealer layout. But after a kita the rinshan
        // sits as a separated tsumohai on the far right, so every closed
        // tile that sorts *after* the rinshan was clicked one slot too far
        // right. Repro: rinshan "1m" sorts first, so discarding the
        // rightmost closed tile "5p" must land on TILES[12] (its true
        // visual slot), not TILES[13] (sorted-14 index, one tile right).
        let tehai = vec![
            "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p", "1m",
        ];
        let snap = snapshot_3p_with_kita(0, 0, tehai, vec!["N"], Some("1m"));
        let act = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        };

        let ctx = ctx_for(
            &act,
            &snap,
            &[],
            None,       // opening turn: no kawa tile yet
            Some("1m"), // rinshan replacement is the live tsumohai
            false,
        );
        let result = mapped_targets(&ctx);
        let MappedClick { x_norm, .. } = result.targets.last().unwrap();
        assert!(
            (*x_norm - TILES[12].0).abs() < 1e-9,
            "post-kita discard must click the true visual slot TILES[12] \
             ({}), got {x_norm} (TILES[13]={} would be one tile too far right)",
            TILES[12].0,
            TILES[13].0,
        );
    }

    #[test]
    fn pixel_translation_in_canvas_rect() {
        let rect = CanvasRect {
            x: 0.0,
            y: 0.0,
            width: 1600.0,
            height: 900.0,
        };
        // Centre of the canvas at (8.0, 4.5) norm.
        assert_eq!(rect.pixel(8.0, 4.5), (800.0, 450.0));
    }

    // ----- Chi/pon candidate-slot regressions ---------------------------
    //
    // The engine enumerates one chi/pon action per physical tile copy,
    // but Majsoul renders a deduplicated, sorted candidate row. The
    // planner must map the bot's consume list onto the rendered row, not
    // the raw engine list (issues #138 / #235 / #239).

    /// Tile ids: suit base (m=0, p=36, s=72) + (rank-1)*4 + copy; copy 0
    /// of a five is the red five.
    fn chi_action(tile: u8, c1: u8, c2: u8) -> Action {
        Action::new(ActionType::Chi, Some(tile), vec![c1, c2], Some(0))
    }

    fn candidate_mapping(legal: &[Action], consumed: [&str; 2], pai: &str) -> TargetMapping {
        let snap = snapshot_with_hand(0, vec!["1m"]);
        let act = MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: pai.into(),
            consumed: [consumed[0].to_string(), consumed[1].to_string()],
        };

        let ctx = ctx_for(&act, &snap, legal, Some(pai), None, false);
        mapped_targets(&ctx)
    }

    #[test]
    fn chi_candidate_dedups_duplicate_copies_issue_138() {
        // Hand 3s 4s 6s 6s 7s, opponent discards 5s. The engine lists 5
        // combinations (two 6s copies duplicate [4s,6s] and [6s,7s]);
        // Majsoul renders 3 boxes. Choosing [3s,4s] must click the
        // leftmost of a 3-slot row, not slot 1 of a phantom 5-slot row
        // (which lands left of the whole row and hangs autoplay).
        let legal = [
            chi_action(89, 80, 84), // [3s,4s]
            chi_action(89, 84, 92), // [4s,6s] (first 6s)
            chi_action(89, 84, 93), // [4s,6s] (second 6s)
            chi_action(89, 92, 96), // [6s,7s] (first 6s)
            chi_action(89, 93, 96), // [6s,7s] (second 6s)
        ];
        let mapping = candidate_mapping(&legal, ["3s", "4s"], "5s");
        assert_eq!(mapping.targets.len(), 1, "no guessed candidate coordinate");
        assert_eq!(mapping.choice, Some(MeldChoice { index: 0, count: 3 }));
    }

    #[test]
    fn chi_candidate_slot_matches_screen_row_issue_235() {
        // Hand 2s 3s 4s 5s 6s 6s, opponent discards 0s. Engine lists
        // [3s,4s], [4s,6s], [4s,6s]; the screen shows two boxes. The
        // bot's [4s,6s] is the right box of a 2-slot row.
        let legal = [
            chi_action(88, 80, 84), // [3s,4s]
            chi_action(88, 84, 92), // [4s,6s] (first 6s)
            chi_action(88, 84, 93), // [4s,6s] (second 6s)
        ];
        let mapping = candidate_mapping(&legal, ["4s", "6s"], "5sr");
        assert_eq!(mapping.choice, Some(MeldChoice { index: 1, count: 2 }));
    }

    #[test]
    fn chi_single_kind_after_dedup_skips_candidate_click() {
        // Hand 4s 6s 6s, opponent discards 5s. The engine lists [4s,6s]
        // twice (two 6s copies) but only one distinct combination
        // exists, so Majsoul auto-confirms without showing a candidate
        // row — the plan must stop at the button click instead of
        // clicking a phantom two-slot row.
        let legal = [
            chi_action(89, 84, 92), // [4s,6s] (first 6s)
            chi_action(89, 84, 93), // [4s,6s] (second 6s)
        ];
        let mapping = candidate_mapping(&legal, ["4s", "6s"], "5s");
        assert_eq!(mapping.targets.len(), 1);
        assert_eq!(mapping.choice, Some(MeldChoice { index: 0, count: 1 }));
    }

    #[test]
    fn single_pon_requires_geometry_and_kan_keeps_its_qualified_button_path() {
        let snap = snapshot_with_hand(0, vec!["1m"]);
        for (action, at, consume) in [
            (
                MjaiEvent::Pon {
                    actor: 0,
                    target: 3,
                    pai: "5p".into(),
                    consumed: ["5p".into(), "5p".into()],
                },
                ActionType::Pon,
                vec![53, 54],
            ),
            (
                MjaiEvent::Daiminkan {
                    actor: 0,
                    target: 3,
                    pai: "5p".into(),
                    consumed: ["5pr".into(), "5p".into(), "5p".into()],
                },
                ActionType::Daiminkan,
                vec![52, 53, 54],
            ),
        ] {
            let legal = [Action::new(at, Some(55), consume, Some(3))];
            let ctx = ctx_for(&action, &snap, &legal, Some("5p"), None, false);
            let mapping = mapped_targets(&ctx);
            assert_eq!(mapping.targets.len(), 1);
            assert_eq!(
                mapping.choice,
                (at == ActionType::Pon).then_some(MeldChoice { index: 0, count: 1 })
            );
        }
    }

    #[test]
    fn chi_red_five_variants_stay_distinct_and_sort_left() {
        // Hand 2s 3s 5s 0s 6s, opponent discards 4s. All five rendered
        // combinations are distinct kinds — dedup must not collapse the
        // red-five variants — and each red variant sits directly left
        // of its normal counterpart:
        //   [2s,3s] [3s,5sr] [3s,5s] [5sr,6s] [5s,6s]
        let legal = [
            chi_action(85, 76, 80), // [2s,3s]
            chi_action(85, 80, 88), // [3s,5sr]
            chi_action(85, 80, 89), // [3s,5s]
            chi_action(85, 88, 92), // [5sr,6s]
            chi_action(85, 89, 92), // [5s,6s]
        ];
        let with_normal = candidate_mapping(&legal, ["3s", "5s"], "4s");
        assert_eq!(with_normal.choice, Some(MeldChoice { index: 2, count: 5 }));
        let with_red = candidate_mapping(&legal, ["3s", "5sr"], "4s");
        assert_eq!(with_red.choice, Some(MeldChoice { index: 1, count: 5 }));
    }

    #[test]
    fn pon_candidate_dedups_red_five_pairs() {
        // Hand 5p 5p 0p, opponent discards 5p. The engine enumerates 3
        // pairs from the three copies; Majsoul shows two boxes with the
        // red pair on the left. Keeping the red five ([5p,5p]) is the
        // right box.
        let legal = [
            Action::new(ActionType::Pon, Some(55), vec![53, 54], Some(0)), // [5p,5p]
            Action::new(ActionType::Pon, Some(55), vec![53, 52], Some(0)), // [5pr,5p]
            Action::new(ActionType::Pon, Some(55), vec![54, 52], Some(0)), // [5pr,5p]
        ];
        let snap = snapshot_with_hand(0, vec!["1m"]);
        let act = MjaiEvent::Pon {
            actor: 0,
            target: 3,
            pai: "5p".into(),
            consumed: ["5p".into(), "5p".into()],
        };

        let ctx = ctx_for(&act, &snap, &legal, Some("5p"), None, false);
        let result = mapped_targets(&ctx);
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.choice, Some(MeldChoice { index: 1, count: 2 }));
    }
}
