//! Explicit script provenance shared by every site and account.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicySource {
    /// Washizu's own pacing policy, the same for every site and engine.
    Bundled,
    Profile {
        path: PathBuf,
    },
}

impl PolicySource {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Bundled => None,
            Self::Profile { path } => Some(path),
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Bundled => "bundled pacing policy".into(),
            Self::Profile { path } => format!("profile pacing override ({})", path.display()),
        }
    }
}

/// `profile_override` must be an explicitly configured path, already resolved
/// against that profile's home. Merely finding an old delay.lua is not an
/// override. Selection never creates files or searches another account's
/// directory.
pub fn resolve_policy_source(profile_override: Option<&Path>) -> Result<PolicySource, String> {
    let Some(path) = profile_override else {
        return Ok(PolicySource::Bundled);
    };
    if path.as_os_str().is_empty() {
        return Err("empty profile pacing override".into());
    }
    Ok(PolicySource::Profile {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_account_paces_with_the_bundled_policy_unless_explicitly_overridden() {
        assert_eq!(resolve_policy_source(None).unwrap(), PolicySource::Bundled);
        let profile = Path::new("profiles/tenhou/custom.lua");
        assert_eq!(
            resolve_policy_source(Some(profile)).unwrap(),
            PolicySource::Profile {
                path: profile.to_path_buf()
            }
        );
        assert!(resolve_policy_source(Some(Path::new(""))).is_err());
    }
}
