//! Shared schema types used across the project.
//!
//! Anything that needs to travel between modules — protocol events, what the
//! core's parts report about themselves, persisted records — lives here so it
//! isn't owned by any single subsystem.

pub mod account;
pub mod history;
pub mod inspector;
pub mod mjai;
pub mod status;

pub use account::{AccountLevels, AccountPlatform, Level};
pub use history::{
    GameRecord, GameStats, HistoryEvent, HistoryEventLog, HistoryFilter, KyokuMode, MatchInfo,
    Platform,
};
pub use inspector::{
    BotReaction, CaptureSource, FrameDirection, FrameRaw, HttpAnnotation, HttpBody, HttpExchange,
    HttpHeader, HttpPhase, InspectorEntry, ParsedFrame,
};
pub use mjai::{GameEndReason, GameMeta, MjaiEvent};
pub use status::{BotStatus, CaptureStatus, LoadStage, LogEntry, Notification, NotifyLevel};
