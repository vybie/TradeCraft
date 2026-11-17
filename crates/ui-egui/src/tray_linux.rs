//! Linux StatusNotifierItem tray, with a visible-window fallback when the host goes away.
use ksni::blocking::TrayMethods;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, SyncSender, sync_channel},
};

#[derive(Clone, Copy)]
enum Action {
    Open,
    Quit,
}

struct Item {
    tx: SyncSender<Action>,
    ctx: egui::Context,
    online: Arc<AtomicBool>,
}

impl Item {
    fn send(&self, action: Action) {
        if self.tx.try_send(action).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "tradecraft".into()
    }
    fn title(&self) -> String {
        "TradeCraft".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let mut data =
            include_bytes!("../../../apps/tradecraft/assets/tradecraft-64.rgba").to_vec();
        for pixel in data.chunks_exact_mut(4) {
            pixel.rotate_right(1);
        }
        vec![ksni::Icon {
            width: 64,
            height: 64,
            data,
        }]
    }
    fn activate(&mut self, _: i32, _: i32) {
        self.send(Action::Open);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open TradeCraft".into(),
                activate: Box::new(|item: &mut Self| item.send(Action::Open)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit TradeCraft".into(),
                activate: Box::new(|item: &mut Self| item.send(Action::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.online.store(false, Ordering::Release);
        self.send(Action::Open);
        self.ctx.request_repaint();
        true
    }
    fn watcher_online(&self) {
        self.online.store(true, Ordering::Release);
    }
}

pub(super) struct Tray {
    handle: ksni::blocking::Handle<Item>,
    events: Receiver<Action>,
    online: Arc<AtomicBool>,
}
impl Tray {
    pub(super) fn new(ctx: &egui::Context) -> Option<Self> {
        let (tx, events) = sync_channel(32);
        let online = Arc::new(AtomicBool::new(true));
        let item = Item {
            tx,
            ctx: ctx.clone(),
            online: online.clone(),
        };
        match item.spawn() {
            Ok(handle) => Some(Self {
                handle,
                events,
                online,
            }),
            Err(error) => {
                eprintln!("Unable to create TradeCraft tray icon: {error}");
                None
            }
        }
    }
    pub(super) fn available(&self) -> bool {
        self.online.load(Ordering::Acquire) && !self.handle.is_closed()
    }
    pub(super) fn pump(&mut self, ctx: &egui::Context) -> bool {
        if !self.available() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }
        let mut quit = false;
        for action in self.events.try_iter().take(32) {
            match action {
                Action::Open if !quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Action::Open => {}
                Action::Quit => quit = true,
            }
        }
        quit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_loss_disables_hiding_and_requests_window_recovery() {
        let (tx, rx) = sync_channel(32);
        let online = Arc::new(AtomicBool::new(true));
        let item = Item {
            tx,
            ctx: egui::Context::default(),
            online: online.clone(),
        };
        assert!(ksni::Tray::watcher_offline(&item, ksni::OfflineReason::No));
        assert!(!online.load(Ordering::Acquire));
        assert!(matches!(rx.try_recv(), Ok(Action::Open)));
        ksni::Tray::watcher_online(&item);
        assert!(online.load(Ordering::Acquire));
    }

    #[test]
    #[ignore = "requires a working desktop StatusNotifierItem host"]
    fn registers_with_desktop_tray_and_shuts_down() {
        let tray = Tray::new(&egui::Context::default()).expect("desktop tray available");
        assert!(tray.available());
        let output = std::process::Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.kde.StatusNotifierWatcher",
                "--object-path",
                "/StatusNotifierWatcher",
                "--method",
                "org.freedesktop.DBus.Properties.Get",
                "org.kde.StatusNotifierWatcher",
                "RegisteredStatusNotifierItems",
            ])
            .output()
            .expect("gdbus available");
        assert!(output.status.success());
        let output_text = String::from_utf8_lossy(&output.stdout);
        let name = output_text
            .split('\'')
            .find(|entry| entry.contains(&format!("StatusNotifierItem-{}-", std::process::id())))
            .expect("registered item name");
        let activated = std::process::Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                name,
                "--object-path",
                "/StatusNotifierItem",
                "--method",
                "org.kde.StatusNotifierItem.Activate",
                "0",
                "0",
            ])
            .output()
            .expect("activate tray icon");
        assert!(
            activated.status.success(),
            "{}",
            String::from_utf8_lossy(&activated.stderr)
        );
        assert!(matches!(
            tray.events.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(Action::Open)
        ));
        tray.handle.shutdown().wait();
        for _ in 0..100 {
            if !tray.available() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!tray.available());
    }
}
