//! Typed access to the Web3 client's native input and UI capabilities.
//! Payloads below describe a choice; only a verified native handler sends it.

use crate::autoplay::tenhou_state::TenhouState;
use crate::autoplay::ui::tenhou_web::Web3Observation;
use crate::schema::MjaiEvent;
use anyhow::{anyhow, Context, Result};
use chromiumoxide::page::Page;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSnapshot {
    #[serde(rename = "client")]
    _client: ClientTag,
    pub ready: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub window: Option<String>,
    pub hand: Vec<u32>,
    pub offers: Vec<Offer>,
}

fn required_nullable<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<String>, D::Error> {
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

pub async fn choose(page: &Page, window: &str, identity: &str) -> Result<Dispatch> {
    evaluate(
        page,
        call(
            "choose",
            &json!({"window": window, "identity": identity}).to_string(),
        ),
    )
    .await
}

pub async fn discard(page: &Page, window: &str, hand: &[u32], tile: u32) -> Result<Dispatch> {
    evaluate(
        page,
        call(
            "discard",
            &json!({"window": window, "hand": hand, "tile": tile}).to_string(),
        ),
    )
    .await
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
            offers: vec![
                Offer {
                    identity: "red".into(),
                    payload: json!({"tag":"N","type":1,"hai0":16,"hai1":17}),
                },
                Offer {
                    identity: "plain".into(),
                    payload: json!({"tag":"N","type":1,"hai0":18,"hai1":19}),
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
}
