//! What a platform says of the player's own account outside any one game:
//! where it stands on the platform's rank ladders.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The native identifier space used by an account's rank report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountPlatform {
    Majsoul,
    Tenhou,
}

/// A place on one of the platform's rank ladders, as the platform gives it.
///
/// On Mahjong Soul `id` names the ladder, the rank and its star — `10203` is
/// the four-player ladder (`1`), the second rank (`02`), third star (`03`) —
/// and `score` is the points held toward the next.
/// On Tenhou `id` is the raw PF4/PF3 rank (0 newcomer, 10 shodan,
/// 11 nidan, ...), and `score` is the server's current pt balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level {
    pub id: u32,
    pub score: i32,
}

/// The player's own ranks as the platform last said them, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLevels {
    pub platform: AccountPlatform,
    /// The four-player ladder.
    pub level: Option<Level>,
    /// The three-player ladder.
    pub level3: Option<Level>,
    /// When the platform said it: every game that ended before is counted in it.
    pub at: DateTime<Utc>,
}
