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
    Ready { through: u64 },
    Recovering { through: u64, reason: String },
    Blocked { through: u64, reason: String },
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
    Majsoul { opened_ns: i64 },
    Tenhou { opened_ns: i64, ops: u32 },
}

impl WindowProof {
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
