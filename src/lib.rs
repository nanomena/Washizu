//! Akagi's core: capture, the bridge, the tracker, the bot, autoplay and the
//! monitor snapshot. It has no window of its own — a desktop shell (`desktop/`)
//! runs it as `akagi-core` and draws what it says. See [`headless`].

pub mod autoplay;
pub mod bot;
pub mod bridge;
pub mod capture;
pub mod config;
pub mod engine;
pub mod event_bus;
pub mod game_state;
pub mod headless;
pub mod history;
pub mod inspector;
pub mod logger;
pub mod monitor;
pub mod schema;
pub mod state;
pub mod util;
