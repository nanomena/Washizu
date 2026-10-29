//! Local-only embedded policy. No API configuration and no Advisor lifetime.

use crate::bot::{native, BotResponse};
use crate::game_state::convert;
use crate::schema::MjaiEvent;
use anyhow::{bail, Result};
use native_bot::engine::Engine;

pub struct LocalStandby {
    engine: Engine,
    seat: u8,
    players: u8,
}

impl LocalStandby {
    /// With an incomplete observation history, the neural rules replay is
    /// unavailable. Prefer only provider-proved wins, otherwise the actual
    /// new draw or a legal decline. This policy owns no input or retry loop.
    pub fn observed_reply(legal: &[MjaiEvent]) -> Option<BotResponse> {
        let action = legal
            .iter()
            .find(|e| matches!(e, MjaiEvent::Hora { .. }))
            .or_else(|| {
                legal.iter().find(|e| {
                    matches!(
                        e,
                        MjaiEvent::Dahai {
                            tsumogiri: true,
                            ..
                        }
                    )
                })
            })
            .or_else(|| legal.iter().find(|e| matches!(e, MjaiEvent::Dahai { .. })))
            .or_else(|| legal.iter().find(|e| matches!(e, MjaiEvent::None)))?
            .clone();
        Some(BotResponse {
            request: None,
            action,
            meta: Some(
                serde_json::json!({"recovery_policy":"Observed win / actual draw / independently validated hand discard / legal decline; missing temporal history remains unavailable."}),
            ),
        })
    }
    pub fn new(seat: u8, players: u8) -> Result<Self> {
        Ok(Self {
            engine: native_bot::defaults::engine(players, seat)?,
            seat,
            players,
        })
    }

    pub fn observe(&mut self, event: &MjaiEvent) -> Result<()> {
        let value = serde_json::to_value(event)?;
        for key in ["actor", "target"] {
            if value
                .get(key)
                .and_then(|v| v.as_u64())
                .is_some_and(|s| s >= u64::from(self.players))
            {
                bail!("standby received an invalid {key}");
            }
        }
        if let Some(event) = convert::to_riichienv(event)? {
            self.engine.feed(event);
        }
        Ok(())
    }

    pub fn legal(&self) -> Vec<MjaiEvent> {
        self.engine
            .legal_replies()
            .into_iter()
            .filter_map(|a| self.bind_winning_tile(native::bot_action_to_mjai(a, self.seat)))
            .collect()
    }

    pub fn decide(&mut self) -> Result<BotResponse> {
        let decision = self
            .engine
            .decide()?
            .ok_or_else(|| anyhow::anyhow!("standby has no legal decision"))?;
        let (action, meta) = native::local_reply(&decision, self.seat);
        let action = self
            .bind_winning_tile(action)
            .ok_or_else(|| anyhow::anyhow!("standby cannot identify the actual winning tile"))?;
        Ok(BotResponse {
            request: None,
            action,
            meta,
        })
    }

    fn bind_winning_tile(&self, mut event: MjaiEvent) -> Option<MjaiEvent> {
        if let MjaiEvent::Hora { target, pai, .. } = &mut event {
            *pai = Some(self.engine.winning_tile(*target)?);
        }
        Some(event)
    }
}

#[cfg(test)]
mod observed_tests {
    use super::*;
    #[test]
    fn a_restored_post_call_window_can_use_its_proved_hand_discard_without_inventing_one() {
        let action = MjaiEvent::Dahai {
            actor: 0,
            pai: "9m".into(),
            tsumogiri: false,
        };
        let reply = LocalStandby::observed_reply(std::slice::from_ref(&action)).unwrap();
        assert_eq!(reply.action, action);
        assert!(LocalStandby::observed_reply(&[]).is_none());
    }
}

/// Physical-copy distinctions, red identity, claim source and exact consumed
/// tiles all matter. Ordering of a consume multiset does not.
pub fn legal_response(response: &BotResponse, legal: &[MjaiEvent]) -> bool {
    if response.unavailable_reason().is_some() {
        return false;
    }
    let canonical = |event: &MjaiEvent| {
        let mut value = serde_json::to_value(event).expect("mjai serializes");
        if let Some(items) = value.get_mut("consumed").and_then(|v| v.as_array_mut()) {
            items.sort_by_key(|v| v.as_str().unwrap_or_default().to_owned());
        }
        value
    };
    let candidate = canonical(&response.action);
    legal.iter().any(|event| canonical(event) == candidate)
}
