use egui_charts::{
    data::{FeedError, Reply},
    services::{Revision, SaveOutcome, Storage, StoreKind, Versioned},
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
    failures: Arc<Mutex<std::collections::HashMap<(StoreKind, String, &'static str), String>>>,
}
struct Ticket(Arc<AtomicUsize>);
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}
impl FileStorage {
    pub(crate) fn is_idle(&self) -> bool {
        self.pending.load(Ordering::Acquire) == 0
    }
    pub(crate) fn write_failures(&self) -> Vec<String> {
        self.failures.lock().map(|failures| failures.values().cloned().collect()).unwrap_or_else(|_| vec!["Could not check storage errors".into()])
    }
    fn begin(&self) -> Option<Ticket> {
        self.pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 32).then_some(n + 1)
            })
            .ok()?;
        Some(Ticket(self.pending.clone()))
    }
}
fn note_write<T>(failures: &Mutex<std::collections::HashMap<(StoreKind, String, &'static str), String>>, kind: StoreKind, name: &str, operation: &'static str, result: &Result<T, FeedError>) {
    if let Ok(mut failures) = failures.lock() {
        let key = (kind, name.to_owned(), operation);
        match result {
            Ok(_) => { failures.remove(&key); }
            Err(error) => { failures.insert(key, format!("Could not {operation} {kind:?} '{name}': {error}")); }
        }
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
    let base = tradecraft_providers::settings::data_dir()
        .ok_or_else(|| FeedError::Other("Local application data unavailable".into()))?;
    let name = folder(kind).ok_or(FeedError::NotSupported)?;
    Ok(base
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
pub(crate) fn write_atomic(path: &Path, document: &str) -> Result<(), FeedError> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(error)?;
    file.write_all(document.as_bytes()).map_err(error)?;
    file.sync_all().map_err(error)?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(error)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() { std::fs::File::open(parent).and_then(|dir| dir.sync_all()).map_err(error)?; }
    Ok(())
}
pub(crate) fn document_lock(path: &Path) -> Result<std::fs::File, FeedError> {
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_extension("lock")).map_err(error)?;
    lock.lock().map_err(error)?;
    Ok(lock)
}
fn version(document: String) -> Versioned {
    Versioned { revision: Revision(document.clone()), document }
}
fn compare_and_write(path: &Path, document: String, expected: Option<Revision>) -> Result<SaveOutcome, FeedError> {
    let _file_lock = document_lock(path)?;
    let current = read(path)?.map(version);
    if current.as_ref().map(|v| &v.revision) != expected.as_ref() { return Ok(SaveOutcome::Conflict(current)); }
    write_atomic(path, &document)?;
    Ok(SaveOutcome::Saved(Revision(document)))
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
            let result = Err(FeedError::Other("Too many storage requests".into()));
            note_write(&self.failures, kind, name, "save", &result);
            reply.send(result);
            return;
        };
        let guard = self.guard.clone();
        let failures = self.failures.clone();
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
                let _file_lock = document_lock(&path)?;
                write_atomic(&path, &document)?;
                Ok(())
            })();
            note_write(&failures, kind, &name, "save", &result);
            reply.send(result);
        });
    }
    fn delete(&self, kind: StoreKind, name: &str, reply: Reply<()>) {
        let Some(ticket) = self.begin() else {
            let result = Err(FeedError::Other("Too many storage requests".into()));
            note_write(&self.failures, kind, name, "delete", &result);
            reply.send(result);
            return;
        };
        let guard = self.guard.clone();
        let failures = self.failures.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                let _lock = guard
                    .lock()
                    .map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                let path = path(kind, &name)?;
                std::fs::create_dir_all(root(kind)?).map_err(error)?;
                let _file_lock = document_lock(&path)?;
                match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(error(e)),
                }
            })();
            note_write(&failures, kind, &name, "delete", &result);
            reply.send(result);
        });
    }
    fn load_versioned(&self, kind: StoreKind, name: &str, reply: Reply<Option<Versioned>>) {
        let Some(ticket) = self.begin() else { reply.send(Err(FeedError::Other("Too many storage requests".into()))); return; };
        let guard = self.guard.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                let _guard = guard.lock().map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                let path = path(kind, &name)?;
                std::fs::create_dir_all(root(kind)?).map_err(error)?;
                let _file_lock = document_lock(&path)?;
                Ok(read(&path)?.map(version))
            })();
            reply.send(result);
        });
    }
    fn save_versioned(&self, kind: StoreKind, name: &str, document: String, expected: Option<Revision>, reply: Reply<SaveOutcome>) {
        let Some(ticket) = self.begin() else { let result = Err(FeedError::Other("Too many storage requests".into())); note_write(&self.failures, kind, name, "save", &result); reply.send(result); return; };
        let guard = self.guard.clone();
        let failures = self.failures.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let _ticket = ticket;
            let result = (|| {
                if document.len() > 2_000_000 { return Err(FeedError::Other("Storage document too large".into())); }
                let _guard = guard.lock().map_err(|_| FeedError::Other("Storage unavailable".into()))?;
                let path = path(kind, &name)?;
                std::fs::create_dir_all(root(kind)?).map_err(error)?;
                compare_and_write(&path, document, expected)
            })();
            note_write(&failures, kind, &name, "save", &result);
            reply.send(result);
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn write_failure_is_kept_until_that_write_succeeds() {
        let storage = FileStorage::default();
        note_write(&storage.failures, StoreKind::StudyTemplates, "Mine", "save", &Err::<(), _>(FeedError::Other("disk full".into())));
        assert_eq!(storage.write_failures().len(), 1);
        note_write(&storage.failures, StoreKind::StudyTemplates, "Other", "save", &Ok::<_, FeedError>(()));
        assert_eq!(storage.write_failures().len(), 1);
        note_write(&storage.failures, StoreKind::StudyTemplates, "Mine", "save", &Ok::<_, FeedError>(()));
        assert!(storage.write_failures().is_empty());
    }
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
    #[test]
    fn competing_writes_do_not_replace_a_newer_document() {
        let path = std::env::temp_dir().join(format!("tradecraft-conflict-{}-{}.json", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()));
        let first = match compare_and_write(&path, "first".into(), None).unwrap() { SaveOutcome::Saved(revision) => revision, _ => panic!("expected save") };
        let second = compare_and_write(&path, "second".into(), Some(first.clone())).unwrap();
        assert!(matches!(second, SaveOutcome::Saved(_)));
        assert!(matches!(compare_and_write(&path, "stale".into(), Some(first)).unwrap(), SaveOutcome::Conflict(_)));
        assert_eq!(read(&path).unwrap().as_deref(), Some("second"));
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}
