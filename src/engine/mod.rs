//! Decision engines: separate programs that choose actions.
//!
//! Washizu owns everything around a decision; an engine owns the decision
//! itself — its model, every inference parameter, and what the monitor says
//! about it. The contract between the two is [`PROTOCOL.md`](PROTOCOL.md):
//! engine packages under `<data>/engines/`, one engine process for the whole
//! application ([`host`]), and a positioned line protocol over loopback TCP
//! ([`runner`] for decisions, `bot::forecast` for forecasts).

pub mod host;
pub mod runner;
mod status;

pub use runner::EngineRunner;
pub use status::{EngineStatus, HostState};

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

/// The protocol this build speaks, in packages, endpoints and greetings.
pub const PROTOCOL: &str = "washizu-engine/1";

const ENGINES_FILE: &str = "engines.toml";
const ENGINE_FILE: &str = "engine.toml";
const ENDPOINT_FILE: &str = "endpoint.json";
const STATE_FILE: &str = "host-state.json";

/// The installed engine packages: `<data>/engines/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packages {
    root: PathBuf,
}

/// What a core is told of engines: where the packages are, and which
/// account it serves. The account id is opaque to Washizu beyond identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub packages: Packages,
    pub account: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnginesFile {
    default: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineFile {
    protocol: String,
    display: String,
    entry: String,
}

/// One installed engine package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The folder's name: what an account's bot slot names.
    pub name: String,
    pub dir: PathBuf,
    /// The name it goes by.
    pub display: String,
    /// The program Washizu runs, relative to `dir`.
    pub entry: PathBuf,
}

impl Packages {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The package an account's bot slot names, when one is installed under
    /// that name. `Ok(None)` means the name is not an engine; a package whose
    /// `engine.toml` is invalid is an error, never another kind of bot.
    pub fn find(&self, name: &str) -> Result<Option<Package>> {
        if !is_plain_name(name) {
            return Ok(None);
        }
        let dir = self.root.join(name);
        let file = dir.join(ENGINE_FILE);
        if !file.is_file() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let parsed: EngineFile =
            toml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
        if parsed.protocol != PROTOCOL {
            bail!(
                "{}: protocol {:?} is not {PROTOCOL:?}",
                file.display(),
                parsed.protocol
            );
        }
        let entry = PathBuf::from(&parsed.entry);
        if parsed.entry.is_empty()
            || !entry
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            bail!(
                "{}: entry {:?} must be a relative path inside the package",
                file.display(),
                parsed.entry
            );
        }
        if parsed.display.trim().is_empty() {
            bail!("{}: display must name the engine", file.display());
        }
        Ok(Some(Package {
            name: name.to_owned(),
            dir,
            display: parsed.display,
            entry,
        }))
    }

    /// The engine new accounts use, from `engines.toml`, when that engine is
    /// installed.
    pub fn default_package(&self) -> Result<Option<Package>> {
        let file = self.root.join(ENGINES_FILE);
        if !file.is_file() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let parsed: EnginesFile =
            toml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
        self.find(&parsed.default)
    }
}

impl Package {
    /// Where the running engine publishes how to reach it.
    pub fn endpoint(&self) -> EndpointFile {
        EndpointFile(self.dir.join(ENDPOINT_FILE))
    }

    /// Where the engine host publishes the engine's state.
    pub fn state_file(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }
}

fn is_plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.contains(['/', '\\', ':'])
}

/// The file a running engine writes its endpoint to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointFile(pub PathBuf);

/// How to reach the running engine.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub token: String,
}

impl EndpointFile {
    /// Read it now: a restarted engine publishes a new port and token.
    pub fn read(&self) -> Result<Endpoint> {
        let text = std::fs::read_to_string(&self.0).with_context(|| {
            format!(
                "the engine has published no endpoint at {}",
                self.0.display()
            )
        })?;
        let endpoint: Endpoint =
            serde_json::from_str(&text).with_context(|| format!("parse {}", self.0.display()))?;
        if endpoint.protocol != PROTOCOL {
            bail!(
                "the engine's endpoint speaks {:?}, not {PROTOCOL:?}",
                endpoint.protocol
            );
        }
        if endpoint.token.is_empty() || endpoint.port == 0 {
            bail!("the engine's endpoint names no port or token");
        }
        Ok(endpoint)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn package(root: &Path, name: &str) -> Package {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ENGINE_FILE),
            format!(
                "protocol = \"{PROTOCOL}\"\ndisplay = \"Test engine\"\nentry = \"engine.py\"\n"
            ),
        )
        .unwrap();
        Packages::new(root.to_path_buf())
            .find(name)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn an_installed_package_is_found_by_its_folder_name() {
        let root = tempfile::tempdir().unwrap();
        let found = package(root.path(), "engine-a");
        assert_eq!(found.name, "engine-a");
        assert_eq!(found.display, "Test engine");
        assert_eq!(found.entry, PathBuf::from("engine.py"));
        assert_eq!(
            found.endpoint(),
            EndpointFile(root.path().join("engine-a").join("endpoint.json"))
        );
        let packages = Packages::new(root.path().to_path_buf());
        // Other names are other kinds of bot, never an engine.
        for name in ["missing", "", "../engine-a", "a/b", "C:x"] {
            assert_eq!(packages.find(name).unwrap(), None, "{name:?}");
        }
    }

    #[test]
    fn an_invalid_package_is_an_error_not_another_bot() {
        let root = tempfile::tempdir().unwrap();
        let packages = Packages::new(root.path().to_path_buf());
        for (name, text) in [
            (
                "old",
                "protocol = \"washizu-engine/0\"\ndisplay = \"x\"\nentry = \"e.py\"\n",
            ),
            (
                "escape",
                &format!("protocol = \"{PROTOCOL}\"\ndisplay = \"x\"\nentry = \"../e.py\"\n"),
            ),
            (
                "extra",
                &format!(
                    "protocol = \"{PROTOCOL}\"\ndisplay = \"x\"\nentry = \"e.py\"\nport = 1\n"
                ),
            ),
            (
                "unnamed",
                &format!("protocol = \"{PROTOCOL}\"\ndisplay = \" \"\nentry = \"e.py\"\n"),
            ),
        ] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
            std::fs::write(root.path().join(name).join(ENGINE_FILE), text).unwrap();
            assert!(packages.find(name).is_err(), "{name}");
        }
    }

    #[test]
    fn new_accounts_take_the_default_only_when_it_is_installed() {
        let root = tempfile::tempdir().unwrap();
        let packages = Packages::new(root.path().to_path_buf());
        assert_eq!(packages.default_package().unwrap(), None);
        std::fs::write(root.path().join(ENGINES_FILE), "default = \"engine-a\"\n").unwrap();
        assert_eq!(packages.default_package().unwrap(), None);
        let installed = package(root.path(), "engine-a");
        assert_eq!(packages.default_package().unwrap(), Some(installed));
    }

    #[test]
    fn an_endpoint_must_speak_this_protocol_and_carry_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let file = EndpointFile(dir.path().join("endpoint.json"));
        assert!(file.read().is_err(), "no endpoint published yet");
        let write = |value: serde_json::Value| std::fs::write(&file.0, value.to_string()).unwrap();
        write(
            serde_json::json!({"protocol": PROTOCOL, "host": "127.0.0.1", "port": 5000, "token": "t"}),
        );
        assert_eq!(file.read().unwrap().port, 5000);
        for bad in [
            serde_json::json!({"protocol": "other", "host": "127.0.0.1", "port": 5000, "token": "t"}),
            serde_json::json!({"protocol": PROTOCOL, "host": "127.0.0.1", "port": 5000, "token": ""}),
            serde_json::json!({"protocol": PROTOCOL, "host": "127.0.0.1", "port": 5000}),
            serde_json::json!({"protocol": PROTOCOL, "host": "127.0.0.1", "port": 5000, "token": "t", "nonce": "x"}),
        ] {
            write(bad.clone());
            assert!(file.read().is_err(), "{bad}");
        }
    }
}
