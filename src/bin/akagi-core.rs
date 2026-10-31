//! Akagi's core without a window, for a desktop shell to run. See `akagi::headless`.

//! `akagi-core engine-host --engine <dir>` runs one engine package for the
//! whole application instead; see `akagi::engine::host`.

fn main() {
    let result =
        if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("engine-host")) {
            akagi::engine::host::run(std::env::args_os().skip(1))
        } else {
            akagi::headless::run()
        };
    if let Err(e) = result {
        eprintln!("akagi-core: {e:#}");
        std::process::exit(1);
    }
}
