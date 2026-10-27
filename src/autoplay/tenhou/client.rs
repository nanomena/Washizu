//! Read-only Web3 input targets in viewport CSS pixels.
//! Playing input is dispatched solely by the shared pointer controller.

use crate::autoplay::context::CanvasRect;
use crate::autoplay::tenhou_state::TenhouState;
use crate::autoplay::ui::tenhou_web::Web3Observation;
use crate::schema::MjaiEvent;
use anyhow::{anyhow, Context, Result};
use chromiumoxide::page::Page;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dispatch {
    Sent,
    Changed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ClientTag {
    #[serde(rename = "web3")]
    Web3,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub identity: String,
    pub payload: Value,
    #[serde(deserialize_with = "required_nullable")]
    pub bounds: Option<CanvasRect>,
    #[serde(deserialize_with = "required_nullable")]
    pub reveal: Option<ControlGeometry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlGeometry {
    pub identity: String,
    pub bounds: CanvasRect,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TileGeometry {
    pub identity: String,
    /// Actual physical tile id, including its red/plain copy.
    pub tile: u32,
    pub drawn: bool,
    pub legal: bool,
    pub bounds: CanvasRect,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClockFacts {
    pub elapsed_ms: u32,
    pub fixed_ms: u32,
    /// Actual bank granted to this native timer at attachment, not a rule cap
    /// and not the balance after elapsed time has already consumed it.
    pub granted_bank_ms: u32,
    pub remaining_base_ms: u32,
    pub remaining_bank_ms: u32,
}

impl ClockFacts {
    /// The elapsed origin is captured on native clock attachment, never read
    /// or frame arrival. The host supplies its receipt time.
    pub fn started_at(&self, observed_at: Instant) -> Option<Instant> {
        observed_at.checked_sub(Duration::from_millis(u64::from(self.elapsed_ms)))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputSnapshot {
    #[serde(rename = "client")]
    _client: ClientTag,
    pub ready: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub window: Option<String>,
    pub hand: Vec<u32>,
    pub viewport: CanvasRect,
    #[serde(deserialize_with = "required_nullable")]
    pub clock: Option<ClockFacts>,
    pub tiles: Vec<TileGeometry>,
    pub offers: Vec<Offer>,
    #[serde(deserialize_with = "required_nullable")]
    pub next_draw: Option<CanvasRect>,
    #[serde(deserialize_with = "required_nullable")]
    pub rest: Option<CanvasRect>,
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::deserialize(de)
}

impl InputSnapshot {
    fn check(self) -> Result<Self> {
        let mut hand = self.hand.clone();
        hand.sort_unstable();
        if hand.len() > 14
            || hand.iter().any(|tile| *tile >= 136)
            || hand.windows(2).any(|pair| pair[0] == pair[1])
            || (self.ready && (hand.is_empty() || self.window.as_deref().is_none_or(str::is_empty)))
            || (!self.ready && self.window.is_some())
            || self.offers.iter().any(|offer| offer.identity.is_empty())
            || self.clock.as_ref().is_some_and(|clock| {
                clock.fixed_ms == 0
                    || clock.remaining_base_ms > clock.fixed_ms
                    || clock.remaining_bank_ms > clock.granted_bank_ms
            })
            || !valid_rect(&self.viewport)
            || self.tiles.iter().any(|tile| {
                tile.identity.is_empty()
                    || !hand.contains(&tile.tile)
                    || !inside(&self.viewport, &tile.bounds)
            })
            || self.tiles.iter().filter(|tile| tile.drawn).count() > 1
            || self.tiles.iter().enumerate().any(|(i, tile)| {
                self.tiles[..i]
                    .iter()
                    .any(|previous| previous.tile == tile.tile)
            })
            || self.offers.iter().any(|offer| {
                offer
                    .bounds
                    .as_ref()
                    .is_some_and(|bounds| !inside(&self.viewport, bounds))
                    || offer.reveal.as_ref().is_some_and(|reveal| {
                        reveal.identity.is_empty() || !inside(&self.viewport, &reveal.bounds)
                    })
            })
            || self
                .next_draw
                .as_ref()
                .is_some_and(|bounds| !inside(&self.viewport, bounds))
            || self
                .rest
                .as_ref()
                .is_some_and(|bounds| !inside(&self.viewport, bounds))
        {
            return Err(anyhow!("invalid Tenhou Web3 input snapshot"));
        }
        Ok(Self { hand, ..self })
    }

    /// Choose one offered native candidate matching the exact semantic action.
    /// Plain copies may differ physically; red/plain choices may not.
    pub fn matching_offer(&self, wanted: &Value) -> Option<&Offer> {
        let expected = normalized(wanted, &self.hand)?;
        self.offers
            .iter()
            .find(|offer| normalized(&offer.payload, &self.hand).as_ref() == Some(&expected))
    }
}

fn valid_rect(rect: &CanvasRect) -> bool {
    [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|value| value.is_finite())
        && rect.width > 0.0
        && rect.height > 0.0
}

fn inside(viewport: &CanvasRect, bounds: &CanvasRect) -> bool {
    valid_rect(bounds)
        && bounds.x >= viewport.x
        && bounds.y >= viewport.y
        && bounds.x + bounds.width <= viewport.x + viewport.width + 1e-7
        && bounds.y + bounds.height <= viewport.y + viewport.height + 1e-7
}

/// Describes an already selected action. Probing never invokes a native action.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetRequest {
    Discard {
        window: String,
        hand: Vec<u32>,
        tile: u32,
        tsumogiri: bool,
    },
    Offer {
        window: String,
        identity: String,
    },
    Hover {
        hand: Vec<u32>,
        tile: u32,
        identity: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    Ready,
    Changed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Discard,
    Offer,
    Reveal,
    Hover,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MouseTarget {
    pub identity: String,
    pub kind: TargetKind,
    pub bounds: CanvasRect,
    pub viewport: CanvasRect,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetProbe {
    #[serde(rename = "client")]
    _client: ClientTag,
    pub status: TargetStatus,
    #[serde(deserialize_with = "required_nullable")]
    pub target: Option<MouseTarget>,
}

impl TargetProbe {
    fn check(self) -> Result<Self> {
        if (self.status == TargetStatus::Ready) != self.target.is_some()
            || self.target.as_ref().is_some_and(|target| {
                target.identity.is_empty()
                    || !valid_rect(&target.viewport)
                    || !inside(&target.viewport, &target.bounds)
            })
        {
            return Err(anyhow!("invalid Tenhou Web3 target probe"));
        }
        Ok(self)
    }
}

async fn evaluate<T: DeserializeOwned>(page: &Page, expression: String) -> Result<T> {
    let response = page
        .evaluate(expression)
        .await
        .context("Tenhou Web3 capability")?;
    serde_json::from_value(
        response
            .value()
            .cloned()
            .ok_or_else(|| anyhow!("Tenhou Web3 returned no value"))?,
    )
    .context("Tenhou Web3 contract")
}

fn call(method: &str, args: &str) -> String {
    format!("(()=>{{const api=window.__akagiTenhou;if(api?.client!=='web3'||typeof api.{method}!=='function')throw new Error('Tenhou Web3 capability unavailable');return api.{method}({args});}})()")
}

pub async fn read_input(page: &Page) -> Result<InputSnapshot> {
    evaluate::<InputSnapshot>(page, call("readInput", ""))
        .await?
        .check()
}

pub async fn probe_target(page: &Page, request: &TargetRequest) -> Result<TargetProbe> {
    evaluate::<TargetProbe>(page, call("probeTarget", &serde_json::to_string(request)?))
        .await?
        .check()
}

/// Final point check before shared CDP mouse-down. A fresh probe must retain
/// the same target identity and accept the actual viewport CSS point.
pub async fn validate_target(
    page: &Page,
    request: &TargetRequest,
    identity: &str,
    point: (f64, f64),
) -> Result<TargetProbe> {
    let mut value = serde_json::to_value(request)?;
    value["targetIdentity"] = json!(identity);
    value["point"] = json!({"x": point.0, "y": point.1});
    evaluate::<TargetProbe>(page, call("probeTarget", &value.to_string()))
        .await?
        .check()
}

pub async fn observe_tenhou_ui(page: &Page) -> Result<Web3Observation> {
    evaluate(page, call("readUi", "")).await
}

pub async fn press_tenhou_ui(page: &Page, kind: &str, identity: &str) -> Result<Dispatch> {
    evaluate(
        page,
        call(
            "pressUi",
            &format!(
                "{},{}",
                serde_json::to_string(kind)?,
                serde_json::to_string(identity)?
            ),
        ),
    )
    .await
}

fn label(tile: u32) -> Option<String> {
    (tile < 136).then(|| crate::bridge::tenhou::tile::tenhou_to_mjai_one(tile))
}

fn keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() == expected.len() && expected.iter().all(|key| map.contains_key(*key))
    })
}

/// Canonicalize only qualified wire shapes, retaining exact red-tile meaning.
fn normalized(value: &Value, hand: &[u32]) -> Option<Value> {
    if value.get("tag")?.as_str()? == "REACH" {
        return keys(value, &["tag"]).then(|| json!({"tag":"REACH"}));
    }
    if value.get("tag")?.as_str()? != "N" {
        return None;
    }
    let Some(kind) = value.get("type") else {
        return keys(value, &["tag"]).then(|| json!({"tag":"N"}));
    };
    let kind = u32::try_from(kind.as_u64()?).ok()?;
    match kind {
        1 | 3 => {
            if !keys(value, &["tag", "type", "hai0", "hai1"]) {
                return None;
            }
            let a = u32::try_from(value.get("hai0")?.as_u64()?).ok()?;
            let b = u32::try_from(value.get("hai1")?.as_u64()?).ok()?;
            if a == b || !hand.contains(&a) || !hand.contains(&b) {
                return None;
            }
            let mut tiles = [label(a)?, label(b)?];
            tiles.sort();
            Some(json!({"tag":"N","type":kind,"tiles":tiles}))
        }
        4 => {
            if !keys(value, &["tag", "type", "hai"]) {
                return None;
            }
            let tile = u32::try_from(value.get("hai")?.as_u64()?).ok()?;
            if tile >= 136 || tile % 4 != 0 || (0..4).any(|copy| !hand.contains(&(tile + copy))) {
                return None;
            }
            Some(json!({"tag":"N","type":4,"class":tile / 4}))
        }
        5 => {
            if !keys(value, &["tag", "type", "hai"]) {
                return None;
            }
            let tile = u32::try_from(value.get("hai")?.as_u64()?).ok()?;
            if !hand.contains(&tile) {
                return None;
            }
            Some(json!({"tag":"N","type":5,"tile":label(tile)?}))
        }
        2 | 6 | 7 | 9 => keys(value, &["tag", "type"]).then(|| json!({"tag":"N","type":kind})),
        _ => None,
    }
}

/// Resolve a descriptor, and reject the legacy encoder's red normalization.
/// The descriptor is never transmitted; it is matched against native offers.
pub fn choice_payload(action: &MjaiEvent, state: &TenhouState) -> Option<Value> {
    let payload: Value = serde_json::from_str(&crate::bridge::tenhou::encode::encode(
        action,
        state.hand_view(),
    )?)
    .ok()?;
    let normalized = normalized(&payload, &state.hand)?;
    match action {
        MjaiEvent::Chi { consumed, .. } | MjaiEvent::Pon { consumed, .. } => {
            let mut wanted = consumed.clone();
            wanted.sort();
            if normalized.get("tiles")? != &json!(wanted) {
                return None;
            }
        }
        MjaiEvent::Kakan { pai, .. } => {
            if normalized.get("tile")? != pai {
                return None;
            }
        }
        _ => {}
    }
    Some(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_preserve_redness_but_allow_equivalent_plain_copies() {
        let input = InputSnapshot {
            _client: ClientTag::Web3,
            ready: true,
            window: Some("doc:1".into()),
            hand: vec![16, 17, 18, 19],
            clock: None,
            viewport: CanvasRect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            tiles: vec![],
            next_draw: None,
            rest: None,
            offers: vec![
                Offer {
                    identity: "red".into(),
                    payload: json!({"tag":"N","type":1,"hai0":16,"hai1":17}),
                    bounds: None,
                    reveal: None,
                },
                Offer {
                    identity: "plain".into(),
                    payload: json!({"tag":"N","type":1,"hai0":18,"hai1":19}),
                    bounds: None,
                    reveal: None,
                },
            ],
        }
        .check()
        .unwrap();
        assert_eq!(
            input
                .matching_offer(&json!({"tag":"N","type":1,"hai0":17,"hai1":18}))
                .unwrap()
                .identity,
            "plain"
        );
        assert_eq!(
            input
                .matching_offer(&json!({"tag":"N","type":1,"hai0":19,"hai1":16}))
                .unwrap()
                .identity,
            "red"
        );
        assert!(input
            .matching_offer(&json!({"tag":"N","type":1,"hai0":17,"hai1":17}))
            .is_none());
        assert!(input
            .matching_offer(&json!({"tag":"N","type":1,"hai0":17,"hai1":20}))
            .is_none());
    }

    #[test]
    fn no_candidate_family_or_unknown_wire_field_is_guessed() {
        assert_ne!(
            normalized(&json!({"tag":"N","type":4,"hai":16}), &[16, 17, 18, 19]),
            normalized(&json!({"tag":"N","type":5,"hai":16}), &[16, 17, 18, 19])
        );
        assert!(normalized(&json!({"tag":"N","type":4,"hai":17}), &[16, 17, 18, 19]).is_none());
        assert!(normalized(&json!({"tag":"N","type":4,"hai":16}), &[16, 17, 18]).is_none());
        assert!(normalized(&json!({"tag":"N","type":10}), &[120]).is_none());
        assert!(normalized(&json!({"tag":"N","unexpected":true}), &[0]).is_none());
        assert!(serde_json::from_value::<InputSnapshot>(
            json!({"client":"web3","ready":false,"hand":[],"offers":[]})
        )
        .is_err());
        assert!(serde_json::from_value::<InputSnapshot>(
            json!({"client":"web4","ready":false,"window":null,"hand":[],"offers":[]})
        )
        .is_err());
    }

    #[test]
    fn geometry_and_probe_contracts_are_required_and_bounded() {
        let value = json!({"client":"web3","ready":false,"window":null,"clock":null,"hand":[16],"tiles":[
            {"identity":"tile:16","tile":16,"drawn":false,"legal":false,"bounds":{"x":10,"y":20,"width":30,"height":40}}
        ],"offers":[],"viewport":{"x":0,"y":0,"width":800,"height":600},"nextDraw":null,"rest":null});
        assert!(serde_json::from_value::<InputSnapshot>(value.clone())
            .unwrap()
            .check()
            .is_ok());
        let mut outside = value.clone();
        outside["tiles"][0]["bounds"]["x"] = json!(799);
        assert!(serde_json::from_value::<InputSnapshot>(outside)
            .unwrap()
            .check()
            .is_err());
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("nextDraw");
        assert!(serde_json::from_value::<InputSnapshot>(missing).is_err());
        assert!(serde_json::from_value::<TargetProbe>(
            json!({"client":"web3","status":"ready","target":null})
        )
        .unwrap()
        .check()
        .is_err());
        assert!(serde_json::from_value::<TargetProbe>(
            json!({"client":"web3","status":"unavailable","target":null})
        )
        .unwrap()
        .check()
        .is_ok());
    }

    #[test]
    fn clock_keeps_native_origin_and_granted_balance_separate() {
        let clock: ClockFacts = serde_json::from_value(json!({
            "elapsedMs":6000,"fixedMs":5000,"grantedBankMs":7000,
            "remainingBaseMs":0,"remainingBankMs":6000
        }))
        .unwrap();
        let now = Instant::now();
        assert_eq!(
            now.duration_since(clock.started_at(now).unwrap()),
            Duration::from_millis(6000)
        );
        assert_eq!(clock.granted_bank_ms, 7000);
        assert_eq!(clock.remaining_bank_ms, 6000);
        assert!(serde_json::from_value::<ClockFacts>(json!({
            "elapsedMs":6000,"fixedMs":5000,"remainingBaseMs":0,"remainingBankMs":6000
        }))
        .is_err());
    }
}
