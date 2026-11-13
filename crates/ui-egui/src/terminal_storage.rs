use egui_charts::{
    data::{FeedError, Reply},
    services::{Storage, StoreKind},
};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
#[derive(Clone, Default)]
pub struct FileStorage {
    guard: Arc<Mutex<()>>,
    pending: Arc<AtomicUsize>,
}
struct Ticket(Arc<AtomicUsize>);
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}
impl FileStorage {
    fn begin(&self) -> Option<Ticket> {
        self.pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 32).then_some(n + 1)
            })
            .ok()?;
        Some(Ticket(self.pending.clone()))
    }
}
fn folder(kind: StoreKind) -> Option<&'static str> {
    match kind {
        StoreKind::Settings => Some("settings"),
        StoreKind::Drawings => Some("drawings"),
        StoreKind::AlertPresets => Some("alert-presets"),
        StoreKind::ChartTemplates => Some("chart-templates"),
        StoreKind::StudyTemplates => Some("study-templates"),
        StoreKind::DrawingTemplates => Some("drawing-templates"),
        StoreKind::SymbolNotes => Some("symbol-notes"),
        StoreKind::Layouts => Some("layouts"),
        StoreKind::RecentSymbols => Some("recent-symbols"),
        StoreKind::Screens => Some("screens"),
        _ => None,
    }
}
fn root(kind: StoreKind) -> Result<PathBuf, FeedError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| FeedError::Other("Local application data unavailable".into()))?;
    let name = folder(kind).ok_or(FeedError::NotSupported)?;
    Ok(PathBuf::from(base)
        .join("TradeCraft")
        .join("terminal")
        .join(name))
}
fn encode(name: &str) -> Result<String, FeedError> {
    if name.len() > 120 {
        return Err(FeedError::Other("Document name is too long".into()));
    }
    let mut s = String::from("d");
    for b in name.as_bytes() {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}
fn decode(s: &str) -> Option<String> {
    let s = s.strip_prefix('d')?;
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    String::from_utf8(bytes).ok()
}
fn path(kind: StoreKind, name: &str) -> Result<PathBuf, FeedError> {
    Ok(root(kind)?.join(encode(name)?))
}
fn error(e: std::io::Error) -> FeedError {
    FeedError::Other(format!("Storage: {e}"))
}
fn read(path: &Path) -> Result<Option<String>, FeedError> {
    match std::fs::File::open(path) {
        Ok(file) => {
            let mut text = String::new();
            file.take(2_000_001)
                .read_to_string(&mut text)
                .map_err(error)?;
            if text.len() > 2_000_000 {
                return Err(FeedError::Other("Storage document too large".into()));
            }
            Ok(Some(text))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(e)),
    }
}
fn write_atomic(path: &Path, document: &str) -> Result<(), FeedError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, document).map_err(error)?;
    std::fs::rename(&tmp, path).map_err(error)
}
impl Storage for FileStorage {
    fn list(&self, kind: StoreKind, reply: Reply<Vec<String>>) {
        let Some(ticket) = self.begin() else {
            reply.send(Err(FeedError::Other("Too many storage requests".into())));
            return;
        };
        let guard = self.guard.clone();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                let _lock = guard
                    .lock()
                    .map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                let dir = root(kind)?;
                let entries = match std::fs::read_dir(dir) {
                    Ok(e) => e,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
                    Err(e) => return Err(error(e)),
                };
                let mut names = Vec::new();
                for item in entries {
                    let item = item.map_err(error)?;
                    if let Some(name) = item.file_name().to_str().and_then(decode) {
                        names.push(name);
                    }
                }
                Ok(names)
            })();
            reply.send(result);
        });
    }
    fn load(&self, kind: StoreKind, name: &str, reply: Reply<Option<String>>) {
        let Some(ticket) = self.begin() else {
            reply.send(Err(FeedError::Other("Too many storage requests".into())));
            return;
        };
        let guard = self.guard.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                let _lock = guard
                    .lock()
                    .map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                read(&path(kind, &name)?)
            })();
            reply.send(result);
        });
    }
    fn save(&self, kind: StoreKind, name: &str, document: String, reply: Reply<()>) {
        let Some(ticket) = self.begin() else {
            reply.send(Err(FeedError::Other("Too many storage requests".into())));
            return;
        };
        let guard = self.guard.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                if document.len() > 2_000_000 {
                    return Err(FeedError::Other("Storage document too large".into()));
                }
                let _lock = guard
                    .lock()
                    .map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                let path = path(kind, &name)?;
                let dir = root(kind)?;
                std::fs::create_dir_all(&dir).map_err(error)?;
                write_atomic(&path, &document)?;
                Ok(())
            })();
            reply.send(result);
        });
    }
    fn delete(&self, kind: StoreKind, name: &str, reply: Reply<()>) {
        let Some(ticket) = self.begin() else {
            reply.send(Err(FeedError::Other("Too many storage requests".into())));
            return;
        };
        let guard = self.guard.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                let _lock = guard
                    .lock()
                    .map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                match std::fs::remove_file(path(kind, &name)?) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(error(e)),
                }
            })();
            reply.send(result);
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_path_safe_and_reversible() {
        for value in ["AAPL", "../x", "a/b", "💵", ""] {
            let encoded = encode(value).expect("encode");
            assert!(!encoded.contains('/') && !encoded.contains('\\'));
            assert_eq!(decode(&encoded).as_deref(), Some(value));
        }
    }
    #[test]
    fn storage_replaces_existing_document() {
        let path = std::env::temp_dir().join(format!(
            "tradecraft-storage-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        write_atomic(&path, "first").expect("first write");
        write_atomic(&path, "second").expect("replace");
        assert_eq!(read(&path).expect("read").as_deref(), Some("second"));
        std::fs::remove_file(path).expect("cleanup");
    }
}
