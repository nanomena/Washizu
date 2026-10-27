//! Explicit script provenance shared by every site and account.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicySource {
    Bundled,
    Bot { path: PathBuf },
    Profile { path: PathBuf },
}

impl PolicySource {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Bundled => None,
            Self::Bot { path } | Self::Profile { path } => Some(path),
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Bundled => "bundled pacing policy".into(),
            Self::Bot { path } => format!("Bot pacing policy ({})", path.display()),
            Self::Profile { path } => format!("profile pacing override ({})", path.display()),
        }
    }
}

/// `profile_override` must be an explicitly configured path, already resolved
/// against that profile's home. Merely finding an old delay.lua is not an
/// override. A Bot entry is a portable relative path inside that Bot package.
/// Selection never creates files or searches another account's directory.
pub fn resolve_policy_source(
    bot_dir: &Path,
    bot_entry: Option<&str>,
    profile_override: Option<&Path>,
) -> Result<PolicySource, String> {
    if let Some(path) = profile_override {
        if path.as_os_str().is_empty() {
            return Err("empty profile pacing override".into());
        }
        return Ok(PolicySource::Profile {
            path: path.to_path_buf(),
        });
    }
    let Some(entry) = bot_entry else {
        return Ok(PolicySource::Bundled);
    };
    let path = Path::new(entry);
    if entry.is_empty()
        || entry.contains(['\\', ':'])
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("invalid Bot pacing policy path: {entry}"));
    }
    Ok(PolicySource::Bot {
        path: bot_dir.join(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_follow_the_same_bot_policy_unless_explicitly_overridden() {
        let bot = Path::new("bots/miai");
        let common = resolve_policy_source(bot, Some("pacing/delay.lua"), None).unwrap();
        assert_eq!(
            common,
            PolicySource::Bot {
                path: bot.join("pacing/delay.lua")
            }
        );
        let profile = Path::new("profiles/tenhou/custom.lua");
        assert_eq!(
            resolve_policy_source(bot, Some("pacing/delay.lua"), Some(profile)).unwrap(),
            PolicySource::Profile {
                path: profile.to_path_buf()
            }
        );
        assert_eq!(
            resolve_policy_source(bot, None, None).unwrap(),
            PolicySource::Bundled
        );
    }
    #[test]
    fn bot_entry_cannot_escape_the_package_or_use_platform_specific_paths() {
        for entry in [
            "",
            "../other.lua",
            "/tmp/delay.lua",
            "C:/delay.lua",
            "pacing\\delay.lua",
            "./delay.lua",
        ] {
            assert!(
                resolve_policy_source(Path::new("bot"), Some(entry), None).is_err(),
                "{entry}"
            );
        }
    }
}
