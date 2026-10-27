//! What capture is pointed at, and what it keeps of what it sees.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    /// The address the game view is sent to. It has to belong to the selected
    /// platform over HTTPS (`capture::embedded::game_url`): the view is signed
    /// in to this account profile.
    pub url: String,
    pub http: HttpCaptureConfig,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            url: "https://game.maj-soul.com/1/".to_string(),
            http: HttpCaptureConfig::default(),
        }
    }
}

/// What to record of the HTTP traffic capture sees.
///
/// Akagi used to record WebSocket frames and discard everything else,
/// which hid the game's own HTTP entirely — route topology, version
/// endpoints, and the analytics beacons through which the client reports
/// on itself. This turns that back on.
///
/// `record_all` defaults to **off** on purpose. Full HTTP capture puts
/// access tokens, cookies and authorization headers into the session
/// file, and nothing is redacted — redaction would re-create the blind
/// spot this exists to remove. So the default keeps only the exchanges a
/// recognizer understood (analytics beacons, and Akagi's own notes about
/// traffic it declined to intercept), and the firehose is opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpCaptureConfig {
    /// Record every exchange seen, not just recognized ones.
    /// Off by default — see the struct docs.
    pub record_all: bool,
    /// Keep textual request/response bodies within `max_body_bytes`.
    /// Only consulted when the exchange is being recorded at all.
    pub bodies: bool,
    /// Ceiling on a buffered body. Larger ones are recorded with their
    /// size and the reason they were skipped, never truncated silently.
    pub max_body_bytes: usize,
    /// Also record static subresources (images, fonts, media,
    /// stylesheets). Off by default — a WebGL client pulls enough of them
    /// to bury everything else, and none of it says anything about the
    /// client.
    pub static_assets: bool,
}

impl Default for HttpCaptureConfig {
    fn default() -> Self {
        Self {
            record_all: false,
            bodies: true,
            max_body_bytes: 256 * 1024,
            static_assets: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let cfg = CaptureConfig::default();
        let back: CaptureConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.url, "https://game.maj-soul.com/1/");
        assert!(!back.http.record_all, "the firehose is opt-in");
    }

    /// A file written for an earlier build names things this one does not
    /// have; what it does have still comes through.
    #[test]
    fn keys_of_other_builds_are_passed_over() {
        let cfg: CaptureConfig = toml::from_str(
            "mode = \"chromium\"\n[chromium]\nstart_url = \"https://example.test/\"\n[http]\nrecord_all = true\n",
        )
        .unwrap();
        assert_eq!(cfg.url, CaptureConfig::default().url);
        assert!(cfg.http.record_all);
    }
}
