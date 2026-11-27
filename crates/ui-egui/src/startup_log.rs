//! A small append-only log in the application data folder. The Windows build has no console.

use std::io::Write;
use std::path::PathBuf;

const MAX_BYTES: u64 = 256 * 1024;

fn path() -> Option<PathBuf> {
    Some(tradecraft_providers::settings::data_dir()?.join("tradecraft.log"))
}

/// Appends a timestamped line. Failures are ignored: logging must never stop the app.
pub(crate) fn log(message: impl AsRef<str>) {
    let Some(path) = path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "{} {}",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
            message.as_ref()
        );
    }
}

/// Records panics, which a console-less build would otherwise lose.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(format!("panic: {info}"));
        previous(info);
    }));
}
