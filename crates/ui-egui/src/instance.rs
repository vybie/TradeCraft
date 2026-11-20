//! Per-user GUI ownership and activation across all TradeCraft builds.
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) struct Instance {
    _lock: File,
    request: PathBuf,
    open: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

impl Instance {
    pub(crate) fn acquire() -> io::Result<Option<Self>> {
        let directory = tradecraft_providers::settings::data_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "TradeCraft application data directory is unavailable",
            )
        })?;
        Self::acquire_in(&directory)
    }

    fn acquire_in(directory: &Path) -> io::Result<Option<Self>> {
        fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("instance.lock"))?;
        let request = directory.join("instance-open-request");
        match lock.try_lock() {
            Ok(()) => Ok(Some(Self {
                _lock: lock,
                request,
                open: Arc::new(AtomicBool::new(false)),
                running: Arc::new(AtomicBool::new(true)),
                watcher: None,
            })),
            Err(std::fs::TryLockError::WouldBlock) => {
                fs::write(request, [])?;
                Ok(None)
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    pub(crate) fn watch(&mut self, ctx: egui::Context) {
        let request = self.request.clone();
        let open = Arc::clone(&self.open);
        let running = Arc::clone(&self.running);
        self.watcher = Some(thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                if fs::remove_file(&request).is_ok() {
                    open.store(true, Ordering::Release);
                    ctx.request_repaint();
                }
                thread::sleep(Duration::from_millis(200));
            }
        }));
    }

    pub(crate) fn take_open_request(&self) -> bool {
        self.open.swap(false, Ordering::AcqRel)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Instant, SystemTime};

    #[test]
    fn duplicate_launch_requests_open_and_lock_is_released() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "tradecraft-instance-{}-{unique}",
            std::process::id()
        ));
        let mut first = Instance::acquire_in(&directory).unwrap().unwrap();
        first.watch(egui::Context::default());
        assert!(Instance::acquire_in(&directory).unwrap().is_none());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !first.take_open_request() {
            assert!(
                Instant::now() < deadline,
                "activation request was not observed"
            );
            thread::sleep(Duration::from_millis(10));
        }
        drop(first);
        assert!(Instance::acquire_in(&directory).unwrap().is_some());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn child_process_attempts_duplicate() {
        let Some(directory) = std::env::var_os("TRADECRAFT_INSTANCE_TEST_DIR") else {
            return;
        };
        assert!(
            Instance::acquire_in(Path::new(&directory))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn another_process_cannot_start_and_requests_activation() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("tradecraft-child-{}-{unique}", std::process::id()));
        let mut first = Instance::acquire_in(&directory).unwrap().unwrap();
        first.watch(egui::Context::default());
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::child_process_attempts_duplicate",
            ])
            .env("TRADECRAFT_INSTANCE_TEST_DIR", &directory)
            .status()
            .unwrap();
        assert!(child.success());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !first.take_open_request() {
            assert!(
                Instant::now() < deadline,
                "child process did not request activation"
            );
            thread::sleep(Duration::from_millis(10));
        }
        drop(first);
        fs::remove_dir_all(directory).unwrap();
    }
}
