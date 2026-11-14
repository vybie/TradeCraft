//! Windows notification-area controls, owned by the window event-loop thread.
use std::sync::mpsc::{Receiver, sync_channel};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const OPEN: &str = "tradecraft.tray.open";
const QUIT: &str = "tradecraft.tray.quit";

#[derive(Clone, Copy)]
enum Action {
    Open,
    Quit,
}

pub(super) struct Tray {
    _icon: TrayIcon,
    events: Receiver<Action>,
}

impl Tray {
    pub(super) fn new(ctx: &egui::Context) -> Option<Self> {
        match Self::create(ctx) {
            Ok(tray) => Some(tray),
            Err(error) => {
                eprintln!("Unable to create TradeCraft tray icon: {error}");
                None
            }
        }
    }

    fn create(ctx: &egui::Context) -> Result<Self, Box<dyn std::error::Error>> {
        let icon = Icon::from_rgba(
            include_bytes!("../../../apps/tradecraft/assets/tradecraft-64.rgba").to_vec(),
            64,
            64,
        )?;
        let menu = Menu::new();
        let open = MenuItem::with_id(OPEN, "Open", true, None);
        let quit = MenuItem::with_id(QUIT, "Quit TradeCraft", true, None);
        menu.append_items(&[&open, &quit])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("TradeCraft")
            .with_menu_on_left_click(false)
            .build()?;
        let (tx, events) = sync_channel(32);
        let menu_tx = tx.clone();
        let wake = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id.as_ref() {
                OPEN => Some(Action::Open),
                QUIT => Some(Action::Quit),
                _ => None,
            };
            if let Some(action) = action
                && menu_tx.try_send(action).is_ok()
            {
                wake.request_repaint();
            }
        }));
        let wake = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) && tx.try_send(Action::Open).is_ok()
            {
                wake.request_repaint();
            }
        }));
        Ok(Self {
            _icon: icon,
            events,
        })
    }

    pub(super) fn pump(&mut self, ctx: &egui::Context) -> bool {
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
