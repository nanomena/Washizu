//! Per-bot manifest + settings.
//!
//! Two files live next to `bot.py`:
//!
//! - `manifest.toml` — schema, source-controlled, immutable. Declares which
//!   knobs the bot exposes to the user (API URL, API key, model selection,
//!   …) and optional metadata for the install pipeline.
//! - `settings.toml` — values, mutable, gitignored. Written by the IPC
//!   `update_bot_settings` command, read on every spawn.
//!
//! Both files are optional. A bot that has neither runs unchanged — the
//! subprocess simply doesn't see an `AKAGI_BOT_CONFIG` env var.
//!
//! Resolution path (used by `BotManager::spawn_runner`):
//!
//! 1. `Manifest::load(bot_dir)` reads `manifest.toml` if present.
//! 2. `load_values(bot_dir, &manifest)` merges defaults + on-disk values,
//!    dropping unknown keys with a warning.
//! 3. `write_resolved(bot_dir, &values)` serializes the merged dict to
//!    `<bot>/.akagi/resolved_settings.json`. The path is exposed via
//!    `AKAGI_BOT_CONFIG` so the bot script can `json.load(open(path))`.
//!
//! TOML for human-edited files, JSON for the resolved file because Python's
//! stdlib parses JSON natively but not TOML on older interpreters.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::warn;

const MANIFEST_FILE: &str = "manifest.toml";
const SETTINGS_FILE: &str = "settings.toml";
const AKAGI_DIR: &str = ".akagi";
const RESOLVED_FILE: &str = "resolved_settings.json";

/// Top-level manifest shape.
///
/// `manifest_version` is `1` for the format documented here. Bumped when
/// the schema changes incompatibly so older Akagi binaries can refuse
/// gracefully rather than silently misinterpret newer fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub manifest_version: u32,
    pub bot: ManifestBot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<BotSource>,
    /// A process the bot needs for as long as it is the active one; see
    /// [`crate::bot::service`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forecast: Option<ForecastSpec>,
    pub settings: BTreeMap<String, FieldSpec>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            manifest_version: 1,
            bot: ManifestBot::default(),
            source: None,
            service: None,
            forecast: None,
            settings: BTreeMap::new(),
        }
    }
}

/// Bot-level metadata (separate from the `meta` field on a `BotResponse` —
/// renamed `ManifestBot` to avoid the name clash).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestBot {
    /// Only a positioned transport can prove the replayed prefix needed
    /// for automatic standby arbitration. Plain mjai remains its own API.
    pub protocol: BotProtocol,
    /// Subdir name under `mjai_bot/`. Should match the directory name.
    pub name: String,
    /// Human-readable label rendered in the UI bot picker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Free-form description (one or two sentences).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Bot's own version string (mirrors `pyproject.toml::version`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Game modes this bot can play. Accepted values: `"4p"`, `"3p"`.
    /// Defaults to `["4p"]` when absent so existing manifests stay 4p-only.
    pub supported_modes: Vec<String>,
}

impl Default for ManifestBot {
    fn default() -> Self {
        Self {
            protocol: BotProtocol::Mjai,
            name: String::new(),
            display: None,
            description: None,
            version: None,
            supported_modes: vec!["4p".to_string()],
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotProtocol {
    #[default]
    Mjai,
    PositionedMjai,
}

/// The `[service]` table: a script Akagi keeps running while the bot is the
/// active one, where the bot's own `bot.py` runs once per game.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// The script, relative to the bot directory. Its directory holds the
    /// service's own `pyproject.toml`, so its environment is not the bot's.
    pub entry: String,
}

/// Lightweight optional relay in the bot's own environment. It owns no model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForecastSpec {
    pub entry: String,
}

/// Optional source pointer for Phase 3 (GitHub install). Tagged on `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BotSource {
    GithubRelease {
        /// `owner/name`.
        repo: String,
        /// Glob to pick one asset out of the release. `None` → first `.zip`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_glob: Option<String>,
    },
}

/// One configurable knob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldSpec {
    #[serde(rename = "type")]
    pub kind: FieldKind,
    /// Label shown next to the input.
    pub label: String,
    /// Default value. Shape must match `kind` (validated on load).
    pub default: serde_json::Value,
    /// Help/tooltip text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// When `true`, the frontend renders a password input and Akagi tracing
    /// substitutes the value with `***` in logs.
    pub secret: bool,
    // Numeric bounds — only meaningful for Int/Float. Stored as f64 so a
    // single field works for either; the validator coerces back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Allowed values for `Enum` fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
}

impl Default for FieldSpec {
    fn default() -> Self {
        Self {
            kind: FieldKind::String,
            label: String::new(),
            default: serde_json::Value::Null,
            help: None,
            secret: false,
            min: None,
            max: None,
            step: None,
            choices: None,
        }
    }
}

/// Field types the frontend knows how to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    String,
    Bool,
    Int,
    Float,
    Enum,
}

impl Manifest {
    /// Read `<bot_dir>/manifest.toml`. Returns `Ok(None)` when the file
    /// does not exist — bots without a manifest are still valid.
    pub fn load(bot_dir: &Path) -> Result<Option<Manifest>> {
        let path = bot_dir.join(MANIFEST_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let body =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let manifest: Manifest =
            toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
        if manifest.manifest_version != 1 {
            bail!(
                "{} declares manifest_version={}, this Akagi build only understands 1",
                path.display(),
                manifest.manifest_version
            );
        }
        Ok(Some(manifest))
    }
}

/// Read `<bot_dir>/settings.toml` and merge with manifest defaults.
///
/// Behaviour:
/// - Missing `settings.toml` → all manifest defaults.
/// - On-disk keys not declared in the manifest are dropped with a warning
///   (so renamed fields don't accumulate forever).
/// - Values whose type doesn't match the manifest are rejected loudly —
///   the bot's `manifest.toml` is the source of truth and a stale on-disk
///   value should not silently corrupt the resolved dict.
pub fn load_values(
    bot_dir: &Path,
    manifest: &Manifest,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut resolved: BTreeMap<String, serde_json::Value> = manifest
        .settings
        .iter()
        .map(|(k, spec)| (k.clone(), spec.default.clone()))
        .collect();

    let path = bot_dir.join(SETTINGS_FILE);
    if !path.is_file() {
        return Ok(resolved);
    }

    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let on_disk: toml::Table =
        toml::from_str(&body).with_context(|| format!("parse {}", path.display()))?;

    for (key, toml_value) in on_disk {
        let Some(spec) = manifest.settings.get(&key) else {
            warn!(
                bot = %manifest.bot.name,
                key = %key,
                "settings.toml key not declared in manifest — dropped",
            );
            continue;
        };
        let value = toml_to_json(toml_value);
        validate(&key, spec, &value)?;
        resolved.insert(key, value);
    }

    Ok(resolved)
}

/// Write the resolved (defaults ⊕ on-disk values) settings dict to
/// `<bot_dir>/.akagi/resolved_settings.json`. Returns the path so the
/// caller can set `AKAGI_BOT_CONFIG=<path>` on the child.
pub fn write_resolved(
    bot_dir: &Path,
    values: &BTreeMap<String, serde_json::Value>,
) -> Result<PathBuf> {
    let dir = bot_dir.join(AKAGI_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let path = dir.join(RESOLVED_FILE);
    let body = serde_json::to_vec_pretty(values).context("serialize resolved settings")?;
    if std::fs::read(&path)
        .map(|existing| existing == body)
        .unwrap_or(false)
    {
        return std::fs::canonicalize(&path)
            .with_context(|| format!("canonicalize {}", path.display()));
    }
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    // Canonicalize so the env var survives the child's `current_dir(bot_dir)`
    // — a relative path would otherwise be resolved against the bot's cwd
    // and miss the file (`bot_dir/bot_dir/.akagi/...`).
    std::fs::canonicalize(&path).with_context(|| format!("canonicalize {}", path.display()))
}

fn validate(key: &str, spec: &FieldSpec, value: &serde_json::Value) -> Result<()> {
    use serde_json::Value;
    match spec.kind {
        FieldKind::String => {
            if !value.is_string() {
                bail!("setting {key:?}: expected string, got {value}");
            }
        }
        FieldKind::Bool => {
            if !value.is_boolean() {
                bail!("setting {key:?}: expected bool, got {value}");
            }
        }
        FieldKind::Int => {
            let Some(n) = value.as_i64() else {
                bail!("setting {key:?}: expected integer, got {value}");
            };
            check_bounds(key, n as f64, spec)?;
        }
        FieldKind::Float => {
            let n = match value {
                Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                _ => bail!("setting {key:?}: expected float, got {value}"),
            };
            check_bounds(key, n, spec)?;
        }
        FieldKind::Enum => {
            let Value::String(s) = value else {
                bail!("setting {key:?}: expected enum string, got {value}");
            };
            let Some(choices) = &spec.choices else {
                bail!("setting {key:?}: enum field missing `choices` in manifest");
            };
            if !choices.iter().any(|c| c == s) {
                bail!("setting {key:?}: {s:?} not in choices {choices:?}");
            }
        }
    }
    Ok(())
}

fn check_bounds(key: &str, n: f64, spec: &FieldSpec) -> Result<()> {
    if let Some(min) = spec.min {
        if n < min {
            bail!("setting {key:?}: {n} < min {min}");
        }
    }
    if let Some(max) = spec.max {
        if n > max {
            bail!("setting {key:?}: {n} > max {max}");
        }
    }
    Ok(())
}

fn toml_to_json(v: toml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        toml::Value::String(s) => J::String(s),
        toml::Value::Integer(i) => J::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(J::Number)
            .unwrap_or(J::Null),
        toml::Value::Boolean(b) => J::Bool(b),
        // Datetime / array / table aren't reachable through validated
        // FieldKind variants in v1, but we map them defensively so a
        // schema-extension doesn't silently lose data.
        toml::Value::Datetime(d) => J::String(d.to_string()),
        toml::Value::Array(a) => J::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            J::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn sample_manifest() -> Manifest {
        let mut m = Manifest::default();
        m.bot.name = "demo".into();
        m.bot.display = Some("Demo".into());
        m.settings.insert(
            "api_url".into(),
            FieldSpec {
                kind: FieldKind::String,
                label: "URL".into(),
                default: json!("https://example.com"),
                ..Default::default()
            },
        );
        m.settings.insert(
            "online".into(),
            FieldSpec {
                kind: FieldKind::Bool,
                label: "Online".into(),
                default: json!(false),
                ..Default::default()
            },
        );
        m.settings.insert(
            "temperature".into(),
            FieldSpec {
                kind: FieldKind::Float,
                label: "Temp".into(),
                default: json!(1.0),
                min: Some(0.0),
                max: Some(2.0),
                ..Default::default()
            },
        );
        m.settings.insert(
            "model".into(),
            FieldSpec {
                kind: FieldKind::Enum,
                label: "Model".into(),
                default: json!("a"),
                choices: Some(vec!["a".into(), "b".into()]),
                ..Default::default()
            },
        );
        m
    }

    #[test]
    fn load_returns_none_when_manifest_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(Manifest::load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn manifest_round_trips_through_toml() {
        let m = sample_manifest();
        let body = toml::to_string_pretty(&m).unwrap();
        let back: Manifest = toml::from_str(&body).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn positioned_mjai_requires_an_explicit_transport_contract() {
        let plain: Manifest = toml::from_str("[bot]\nname = 'demo'\n").unwrap();
        assert_eq!(plain.bot.protocol, BotProtocol::Mjai);
        let positioned: Manifest =
            toml::from_str("[bot]\nname = 'miai'\nprotocol = 'positioned_mjai'\n").unwrap();
        assert_eq!(positioned.bot.protocol, BotProtocol::PositionedMjai);
        assert!(toml::from_str::<Manifest>("[bot]\nprotocol = 'unknown'\n").is_err());
    }

    #[test]
    fn rejects_unknown_manifest_version() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp.path().join("manifest.toml"),
            r#"
manifest_version = 99
[bot]
name = "demo"
"#,
        );
        let err = Manifest::load(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("manifest_version=99"),
            "got: {err:#}"
        );
    }

    #[test]
    fn load_values_falls_back_to_defaults_when_settings_missing() {
        let tmp = TempDir::new().unwrap();
        let m = sample_manifest();
        let v = load_values(tmp.path(), &m).unwrap();
        assert_eq!(v["api_url"], json!("https://example.com"));
        assert_eq!(v["online"], json!(false));
        assert_eq!(v["temperature"], json!(1.0));
        assert_eq!(v["model"], json!("a"));
    }

    #[test]
    fn load_values_drops_unknown_keys() {
        let tmp = TempDir::new().unwrap();
        let m = sample_manifest();
        write(
            &tmp.path().join("settings.toml"),
            r#"
api_url = "https://override.example"
mystery = "ignored"
"#,
        );
        let v = load_values(tmp.path(), &m).unwrap();
        assert_eq!(v["api_url"], json!("https://override.example"));
        assert!(!v.contains_key("mystery"));
    }

    #[test]
    fn write_resolved_emits_json_at_expected_path() {
        let tmp = TempDir::new().unwrap();
        let mut values: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        values.insert("k".into(), json!("v"));
        let path = write_resolved(tmp.path(), &values).unwrap();
        assert!(path.ends_with(".akagi/resolved_settings.json"));
        let body = std::fs::read_to_string(&path).unwrap();
        let back: BTreeMap<String, serde_json::Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(back["k"], json!("v"));
    }

    #[test]
    fn write_resolved_leaves_identical_file_untouched() {
        let tmp = TempDir::new().unwrap();
        let mut values: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        values.insert("k".into(), json!("v"));

        let path = write_resolved(tmp.path(), &values).unwrap();
        let original_perms = std::fs::metadata(&path).unwrap().permissions();
        let mut perms = original_perms.clone();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        let result = write_resolved(tmp.path(), &values);

        std::fs::set_permissions(&path, original_perms).unwrap();

        result.unwrap();
    }

    /// Regression: the resolved-settings path must be absolute. The bot
    /// child runs with `current_dir(bot_dir)`, so a relative
    /// `AKAGI_BOT_CONFIG` would resolve against the wrong cwd and the bot
    /// would silently fall back to its hard-coded defaults (e.g. mortal
    /// dropping `online: true` and the API key on the floor).
    #[test]
    fn write_resolved_returns_absolute_path_even_for_relative_bot_dir() {
        let tmp = TempDir::new().unwrap();
        let cwd_guard = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        std::fs::create_dir_all("bots/demo").unwrap();
        let mut values: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        values.insert("k".into(), json!("v"));

        let result = write_resolved(Path::new("bots/demo"), &values);
        // Always restore cwd before asserting so a failure doesn't leak.
        std::env::set_current_dir(&cwd_guard).unwrap();

        let path = result.unwrap();
        assert!(
            path.is_absolute(),
            "expected absolute, got {}",
            path.display()
        );
        assert!(path.ends_with(".akagi/resolved_settings.json"));
    }
}
