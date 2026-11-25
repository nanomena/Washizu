use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The folder every relative directory lives in — see [`set_home`].
static HOME: OnceLock<PathBuf> = OnceLock::new();

/// Keep everything this process reads and writes by a relative path — bots,
/// logs, history — under `dir`, wherever the executable is. The shell names
/// the folder when it starts the core (`headless`): a packaged core is
/// read-only, so nothing can live beside it. Takes effect once; later calls
/// are ignored.
pub fn set_home(dir: PathBuf) {
    let _ = HOME.set(dir);
}

/// Resolve a configured directory: an absolute path is used as it is, a
/// relative one lives under the home ([`set_home`]). A process that was given
/// no home — a test, an example — gets the path as written, relative to where
/// it runs. The caller creates the directory.
pub fn resolve_dir(configured: &Path) -> PathBuf {
    match HOME.get() {
        Some(home) => under(home, configured),
        None => configured.to_path_buf(),
    }
}

/// `configured` as a place under `home`, unless it names its own place.
fn under(home: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        // Without its leading `./`, so that `./logs` joins as `<home>/logs`
        // and not as `<home>/./logs`, which then shows up in every path.
        home.join(configured.strip_prefix("./").unwrap_or(configured))
    }
}

/// `url` without its query string and fragment, for log lines: queries can
/// carry tokens (OAuth codes, session ids) and logs get shared.
pub fn redact_query(url: &str) -> std::borrow::Cow<'_, str> {
    match url.find(['?', '#']) {
        Some(i) => format!("{}?{}", &url[..i], crate::schema::WITHHELD).into(),
        None => url.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_holds_every_relative_directory() {
        let home = Path::new("/data/akagi");
        assert_eq!(under(home, Path::new("./logs")), home.join("logs"));
        assert_eq!(under(home, Path::new("mjai_bot")), home.join("mjai_bot"));
        assert_eq!(under(home, Path::new("./a/b")), home.join("a").join("b"));
        let elsewhere = std::env::temp_dir().join("elsewhere");
        assert_eq!(under(home, &elsewhere), elsewhere);
    }

    #[test]
    fn redact_query_keeps_only_scheme_host_and_path() {
        assert_eq!(
            redact_query("https://example.invalid/cb?code=fake&state=x#frag"),
            "https://example.invalid/cb?<withheld>"
        );
        assert_eq!(
            redact_query("wss://example.invalid/gateway"),
            "wss://example.invalid/gateway"
        );
        assert_eq!(redact_query("example.invalid:443"), "example.invalid:443");
    }
}
