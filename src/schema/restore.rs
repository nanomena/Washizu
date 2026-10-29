//! Public position import. This is an observation, never an invented move log.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KyokuRestore {
    pub seat: u8,
    pub bakaze: String,
    pub kyoku: u8,
    pub honba: u8,
    pub kyotaku: u8,
    pub oya: u8,
    pub scores: [i32; 4],
    pub hand: Vec<String>,
    pub dora_markers: Vec<String>,
    pub players: [RestorePlayer; 4],
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestorePlayer {
    /// Unclaimed river in provider order. Called tiles live in the melds;
    /// their former river positions are unknown, not appended as moves.
    pub river: Vec<String>,
    pub melds: Vec<RestoreMeld>,
    pub riichi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreMeld {
    pub kind: RestoreMeldKind,
    pub tiles: Vec<String>,
    pub target: Option<u8>,
    pub called: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMeldKind {
    Chi,
    Pon,
    Daiminkan,
    Ankan,
    Kakan,
}
