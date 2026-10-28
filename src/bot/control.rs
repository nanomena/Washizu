//! Public provenance shared by the decision coordinator and UI effect owner.
//! These types carry identity and health; they grant no input permission.

use crate::autoplay::AutoplayContext;
use crate::config::Platform;
use crate::monitor::DecisionContext;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    Primary,
    NativeStandby,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunnerHealth {
    DirectReady { delivered: u64 },
    DirectUnavailable { delivered: u64, reason: String },
    Ready { through: u64 },
    Recovering { through: u64, reason: String },
    Blocked { through: u64, reason: String },
}

/// Original producer-owned input window, retained internally through planning.
/// This is never reconstructed from a later shared-slot read or wire metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputWindow {
    Majsoul(crate::autoplay::TimeBudget),
    Tenhou(crate::autoplay::tenhou_state::DecisionWindow),
}

impl InputWindow {
    /// A deliberately restricted set proved without replaying missing history.
    /// The provider adapter normalizes real observations; it selects no source.
    pub fn interrupted_actions(
        self,
        event: &crate::schema::MjaiEvent,
        seat: u8,
    ) -> Vec<crate::schema::MjaiEvent> {
        match self {
            Self::Tenhou(window) => crate::bridge::tenhou::observed_actions(event, seat, window),
            Self::Majsoul(_) => Vec::new(),
        }
    }
    /// Known base-clock fact from this original window, never a newer slot.
    pub fn fixed_ms(self) -> Option<u32> {
        match self {
            Self::Majsoul(b) => Some(b.fixed_ms),
            Self::Tenhou(w) => w.clock.map(|clock| clock.fixed_ms),
        }
    }

    pub fn proof(self) -> WindowProof {
        match self {
            Self::Majsoul(b) => WindowProof::Majsoul {
                opened_ns: instant_token(b.opened_at),
            },
            Self::Tenhou(w) => WindowProof::Tenhou {
                opened_ns: instant_token(w.opened_at),
                ops: w.ops,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionRequest {
    pub context: DecisionContext,
    pub window: Option<InputWindow>,
}

/// Serialization of the existing bridge-owned monotonic window identity.
/// The process-wide origin is only a lossless transport projection of
/// Instant, not a new window clock or a producer of window generations.
fn instant_token(at: Instant) -> i64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = *ORIGIN.get_or_init(Instant::now);
    if at >= origin {
        i64::try_from(at.duration_since(origin).as_nanos()).expect("window age fits i64")
    } else {
        -i64::try_from(origin.duration_since(at).as_nanos()).expect("window age fits i64")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum WindowProof {
    Majsoul {
        #[serde(with = "window_nanos")]
        opened_ns: i64,
    },
    Tenhou {
        #[serde(with = "window_nanos")]
        opened_ns: i64,
        ops: u32,
    },
}

// Decimal strings preserve nanoseconds through JavaScript and JSON viewers.
mod window_nanos {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl WindowProof {
    pub fn is_current(self, ctx: &AutoplayContext) -> bool {
        let platform = match self {
            Self::Majsoul { .. } => Platform::Majsoul,
            Self::Tenhou { .. } => Platform::Tenhou,
        };
        Self::capture(ctx, platform) == Some(self)
    }
    pub fn capture(ctx: &AutoplayContext, platform: Platform) -> Option<Self> {
        match platform {
            Platform::Majsoul => {
                ctx.time_budget
                    .read()
                    .ok()
                    .and_then(|g| *g)
                    .map(|b| Self::Majsoul {
                        opened_ns: instant_token(b.opened_at),
                    })
            }
            Platform::Tenhou => ctx
                .tenhou_state
                .read()
                .ok()
                .and_then(|g| g.as_ref().and_then(|s| s.window))
                .map(|w| Self::Tenhou {
                    opened_ns: instant_token(w.opened_at),
                    ops: w.ops,
                }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionControl {
    pub context: DecisionContext,
    pub window: Option<WindowProof>,
    pub source: DecisionSource,
    pub primary: RunnerHealth,
    pub standby: RunnerHealth,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_identity_round_trips_beyond_javascript_integer_precision() {
        let proof = WindowProof::Tenhou {
            opened_ns: i64::MIN + 1,
            ops: 42,
        };
        let json = serde_json::to_value(proof).unwrap();
        assert_eq!(json["opened_ns"], (i64::MIN + 1).to_string());
        assert_eq!(serde_json::from_value::<WindowProof>(json).unwrap(), proof);
    }
}
