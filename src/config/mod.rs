mod autoplay;
mod bot;
mod capture;
mod logging;
mod merge;
mod monitor;
mod platform;

pub use autoplay::{
    AutoplayConfig, DelayDistribution, DelayMode, DelayModelConfig, MajsoulAutoplayConfig,
};
pub use bot::{BotConfig, NativeApiConfig};
pub use capture::{CaptureConfig, HttpCaptureConfig};
pub use logging::LoggingConfig;
pub use merge::merge_into;
pub use monitor::MonitorConfig;
pub use platform::{Platform, PlatformConfig};

use serde::{Deserialize, Serialize};
use std::path::Path;

/// `config.toml`. Every section has defaults, so a file may name as little as
/// it likes; keys this build does not know are passed over on load and kept on
/// save ([`merge_into`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub logging: LoggingConfig,
    pub platform: PlatformConfig,
    pub bot: BotConfig,
    pub capture: CaptureConfig,
    pub autoplay: AutoplayConfig,
    pub monitor: MonitorConfig,
}

fn write_default_config(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let defaults = AppConfig::default();
    let body = toml::to_string_pretty(&defaults).map_err(std::io::Error::other)?;
    std::fs::write(path, body)
}

/// Load the configuration at `path`, writing the defaults there first if there
/// is no file. A file that cannot be written, read or parsed leaves the
/// defaults in force, and says so on stderr.
pub fn load_config(path: &Path) -> AppConfig {
    if !path.exists() {
        eprintln!(
            "No config file found, writing defaults to: {}",
            path.display()
        );
        if let Err(e) = write_default_config(path) {
            eprintln!("Failed to write default config: {e}, using in-memory defaults");
            return AppConfig::default();
        }
    }
    eprintln!("Loading config from: {}", path.display());

    let mut cfg = match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<AppConfig>(&content) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("Failed to parse config: {e}, using defaults");
                AppConfig::default()
            }
        },
        Err(e) => {
            eprintln!("Failed to read config: {e}, using defaults");
            AppConfig::default()
        }
    };
    // Migrate legacy `[bot] active = "..."` into `active_4p` once.
    cfg.bot.migrate_legacy_active();
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        d.push(format!("akagi-cfg-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_the_defaults_where_there_is_no_file() {
        let dir = temp_dir("missing");
        let target = dir.join("nested").join("config.toml");
        assert!(!target.exists());

        let cfg = load_config(&target);

        assert!(target.exists(), "default config file should be created");
        let written: AppConfig =
            toml::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(written.capture, cfg.capture);
        assert_eq!(written.monitor, cfg.monitor);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The nested `[bot.api]` table must survive a TOML round-trip inside the
    /// full `AppConfig` — a table field serialized among scalar `[bot]` keys is
    /// an easy way to get "table before value" ordering wrong. Pure string
    /// round-trip, no network: every value is a placeholder (loopback URL,
    /// fake key, made-up model ids).
    #[test]
    fn bot_api_section_round_trips_through_toml() {
        let mut cfg = AppConfig::default();
        cfg.bot.api.enabled = true;
        cfg.bot.api.base_url = "http://127.0.0.1:8080".into();
        cfg.bot.api.key = "test-key-not-real".into();
        cfg.bot.api.model_4p = "4p-model".into();
        cfg.bot.api.model_3p = "3p-model".into();

        let body = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            body.contains("[bot.api]"),
            "expected a [bot.api] table in:\n{body}"
        );

        let back: AppConfig = toml::from_str(&body).unwrap();
        assert!(back.bot.api.enabled);
        assert_eq!(back.bot.api.base_url, "http://127.0.0.1:8080");
        assert_eq!(back.bot.api.key, "test-key-not-real");
        assert_eq!(back.bot.api.model_4p, "4p-model");
        assert_eq!(back.bot.api.model_3p, "3p-model");
        assert!(back.bot.api.is_active());
        assert_eq!(back.bot.api.model_for(3), "3p-model");
        assert_eq!(back.bot.api.model_for(4), "4p-model");
    }

    /// A legacy config file without any `[bot.api]` section still parses, with
    /// the API path defaulting to off (fully offline local model). The default
    /// server URL is pre-filled but that alone must not activate the API.
    #[test]
    fn missing_bot_api_section_defaults_to_disabled() {
        let legacy = "[bot]\nenabled = true\nactive_4p = \"akagi-native\"\n";
        let cfg: AppConfig = toml::from_str(legacy).unwrap();
        assert!(!cfg.bot.api.enabled);
        assert!(
            !cfg.bot.api.is_active(),
            "default URL alone must not activate"
        );
        assert_eq!(cfg.bot.api.base_url, NativeApiConfig::default().base_url);
        assert!(!cfg.bot.api.base_url.is_empty(), "URL should be pre-filled");
    }

    #[test]
    fn reads_the_file_that_is_there_and_passes_over_what_it_does_not_know() {
        let dir = temp_dir("existing");
        let target = dir.join("config.toml");
        std::fs::write(
            &target,
            "[monitor]\nquick_confirm = true\n\n[overlay]\nenabled = true\n",
        )
        .unwrap();

        let cfg = load_config(&target);
        assert!(cfg.monitor.quick_confirm);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "[monitor]\nquick_confirm = true\n\n[overlay]\nenabled = true\n",
            "loading leaves the file as it is"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
