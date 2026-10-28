//! Shared browser autoplay runtime.
//!
//! SiteAdapter normalizes real observations and exact physical targets.
//! TurnController owns preparation, readiness, pacing, one-use mouse permits,
//! confirmations, retries and cancellation for the coordinator-selected action.
//! PacingPolicy chooses one absolute schedule; PointerController executes all
//! browser travel, hover, press and release. UI Machine separately owns lobby,
//! result and continuation-page effects. Forecasts only feed that same pointer.
//!
//! The non-browser Riichi City protocol adapter has its own private frame
//! transport and uses the same pacing policy. No browser action calls native
//! discard/choice handlers directly.

pub mod advisor;
pub mod budget;
pub mod canvas;
pub mod cdp_input;
pub mod context;
pub mod delay;
pub mod humanize;
pub mod inject;
pub mod majsoul;
pub mod manager;
mod manual_pointer;
pub mod pixels;
pub mod platform;
pub mod pointer;
pub mod riichi_city;
pub mod table;
pub mod tenhou;
pub mod tenhou_state;
pub mod tenpai;
pub mod toggles;
pub mod transaction;
pub mod ui;
pub mod verify;

pub use budget::{BudgetSource, SharedTimeBudget, TimeBudget};
pub use context::{AutoplayContext, CanvasRect};
pub use manager::run_autoplay_manager;
pub use platform::{ActionContext, SiteAdapter, TurnPlan};
pub use verify::{InputKind, InputWatch, SharedInputWatch};
