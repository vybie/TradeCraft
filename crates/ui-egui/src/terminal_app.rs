use crate::terminal_feed::Feed;
use crate::terminal_storage::FileStorage;
use crate::window_settings::WindowSettings;
use egui_charts::model::Timeframe;
use egui_charts::ui::top_toolbar::TopToolbarAction;
use egui_charts_terminal::Terminal;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tradecraft_providers::settings::{Provider, Settings};

#[derive(Debug)]
enum QuitState {
    Idle,
    Saving {
        started: Instant,
        observed_ready: bool,
    },
    Attention {
        details: Vec<String>,
        confirm_discard: bool,
    },
    Closing,
}

impl QuitState {
    fn saving() -> Self {
        Self::Saving {
            started: Instant::now(),
            observed_ready: false,
        }
    }
    fn observe(&mut self, now: Instant, complete: bool, errors: Vec<String>) -> bool {
        let Self::Saving {
            started,
            observed_ready,
        } = self
        else {
            return false;
        };
        if complete && errors.is_empty() {
            if *observed_ready {
                *self = Self::Closing;
                return true;
            }
            *observed_ready = true;
            return false;
        }
        *observed_ready = false;
        if !errors.is_empty() || now.saturating_duration_since(*started) >= Duration::from_secs(3) {
            *self = Self::Attention {
                details: errors,
                confirm_discard: false,
            };
        }
        false
    }
}
fn alerts_path() -> Result<std::path::PathBuf, String> {
    tradecraft_providers::settings::data_dir()
        .map(|p| p.join("alerts.json"))
        .ok_or_else(|| "Alert storage is unavailable".into())
}
fn load_alerts() -> Result<Vec<egui_charts::model::Alert>, String> {
    let path = alerts_path()?;
    match std::fs::read(path) {
        Ok(data) => {
            serde_json::from_slice(&data).map_err(|e| format!("Could not read saved alerts: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Could not read saved alerts: {e}")),
    }
}
fn save_alerts(json: &str, expected: &str) -> Result<(), String> {
    let path = alerts_path()?;
    let parent = path
        .parent()
        .ok_or("Alert storage directory is unavailable")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("Could not save alerts: {e}"))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|e| format!("Alert storage is busy: {e}"))?;
    let current = match std::fs::read(&path) {
        Ok(bytes) => {
            let alerts: Vec<egui_charts::model::Alert> = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Could not read saved alerts: {e}"))?;
            serde_json::to_string(&alerts).map_err(|e| e.to_string())?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "[]".to_owned(),
        Err(e) => return Err(format!("Could not read saved alerts: {e}")),
    };
    if current != expected {
        return Err(
            "Alerts changed in another TradeCraft window; unsaved alerts remain in memory".into(),
        );
    }
    crate::terminal_storage::write_atomic(&path, json)
        .map_err(|e| format!("Could not save alerts: {e}"))
}
fn indicator_factory(
    library: &[crate::pine_editor::PineEntry],
) -> egui_charts::studies::IndicatorFactory {
    let mut factory = egui_charts::studies::IndicatorFactory::new();
    let cache = std::sync::Mutex::new(std::collections::HashMap::<
        String,
        crate::pine_engine::PineStudy,
    >::new());
    factory.set_saved_study_resolver(move |key| {
        let source = key.strip_prefix("tradecraft-pine-v1:")?;
        if let Ok(cache) = cache.lock() {
            if let Some(study) = cache.get(source) {
                return Some(Box::new(study.clone()) as Box<dyn egui_charts::studies::Indicator>);
            }
        }
        crate::pine_editor::validate_source(source).ok()?;
        let study = crate::pine_engine::PineStudy::new(source).ok()?;
        if !study.diagnostics().is_empty() {
            return None;
        }
        if let Ok(mut cache) = cache.lock() {
            if cache.len() < 128 {
                cache.insert(source.to_owned(), study.clone());
            }
        }
        Some(Box::new(study) as Box<dyn egui_charts::studies::Indicator>)
    });
    for entry in library {
        if crate::pine_editor::validate_source(&entry.source).is_err() {
            continue;
        }
        let Ok(study) = crate::pine_engine::PineStudy::new(&entry.source) else {
            continue;
        };
        if !study.diagnostics().is_empty() {
            continue;
        }
        factory.register_in(
            egui_charts::studies::IndicatorFactory::PERSONAL,
            format!("tradecraft-pine-library:{}", entry.id),
            move || Box::new(study.clone()),
        );
    }
    factory
}
#[cfg(windows)]
#[path = "tray.rs"]
mod tray;
pub struct TerminalApp {
    terminal: Terminal,
    feed: Arc<Feed>,
    storage: Arc<FileStorage>,
    alerts: Arc<egui_charts::services::AlertEngine>,
    saved_alerts: Option<String>,
    alert_error: String,
    engine: tradecraft_engine::Engine,
    settings: Settings,
    draft: Settings,
    token: String,
    settings_open: bool,
    error: String,
    quit: QuitState,
    window_settings: WindowSettings,
    #[cfg(windows)]
    tray: Option<tray::Tray>,
    menu_icon: Option<egui::TextureHandle>,
    pine: crate::pine_editor::PineEditor,
    pine_catalog_revision: u64,
}
impl TerminalApp {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::new_with_window_settings(WindowSettings::load())
    }
    pub(crate) fn new_with_window_settings(window_settings: WindowSettings) -> Self {
        let settings = Settings::load();
        let feed = Arc::new(Feed::new(settings));
        let storage = Arc::new(FileStorage::default());
        let loaded_alerts = load_alerts();
        let saved_alerts = loaded_alerts
            .as_ref()
            .ok()
            .and_then(|a| serde_json::to_string(a).ok());
        let alert_error = loaded_alerts.as_ref().err().cloned().unwrap_or_default();
        let alerts = Arc::new(
            egui_charts::services::AlertEngine::new()
                .with_alerts(loaded_alerts.unwrap_or_default()),
        );
        let pine = crate::pine_editor::PineEditor::new();
        let terminal = Self::make_terminal(
            feed.clone(),
            storage.clone(),
            alerts.clone(),
            &pine.library,
            "SPX",
            Timeframe::Min5,
        );
        Self {
            terminal,
            feed,
            storage,
            alerts,
            saved_alerts,
            engine: tradecraft_engine::Engine::new(tradecraft_core::Interval::FiveMinutes),
            settings,
            draft: settings,
            token: String::new(),
            settings_open: false,
            error: String::new(),
            alert_error,
            quit: QuitState::Idle,
            window_settings,
            #[cfg(windows)]
            tray: None,
            menu_icon: None,
            pine,
            pine_catalog_revision: 0,
        }
    }
    pub(crate) fn init_tray(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            self.tray = tray::Tray::new(ctx);
        }
        #[cfg(not(windows))]
        let _ = ctx;
    }
    fn make_terminal(
        feed: Arc<Feed>,
        storage: Arc<FileStorage>,
        alerts: Arc<egui_charts::services::AlertEngine>,
        pine_library: &[crate::pine_editor::PineEntry],
        symbol: &str,
        timeframe: Timeframe,
    ) -> Terminal {
        let mut terminal = Terminal::new(feed.clone())
            .with_indicator_factory(indicator_factory(pine_library))
            .with_symbol(symbol)
            .with_timeframe(timeframe)
            .with_symbol_search(feed.clone())
            .with_quotes(feed)
            .with_storage(storage)
            .with_alerts(alerts);
        terminal.set_force_session_control(true);
        terminal.set_script_manager_available(true);
        terminal.set_time_controls_in_tabs(true);
        let toolbar = terminal.top_toolbar_mut();
        toolbar
            .host_menu_entries
            .push(("market-data-settings".into(), "Market data settings".into()));
        toolbar
            .host_menu_entries
            .push(("pine-editor".into(), "Pine editor".into()));
        terminal.chart_mut().set_extended_hours(true);
        let mut options = terminal.chart().chart_options().clone();
        options.time_scale.timezone = egui_charts::scales::TimezoneMode::Local;
        terminal.chart_mut().set_chart_options(options);
        terminal
            .chart_mut()
            .set_time_formatter(Some(Arc::new(crate::ChartTimeFormatter)));
        terminal
    }
    fn save(&mut self) {
        if !self.terminal.flush_storage() {
            self.error = "Finishing workspace save. Please try again in a moment.".into();
            return;
        }
        match self.engine.save_settings(self.draft, &self.token) {
            Ok(()) => {
                let symbol = self.terminal.chart().symbol().to_owned();
                let extended = self.terminal.chart().extended_hours();
                let timeframe = self.terminal.chart().timeframe().unwrap_or(Timeframe::Min5);
                self.settings = self.draft;
                self.token.clear();
                self.settings_open = false;
                let feed = Arc::new(self.feed.reconfigure(self.settings));
                self.terminal = Self::make_terminal(
                    feed.clone(),
                    self.storage.clone(),
                    self.alerts.clone(),
                    &self.pine.library,
                    &symbol,
                    timeframe,
                );
                self.terminal.chart_mut().set_extended_hours(extended);
                self.feed = feed;
                self.error.clear()
            }
            Err(e) => self.error = e.into(),
        }
    }

    fn flush_alerts(&mut self) -> Result<(), String> {
        let previous = self.saved_alerts.as_ref().ok_or_else(|| {
            if self.alert_error.is_empty() {
                "Saved alerts could not be read".to_owned()
            } else {
                self.alert_error.clone()
            }
        })?;
        let current = serde_json::to_string(&self.alerts.alerts())
            .map_err(|e| format!("Could not save alerts: {e}"))?;
        if &current != previous {
            save_alerts(&current, previous)?;
            self.saved_alerts = Some(current);
        }
        Ok(())
    }

    fn begin_quit(&mut self, ctx: &egui::Context) {
        if matches!(self.quit, QuitState::Idle) {
            self.quit = QuitState::saving();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}
impl eframe::App for TerminalApp {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        match self.flush_alerts() {
            Ok(()) => self.alert_error.clear(),
            Err(error) => self.alert_error = error,
        }
        self.window_settings.observe(ctx);
        #[cfg(windows)]
        if let Some(tray) = &mut self.tray {
            let quit_requested = tray.pump(ctx);
            if quit_requested {
                self.begin_quit(ctx);
            }
            if ctx.input(|i| i.viewport().close_requested()) && matches!(self.quit, QuitState::Idle)
            {
                let _ = self.window_settings.flush();
                let _ = self.terminal.flush_storage();
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                return;
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !matches!(self.quit, QuitState::Closing)
        {
            self.begin_quit(ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if matches!(self.quit, QuitState::Saving { .. }) {
            let mut errors = Vec::new();
            if let Err(error) = self.window_settings.flush() {
                errors.push(error);
            }
            if let Err(error) = self.pine.flush_draft() {
                errors.push(format!("Pine draft: {error}"));
            }
            if let Err(error) = self.pine.flush_library() {
                errors.push(format!("Pine library: {error}"));
            }
            if let Err(error) = self.flush_alerts() {
                errors.push(format!("Alerts: {error}"));
            }
            let terminal_ready = self.terminal.flush_storage();
            let idle = self.storage.is_idle();
            if idle {
                errors.extend(self.storage.write_failures());
            }
            let ready = terminal_ready && idle;
            if self.quit.observe(Instant::now(), ready, errors) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        } else if matches!(self.quit, QuitState::Closing) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.show_ui(ui);
    }
}
fn update_crypto_watchlist_labels(terminal: &mut Terminal) {
    let watchlist = &mut terminal.panels_mut().watchlist;
    let symbols: Vec<_> = watchlist.symbols().map(str::to_owned).collect();
    for symbol in symbols {
        let Some(pair) = tradecraft_providers::crypto::pair(&symbol) else {
            continue;
        };
        let Some((base, quote)) = pair.split_once('/') else {
            continue;
        };
        let name = tradecraft_providers::crypto_names::cached_name(base)
            .unwrap_or_else(|| base.to_owned());
        let label = if quote == "USD" {
            name
        } else {
            format!("{name} · {quote}")
        };
        watchlist.set_label(&symbol, label);
    }
}

fn configure_session_shading(
    chart: &mut egui_charts::widget::Chart,
    overrides: &[egui_charts::model::SessionOverride],
) {
    let enabled =
        chart.extended_hours() && tradecraft_providers::crypto::pair(chart.symbol()).is_none();
    let config = chart.config_mut();
    config.show_extended_hours = enabled;
    config.session_config = enabled.then(|| egui_charts::config::SessionConfig {
        show_premarket: true,
        show_postmarket: true,
        overrides: overrides.to_vec(),
        ..egui_charts::config::SessionConfig::nyse()
    });
    config.extended_hours_color = egui::Color32::from_rgba_unmultiplied(100, 81, 60, 105);
    config.post_market_color = Some(egui::Color32::from_rgba_unmultiplied(52, 74, 132, 112));
    config.overnight_color = Some(egui::Color32::from_rgba_unmultiplied(99, 51, 137, 100));
}

#[derive(Debug, PartialEq, Eq)]
enum QuitChoice {
    None,
    Retry,
    Stay,
    Discard,
}

fn quit_attention_dialog(ctx: &egui::Context, state: &mut QuitState) -> QuitChoice {
    use egui_charts::ui_kit::buttons::Button;
    let QuitState::Attention {
        details,
        confirm_discard,
    } = state
    else {
        return QuitChoice::None;
    };
    let mut retry = false;
    let mut stay = false;
    let mut discard = false;
    egui::Window::new("Unable to finish saving")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_max_width(440.0);
            if details.is_empty() {
                ui.label(
                    "Some changes are still saving or need attention. TradeCraft is still open.",
                );
                ui.label("Return to TradeCraft if a storage conflict needs your choice.");
            } else {
                ui.label("TradeCraft could not save all changes:");
                for detail in details.iter() {
                    ui.label(detail);
                }
                ui.label("Retry, or return to TradeCraft to resolve the problem.");
            }
            ui.add_space(ui.spacing().item_spacing.y);
            if *confirm_discard {
                ui.label("Quit anyway? Unsaved changes may be lost.");
                ui.horizontal_wrapped(|ui| {
                    discard = Button::new("Quit anyway").danger().show(ui).clicked();
                    if Button::new("Go back").secondary().show(ui).clicked() {
                        *confirm_discard = false;
                    }
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    retry = Button::new("Retry saving").primary().show(ui).clicked();
                    stay = Button::new("Return to TradeCraft")
                        .secondary()
                        .show(ui)
                        .clicked();
                    if Button::new("Quit without saving")
                        .ghost()
                        .show(ui)
                        .clicked()
                    {
                        *confirm_discard = true;
                    }
                });
            }
        });
    if retry {
        QuitChoice::Retry
    } else if stay {
        QuitChoice::Stay
    } else if discard {
        QuitChoice::Discard
    } else {
        QuitChoice::None
    }
}

impl TerminalApp {
    fn show_quit_attention(&mut self, ctx: &egui::Context) {
        match quit_attention_dialog(ctx, &mut self.quit) {
            QuitChoice::Retry => {
                self.terminal.retry_layout_saves();
                self.quit = QuitState::saving();
                ctx.request_repaint();
            }
            QuitChoice::Stay => {
                self.terminal.focus_pending_storage_conflict();
                self.quit = QuitState::Idle;
            }
            QuitChoice::Discard => {
                self.quit = QuitState::Closing;
                ctx.request_repaint();
            }
            QuitChoice::None => {}
        }
    }

    fn show_ui(&mut self, ui: &mut egui::Ui) {
        sync_interval_capabilities(&mut self.terminal);
        update_crypto_watchlist_labels(&mut self.terminal);
        let status = self.feed.status();
        let source = if tradecraft_providers::crypto::pair(self.terminal.chart().symbol()).is_some()
        {
            "Kraken · live crypto"
        } else {
            match self.settings.provider {
                Provider::Yahoo => "Yahoo · delayed at least 15 min",
                Provider::Tradier => "Tradier",
            }
        };
        let label = if status.is_empty() {
            source.to_owned()
        } else if self.settings.provider == Provider::Yahoo && status.starts_with("Yahoo") {
            status
        } else {
            format!("{source} · {status}")
        };
        let icon = self.menu_icon.get_or_insert_with(|| {
            ui.ctx().load_texture(
                "tradecraft-menu-icon",
                egui::ColorImage::from_rgba_unmultiplied(
                    [64, 64],
                    include_bytes!("../../../apps/tradecraft/assets/tradecraft-64.rgba"),
                ),
                egui::TextureOptions::LINEAR,
            )
        });
        let toolbar = self.terminal.top_toolbar_mut();
        toolbar.host_menu_image = Some((icon.id(), egui::vec2(24.0, 24.0)).into());
        toolbar.host_status = Some(if self.alert_error.is_empty() {
            label
        } else {
            format!("{label} · {}", self.alert_error)
        });
        let overrides = if self.settings.provider == Provider::Tradier {
            self.feed.calendar_overrides()
        } else {
            Vec::new()
        };
        for action in self.terminal.show_with(ui, |ui, _, chart| {
            configure_session_shading(chart, &overrides);
            if chart.chart_options().tooltip.enabled {
                let mut options = chart.chart_options().clone();
                options.tooltip.enabled = false;
                chart.set_chart_options(options);
            }
            chart.show(ui)
        }) {
            if matches!(action, egui_charts_terminal::Action::TopToolbar(TopToolbarAction::Host(ref id)) if id == "market-data-settings")
            {
                self.draft = self.settings;
                self.settings_open = true;
            }
            if matches!(action, egui_charts_terminal::Action::TopToolbar(TopToolbarAction::Host(ref id)) if id == "pine-editor")
            {
                self.pine.open = true;
            }
            if matches!(action, egui_charts_terminal::Action::Indicators(egui_charts::ui::top_toolbar::components::indicator_dialog::IndicatorDialogAction::ManageScripts)) {
                self.pine.new_script();
            }
            if let egui_charts_terminal::Action::Indicators(egui_charts::ui::top_toolbar::components::indicator_dialog::IndicatorDialogAction::EditScript(ref key)) = action {
                if let Some(id) = key.strip_prefix("tradecraft-pine-library:") { self.pine.edit_script(id); }
            }
            if let egui_charts_terminal::Action::Indicators(egui_charts::ui::top_toolbar::components::indicator_dialog::IndicatorDialogAction::ManageScript(ref key)) = action {
                if let Some(id) = key.strip_prefix("tradecraft-pine-library:") {
                    self.pine.confirm_delete(id);
                }
            }
        }
        self.pine.show(ui.ctx(), self.terminal.chart_mut());
        if self.pine.revision != self.pine_catalog_revision {
            self.terminal
                .set_indicator_factory(indicator_factory(&self.pine.library));
            self.pine_catalog_revision = self.pine.revision;
        }
        if self.settings_open {
            let mut open = self.settings_open;
            egui::Window::new("Market data settings")
                .open(&mut open)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.radio_value(
                        &mut self.draft.provider,
                        Provider::Yahoo,
                        "Yahoo Finance · delayed",
                    );
                    ui.radio_value(
                        &mut self.draft.provider,
                        Provider::Tradier,
                        "Tradier · production token",
                    );
                    if self.draft.provider == Provider::Tradier {
                        #[cfg(feature = "vibestrader-credential")]
                        ui.checkbox(
                            &mut self.draft.use_vibestrader,
                            "Use the existing VibesTrader credential",
                        );
                        if !self.draft.use_vibestrader {
                            let label = ui.label("Tradier token");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.token)
                                    .password(true)
                                    .hint_text("Leave blank to keep the saved token"),
                            )
                            .labelled_by(label.id);
                            ui.label("Stored in Windows Credential Locker.");
                        }
                    }
                    if !self.error.is_empty() {
                        ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
                    }
                    if ui.button("Save settings").clicked() {
                        self.save();
                    }
                });
            if !open {
                self.settings_open = false;
                self.token.clear();
            }
        }
        self.show_quit_attention(ui.ctx());
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn sync_interval_capabilities(terminal: &mut Terminal) {
    // Until the active feed reports its capabilities, don't offer speculative intervals.
    let mut capabilities = terminal
        .chart()
        .feed_capabilities()
        .cloned()
        .unwrap_or_else(|| egui_charts::data::FeedCapabilities::new().with_timeframes([]));
    if capabilities.timeframes.is_none() {
        capabilities.timeframes = Some(Vec::new());
    }
    let symbol_intervals = terminal
        .chart()
        .instrument()
        .and_then(|instrument| instrument.supported_timeframes.clone());
    let selector = &mut terminal.top_toolbar_mut().timeframe_selector;
    selector.set_feed_capabilities(&capabilities);
    selector.set_supported_timeframes(symbol_intervals);
}
#[cfg(test)]
mod tests {
    use super::*;
    use egui_charts::data::{Datafeed, HistoryPage, HistoryRequest, Reply};
    use egui_kittest::kittest::Queryable;
    #[test]
    fn quit_never_times_out_into_discarding_pending_saves() {
        let start = Instant::now();
        let mut quit = QuitState::Saving {
            started: start,
            observed_ready: false,
        };
        assert!(!quit.observe(start + Duration::from_secs(1), false, vec![]));
        assert!(matches!(quit, QuitState::Saving { .. }));
        assert!(!quit.observe(start + Duration::from_secs(3), false, vec![]));
        assert!(matches!(
            quit,
            QuitState::Attention {
                confirm_discard: false,
                ..
            }
        ));
        assert!(!quit.observe(start + Duration::from_secs(30), false, vec![]));
        assert!(matches!(quit, QuitState::Attention { .. }));
        quit = QuitState::Saving {
            started: start,
            observed_ready: false,
        };
        assert!(!quit.observe(start + Duration::from_millis(50), true, vec![]));
        assert!(quit.observe(start + Duration::from_millis(100), true, vec![]));
        assert!(matches!(quit, QuitState::Closing));
        quit = QuitState::Saving {
            started: start,
            observed_ready: false,
        };
        assert!(!quit.observe(start, true, vec!["Disk is full".into()]));
        assert!(matches!(quit, QuitState::Attention { .. }));
    }
    #[test]
    fn quitting_without_saving_requires_a_second_click() {
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                |ui, state: &mut (QuitState, QuitChoice)| {
                    state.1 = quit_attention_dialog(ui.ctx(), &mut state.0);
                },
                (
                    QuitState::Attention {
                        details: vec!["Save failed".into()],
                        confirm_discard: false,
                    },
                    QuitChoice::None,
                ),
            );
        h.run_steps(2);
        h.get_by_label("Quit without saving").click();
        h.run_steps(2);
        assert!(matches!(
            h.state().0,
            QuitState::Attention {
                confirm_discard: true,
                ..
            }
        ));
        assert_eq!(h.state().1, QuitChoice::None);
        h.get_by_label("Quit anyway").click();
        h.step();
        assert_eq!(h.state().1, QuitChoice::Discard);
    }
    #[test]
    #[ignore = "headless visual inspection"]
    fn render_quit_attention() {
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1280.0, 720.0))
            .wgpu()
            .build_ui_state(
                |ui, state: &mut QuitState| {
                    egui_charts::theme::apply_to_egui(ui.ctx(), &egui_charts::theme::Theme::dark());
                    quit_attention_dialog(ui.ctx(), state);
                },
                QuitState::Attention {
                    details: vec!["Could not save window settings: disk is full".into()],
                    confirm_discard: false,
                },
            );
        h.run_steps(3);
        h.render()
            .unwrap()
            .save(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/quit-attention-preview.png"),
            )
            .unwrap();
    }
    #[test]
    fn async_storage_finishes_quit_for_unchanged_multitab_workspace() {
        use egui_charts::services::{Storage, StoreKind};
        struct ReadonlyCopy(Arc<std::sync::Mutex<std::collections::HashMap<(StoreKind, String), String>>>);
        impl Storage for ReadonlyCopy {
            fn list(&self, _: StoreKind, reply: Reply<Vec<String>>) { reply.send(Ok(vec![])); }
            fn load(&self, kind: StoreKind, name: &str, reply: Reply<Option<String>>) { let docs = self.0.clone(); let name = name.to_owned(); std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(20)); reply.send(Ok(docs.lock().unwrap().get(&(kind, name)).cloned())); }); }
            fn save(&self, kind: StoreKind, name: &str, value: String, reply: Reply<()>) { let docs = self.0.clone(); let name = name.to_owned(); std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(20)); docs.lock().unwrap().insert((kind, name), value); reply.send(Ok(())); }); }
            fn delete(&self, _: StoreKind, _: &str, reply: Reply<()>) { reply.send(Ok(())); }
        }
        let store = Arc::new(ReadonlyCopy(Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))));
        let terminal = Terminal::new(Arc::new(OfflineFeed)).with_symbol("SPY").with_indicator_factory(indicator_factory(&[])).with_storage(store.clone());
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().build_ui_state(|ui, t: &mut Terminal| { t.show(ui); }, terminal);
        for _ in 0..20 { h.run_steps(1); std::thread::sleep(Duration::from_millis(20)); }
        h.state_mut().chart_mut().add_indicator(Box::new(crate::pine_engine::PineStudy::new("indicator(\"Fixture\")\nplot(close)").unwrap()));
        h.state_mut().run(egui_charts_terminal::Action::Layout(egui_charts::ui::top_toolbar::LayoutAction::CreateNew));
        h.state_mut().chart_mut().set_symbol("AMD");
        let mut ready = false;
        for _ in 0..50 { h.run_steps(1); ready = h.state_mut().flush_storage(); if ready { break; } std::thread::sleep(Duration::from_millis(20)); }
        assert!(ready, "unchanged workspace must finish flushing with asynchronous storage");
        let docs = store.0.lock().unwrap();
        let saved: serde_json::Value = serde_json::from_str(docs.get(&(StoreKind::Settings, "settings".into())).unwrap()).unwrap();
        assert_eq!(saved["open_workspace"]["tabs"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn failed_named_layout_save_blocks_quit_until_retry_succeeds() {
        use egui_charts::services::{Storage, StoreKind};
        use std::sync::atomic::{AtomicBool, Ordering};
        #[derive(Default)]
        struct Store {
            documents: std::sync::Mutex<std::collections::HashMap<(StoreKind, String), String>>,
            fail: AtomicBool,
        }
        impl Storage for Store {
            fn list(&self, kind: StoreKind, reply: Reply<Vec<String>>) {
                reply.send(Ok(self
                    .documents
                    .lock()
                    .unwrap()
                    .keys()
                    .filter(|(k, _)| *k == kind)
                    .map(|(_, name)| name.clone())
                    .collect()));
            }
            fn load(&self, kind: StoreKind, name: &str, reply: Reply<Option<String>>) {
                reply.send(Ok(self
                    .documents
                    .lock()
                    .unwrap()
                    .get(&(kind, name.into()))
                    .cloned()));
            }
            fn save(&self, kind: StoreKind, name: &str, document: String, reply: Reply<()>) {
                if kind == StoreKind::Layouts && self.fail.load(Ordering::Relaxed) {
                    reply.send(Err(egui_charts::data::FeedError::Other(
                        "test write failure".into(),
                    )));
                } else {
                    self.documents
                        .lock()
                        .unwrap()
                        .insert((kind, name.into()), document);
                    reply.send(Ok(()));
                }
            }
            fn delete(&self, kind: StoreKind, name: &str, reply: Reply<()>) {
                self.documents.lock().unwrap().remove(&(kind, name.into()));
                reply.send(Ok(()));
            }
        }
        let store = Arc::new(Store::default());
        store.fail.store(true, Ordering::Relaxed);
        let terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPY")
            .with_storage(store.clone());
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    t.show(ui);
                },
                terminal,
            );
        h.run_steps(6);
        h.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::Save,
        ));
        h.run_steps(4);
        assert!(
            !h.state_mut().flush_storage(),
            "failed layout write must block quit"
        );
        store.fail.store(false, Ordering::Relaxed);
        h.state_mut().retry_layout_saves();
        h.run_steps(6);
        for _ in 0..8 {
            if h.state_mut().flush_storage() {
                break;
            }
            h.run_steps(2);
        }
        assert!(h.state_mut().flush_storage());
        assert!(
            store
                .documents
                .lock()
                .unwrap()
                .contains_key(&(StoreKind::Layouts, "Unnamed".into()))
        );
        store.fail.store(true, Ordering::Relaxed);
        h.state_mut().chart_mut().set_timeframe(Timeframe::Min1);
        h.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::Save,
        ));
        h.run_steps(4);
        store.fail.store(false, Ordering::Relaxed);
        h.state_mut().chart_mut().set_timeframe(Timeframe::Min15);
        h.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::Save,
        ));
        h.run_steps(4);
        h.state_mut().retry_layout_saves();
        h.run_steps(4);
        let doc = store
            .documents
            .lock()
            .unwrap()
            .get(&(StoreKind::Layouts, "Unnamed".into()))
            .unwrap()
            .clone();
        let layout = egui_charts::widget::SavedLayout::from_json(&doc).unwrap();
        assert_eq!(
            layout.charts[0].interval,
            Some(Timeframe::Min15),
            "retry must not overwrite a newer save"
        );
    }
    #[test]
    fn switching_tabs_keeps_inflight_drawing_save_for_quit() {
        use egui_charts::services::{Storage, StoreKind};
        #[derive(Default)]
        struct Store {
            documents: std::sync::Mutex<std::collections::HashMap<(StoreKind, String), String>>,
            held: std::sync::Mutex<Option<Reply<()>>>,
        }
        impl Storage for Store {
            fn list(&self, _: StoreKind, reply: Reply<Vec<String>>) {
                reply.send(Ok(vec![]));
            }
            fn load(&self, kind: StoreKind, name: &str, reply: Reply<Option<String>>) {
                reply.send(Ok(self
                    .documents
                    .lock()
                    .unwrap()
                    .get(&(kind, name.into()))
                    .cloned()));
            }
            fn save(&self, kind: StoreKind, name: &str, document: String, reply: Reply<()>) {
                self.documents
                    .lock()
                    .unwrap()
                    .insert((kind, name.into()), document);
                if kind == StoreKind::Drawings && name == "SPY" {
                    *self.held.lock().unwrap() = Some(reply);
                } else {
                    reply.send(Ok(()));
                }
            }
            fn delete(&self, _: StoreKind, _: &str, reply: Reply<()>) {
                reply.send(Ok(()));
            }
        }
        let store = Arc::new(Store::default());
        let terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPY")
            .with_storage(store.clone());
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    t.show(ui);
                },
                terminal,
            );
        h.run_steps(8);
        let mut line = egui_charts::drawings::Drawing::new(
            1,
            egui_charts::drawings::DrawingToolType::HorizontalLine,
        );
        line.completed = true;
        line.chart_points
            .push(egui_charts::drawings::ChartPoint::new(10.0, 42.0));
        h.state_mut()
            .chart_mut()
            .user_action(|chart| chart.drawings_mut().add_drawing(line).unwrap());
        h.state_mut().flush_storage();
        assert!(store.held.lock().unwrap().is_some());
        h.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::CreateNew,
        ));
        h.state_mut().chart_mut().set_symbol("AMD");
        h.run_steps(4);
        assert!(
            !h.state_mut().flush_storage(),
            "parked drawing write is still pending"
        );
        store.held.lock().unwrap().take().unwrap().send(Ok(()));
        for _ in 0..10 {
            h.run_steps(2);
            if h.state_mut().flush_storage() {
                break;
            }
        }
        assert!(
            h.state_mut().flush_storage(),
            "the acknowledged parked drawing must settle"
        );
    }
    #[test]
    fn alert_definitions_round_trip_into_engine() {
        use egui_charts::model::{Alert, AlertCondition};
        let engine = egui_charts::services::AlertEngine::new()
            .with_alerts(vec![Alert::new("AMD", AlertCondition::CrossingUp(200.0))]);
        let json = serde_json::to_string(&engine.alerts()).unwrap();
        let decoded: Vec<Alert> = serde_json::from_str(&json).unwrap();
        let restored = egui_charts::services::AlertEngine::new().with_alerts(decoded);
        assert_eq!(engine.alerts(), restored.alerts());
    }
    #[test]
    fn workspace_restores_tabs_indicators_sessions_and_watchlist() {
        use egui_charts::services::{Storage, StoreKind};
        #[derive(Default)]
        struct Memory(std::sync::Mutex<std::collections::HashMap<(StoreKind, String), String>>);
        impl Storage for Memory {
            fn list(&self, kind: StoreKind, reply: Reply<Vec<String>>) {
                reply.send(Ok(self
                    .0
                    .lock()
                    .unwrap()
                    .keys()
                    .filter(|(k, _)| *k == kind)
                    .map(|(_, n)| n.clone())
                    .collect()));
            }
            fn load(&self, kind: StoreKind, name: &str, reply: Reply<Option<String>>) {
                reply.send(Ok(self
                    .0
                    .lock()
                    .unwrap()
                    .get(&(kind, name.to_owned()))
                    .cloned()));
            }
            fn save(&self, kind: StoreKind, name: &str, document: String, reply: Reply<()>) {
                self.0
                    .lock()
                    .unwrap()
                    .insert((kind, name.to_owned()), document);
                reply.send(Ok(()));
            }
            fn delete(&self, kind: StoreKind, name: &str, reply: Reply<()>) {
                self.0.lock().unwrap().remove(&(kind, name.to_owned()));
                reply.send(Ok(()));
            }
        }
        let store = Arc::new(Memory::default());
        let make = || {
            Terminal::new(Arc::new(OfflineFeed))
                .with_indicator_factory(indicator_factory(&[]))
                .with_symbol("SPY")
                .with_timeframe(Timeframe::Min5)
                .with_storage(store.clone())
        };
        let mut first = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    t.show(ui);
                },
                make(),
            );
        first.run_steps(8);
        first
            .state_mut()
            .chart_mut()
            .add_indicator(Box::new(egui_charts::studies::SMA::new(13)));
        first.state_mut().panels_mut().watchlist.add_symbol("NVDA");
        first.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::CreateNew,
        ));
        first.state_mut().chart_mut().set_symbol("AMD");
        first
            .state_mut()
            .chart_mut()
            .set_timeframe(Timeframe::Min15);
        first.state_mut().chart_mut().set_extended_hours(false);
        first
            .state_mut()
            .chart_mut()
            .add_indicator(Box::new(egui_charts::studies::EMA::new(21)));
        for source in [
            "indicator(\"My custom\")\nplot(close)",
            "indicator(\"My custom\")\nplot(open)",
        ] {
            first.state_mut().chart_mut().add_indicator(Box::new(
                crate::pine_engine::PineStudy::new(source).unwrap(),
            ));
        }
        first.run_steps(12);
        let mut drawing = egui_charts::drawings::Drawing::new(
            937,
            egui_charts::drawings::DrawingToolType::HorizontalLine,
        );
        drawing.completed = true;
        drawing
            .chart_points
            .push(egui_charts::drawings::ChartPoint::new(10.0, 42.0));
        first
            .state_mut()
            .chart_mut()
            .drawings_mut()
            .add_synced_drawing(drawing)
            .unwrap();
        first.run_steps(4);
        first.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::Save,
        ));
        first.run_steps(4);
        for _ in 0..10 {
            if first.state_mut().flush_storage() {
                break;
            }
            first.run_steps(2);
        }
        assert!(
            first.state_mut().flush_storage(),
            "workspace save should settle"
        );
        let documents = store.0.lock().unwrap();
        let saved = documents
            .get(&(StoreKind::Settings, "settings".into()))
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        assert!(
            saved
                .as_ref()
                .and_then(|v| v.get("open_workspace"))
                .is_some_and(|v| !v.is_null()),
            "workspace was not saved: {saved:?}"
        );
        drop(documents);
        let mut second = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    t.show(ui);
                },
                make(),
            );
        second.run_steps(12);
        assert_eq!(second.state().chart().symbol(), "AMD");
        assert_eq!(second.state().chart().timeframe(), Some(Timeframe::Min15));
        assert!(!second.state().chart().extended_hours());
        assert!(
            second
                .state()
                .chart()
                .drawings()
                .drawings
                .iter()
                .any(|drawing| drawing.chart_points.iter().any(|point| point.price == 42.0)),
            "the horizontal drawing should retain its price"
        );
        let pine_keys: Vec<_> = second
            .state()
            .chart()
            .indicators()
            .filter(|(_, s)| s.name() == "My custom")
            .map(|(_, s)| s.persistence_name())
            .collect();
        assert_eq!(pine_keys.len(), 2);
        assert_ne!(pine_keys[0], pine_keys[1]);
        second
            .state_mut()
            .chart_mut()
            .set_timeframe(Timeframe::Min1);
        second.state_mut().run(egui_charts_terminal::Action::Layout(
            egui_charts::ui::top_toolbar::LayoutAction::Open("Unnamed 2".into()),
        ));
        second.run_steps(8);
        assert_eq!(
            second.state().chart().timeframe(),
            Some(Timeframe::Min15),
            "opening the saved named layout restores its timeframe"
        );
        assert!(
            second
                .state()
                .chart()
                .indicators()
                .any(|(_, s)| s.name() == "EMA")
        );
        assert!(
            second
                .state_mut()
                .panels_mut()
                .watchlist
                .symbols()
                .any(|s| s == "NVDA")
        );
        second.get_by_label("SPY").click();
        second.run_steps(3);
        assert_eq!(second.state().chart().symbol(), "SPY");
        assert_eq!(second.state().chart().timeframe(), Some(Timeframe::Min5));
        assert!(
            second
                .state()
                .chart()
                .indicators()
                .any(|(_, s)| s.name() == "SMA")
        );
    }
    #[test]
    fn chart_tabs_add_switch_and_close_without_losing_chart_state() {
        use egui_charts::model::Instrument;
        #[derive(Default)]
        struct TabFeed(std::sync::Mutex<Vec<String>>);
        impl Datafeed for TabFeed {
            fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
                OfflineFeed.history(request, reply);
            }
            fn subscribe(&self, subscription: egui_charts::data::Subscription) {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("subscribe {}", subscription.symbol));
            }
            fn unsubscribe(&self, _: egui_charts::data::SubscriptionId) {
                self.0.lock().unwrap().push("unsubscribe".into());
            }
        }
        impl egui_charts::services::Quotes for TabFeed {
            fn subscribe(
                &self,
                symbols: &[String],
                events: egui_charts::services::Events<(String, egui_charts::model::Quote)>,
            ) {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("quotes {}", symbols.join(",")));
                for symbol in symbols {
                    let (last, previous) = if symbol == "AMD" {
                        (200.0, 250.0)
                    } else {
                        (120.0, 100.0)
                    };
                    events.push((
                        symbol.clone(),
                        egui_charts::model::Quote::new(
                            last,
                            previous,
                            chrono::DateTime::UNIX_EPOCH,
                        ),
                    ));
                }
            }
        }
        let feed = Arc::new(TabFeed::default());
        let search = Arc::new(vec![Instrument::new("SPY"), Instrument::new("AMD")]);
        let terminal = Terminal::new(feed.clone())
            .with_symbol("SPY")
            .with_timeframe(Timeframe::Min5)
            .with_symbol_search(search)
            .with_quotes(feed.clone());
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    terminal.show(ui);
                },
                terminal,
            );
        h.run_steps(4);
        h.state_mut()
            .chart_mut()
            .add_indicator(Box::new(egui_charts::studies::SMA::new(13)));
        h.run_steps(2);
        let spy_indicators = h.state().chart().indicators().count();
        h.get_by_label("New chart tab").click();
        h.run_steps(3);
        h.query_all_by_label("Search symbols")
            .last()
            .unwrap()
            .type_text("AMD");
        h.run_steps(3);
        h.get_by_label("AMD").click();
        h.run_steps(3);
        assert_eq!(h.state().chart().symbol(), "AMD");
        let tab_text = h
            .output()
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(tab_text.contains(&"120.00") && tab_text.contains(&"+20.00%"));
        assert!(tab_text.contains(&"200.00") && tab_text.contains(&"-20.00%"));
        assert_eq!(h.state_mut().panels_mut().watchlist.symbols().count(), 0);
        h.state_mut()
            .chart_mut()
            .add_indicator(Box::new(egui_charts::studies::EMA::new(21)));
        h.state_mut().chart_mut().set_timeframe(Timeframe::Min15);
        h.run_steps(2);
        h.get_by_label("SPY").click();
        h.run_steps(2);
        assert_eq!(h.state().chart().symbol(), "SPY");
        assert_eq!(h.state().chart().timeframe(), Some(Timeframe::Min5));
        assert_eq!(h.state().chart().indicators().count(), spy_indicators);
        h.get_by_label("AMD").click();
        h.run_steps(2);
        assert_eq!(h.state().chart().symbol(), "AMD");
        assert_eq!(h.state().chart().timeframe(), Some(Timeframe::Min15));
        assert_eq!(h.state().chart().indicators().count(), spy_indicators + 1);
        h.query_all_by_label("Close tab").last().unwrap().click();
        h.run_steps(2);
        assert_eq!(h.state().chart().symbol(), "SPY");
        assert!(h.query_all_by_label("Close tab").next().is_none());
        let events = feed.0.lock().unwrap();
        assert!(events.iter().any(|event| event == "subscribe SPY"));
        assert!(events.iter().any(|event| event == "subscribe AMD"));
        assert!(events.iter().any(|event| event == "unsubscribe"));
        assert!(events.iter().any(|event| event == "quotes AMD,SPY"));
    }
    #[test]
    fn new_tab_still_opens_when_current_chart_has_a_custom_pine_study() {
        use egui_charts::model::Instrument;
        let terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPY")
            .with_symbol_search(Arc::new(vec![
                Instrument::new("SPY"),
                Instrument::new("AMD"),
            ]));
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    terminal.show(ui);
                },
                terminal,
            );
        h.run_steps(4);
        let study =
            crate::pine_engine::PineStudy::new("indicator(\"Example\")\nplot(close)").unwrap();
        h.state_mut().chart_mut().add_indicator(Box::new(study));
        h.run_steps(2);
        h.get_by_label("New chart tab").click();
        h.run_steps(3);
        assert!(h.query_all_by_label("Search symbols").next().is_some());
        assert_eq!(h.query_all_by_label("Close tab").count(), 2);
    }
    #[test]
    fn session_shading_tracks_extended_hours_and_excludes_crypto() {
        let mut chart =
            egui_charts::widget::Chart::new(egui_charts::model::BarData::from_bars(Vec::new()));
        chart.set_symbol("SPY");
        chart.set_extended_hours(true);
        configure_session_shading(&mut chart, &[]);
        assert!(chart.config().show_extended_hours);
        assert_ne!(
            chart.config().post_market_color,
            Some(chart.config().extended_hours_color)
        );
        assert!(chart.config().overnight_color.is_some());
        chart.set_extended_hours(false);
        configure_session_shading(&mut chart, &[]);
        assert!(!chart.config().show_extended_hours);
        chart.set_symbol("KRAKEN:BTC/USD");
        chart.set_extended_hours(true);
        configure_session_shading(&mut chart, &[]);
        assert!(!chart.config().show_extended_hours);
    }
    #[test]
    fn session_shading_paints_separate_colors_with_early_close() {
        use egui_charts::model::{Bar, BarData, OverrideWindow, SessionOverride};
        let clock = |h, m| chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let times = [
            "2026-11-27T04:00:00Z",
            "2026-11-27T13:00:00Z",
            "2026-11-27T16:00:00Z",
            "2026-11-27T19:00:00Z",
        ];
        let bars = times
            .into_iter()
            .map(|t| Bar::new(t.parse().unwrap(), 100.0, 101.0, 99.0, 100.0, 1.0))
            .collect();
        let mut chart = egui_charts::widget::Chart::new(BarData::from_bars(bars));
        chart.set_symbol("SPY");
        chart.set_timeframe(Timeframe::Min15);
        chart.set_extended_hours(true);
        let overrides = [SessionOverride::new(date)
            .with_windows([OverrideWindow::new(clock(9, 30), clock(13, 0))])
            .with_extended_windows([
                OverrideWindow::new(clock(4, 0), clock(9, 30)),
                OverrideWindow::new(clock(13, 0), clock(20, 0)),
            ])];
        configure_session_shading(&mut chart, &overrides);
        let pre = chart.config().extended_hours_color;
        let post = chart.config().post_market_color.unwrap();
        let overnight = chart.config().overnight_color.unwrap();
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                chart.show(ui);
            });
        });
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == pre))
        );
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == post))
        );
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == overnight))
        );
        let mapping = chart.get_chart_mapping();
        for (index, expected) in [
            (0, Some(overnight)),
            (1, Some(pre)),
            (2, None),
            (3, Some(post)),
        ] {
            let x = mapping.idx_to_x(index);
            let colors = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Rect(r)
                        if r.rect.left() <= x
                            && r.rect.right() > x
                            && [pre, post, overnight].contains(&r.fill) =>
                    {
                        Some(r.fill)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(colors.first().copied(), expected, "bar {index}");
        }
        output.drop_without_applying_deltas();
    }
    #[test]
    fn stock_session_boundaries_align_with_pacific_local_time() {
        use chrono::{TimeZone, Timelike};
        for (year, month, day) in [(2026, 1, 15), (2026, 7, 15)] {
            for (ny_hour, ny_minute, la_hour, la_minute) in
                [(16, 0, 13, 0), (20, 0, 17, 0), (4, 0, 1, 0), (9, 30, 6, 30)]
            {
                let eastern = chrono_tz::America::New_York
                    .with_ymd_and_hms(year, month, day, ny_hour, ny_minute, 0)
                    .unwrap();
                let local = eastern.with_timezone(&chrono_tz::America::Los_Angeles);
                assert_eq!((local.hour(), local.minute()), (la_hour, la_minute));
            }
        }
    }
    #[test]
    fn chart_keeps_position_and_zoom_when_new_candle_arrives() {
        use egui_charts::{
            model::{Bar, BarData},
            widget::{Chart, SeriesId},
        };
        let bars: Vec<_> = (0..200)
            .map(|i| {
                Bar::new(
                    chrono::DateTime::from_timestamp(i * 60, 0).unwrap(),
                    100.0,
                    102.0,
                    99.0,
                    101.0,
                    100.0,
                )
            })
            .collect();
        for (offset, expected_shift) in [(18.0, 1.0), (5.0, 1.0), (-30.0, 0.0)] {
            let chart = Chart::new(BarData::from_bars(bars.clone()));
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1000.0, 600.0))
                .build_ui_state(
                    |ui, chart: &mut Chart| {
                        chart.show(ui);
                    },
                    chart,
                );
            harness.run_steps(2);
            harness.state_mut().scroll_to_position(offset, false);
            harness.run_steps(2);
            let before = harness.state().visible_logical_range().unwrap();
            let next = Bar::new(
                chrono::DateTime::from_timestamp(200 * 60, 0).unwrap(),
                101.0,
                103.0,
                100.0,
                102.0,
                100.0,
            );
            harness.state_mut().update(SeriesId::MAIN, next);
            harness.run_steps(2);
            let after = harness.state().visible_logical_range().unwrap();
            assert!(
                (after.right - before.right - expected_shift).abs() < 0.01,
                "offset {offset}: {before:?} -> {after:?}"
            );
            assert!(
                ((after.right - after.left) - (before.right - before.left)).abs() < 0.01,
                "zoom changed at offset {offset}"
            );
        }
    }
    #[test]
    fn crypto_watchlist_shows_names_and_keeps_market_keys() {
        let mut terminal = Terminal::new(Arc::new(OfflineFeed));
        for symbol in [
            "KRAKEN:BTC/USD",
            "KRAKEN:SOL/USD",
            "KRAKEN:SOL/USDT",
            "TSLA",
        ] {
            terminal.panels_mut().watchlist.add_symbol(symbol);
        }
        terminal.panels_mut().watchlist.set_quote(
            "KRAKEN:SOL/USD",
            egui_charts::model::Quote::new(79.86, 80.0, chrono::DateTime::UNIX_EPOCH),
        );
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    update_crypto_watchlist_labels(terminal);
                    terminal.show(ui);
                },
                terminal,
            );
        h.run_steps(4);
        assert!(h.query_all_by_label("Bitcoin").next().is_some());
        assert!(h.query_all_by_label("Solana").next().is_some());
        assert!(h.query_all_by_label("Solana · USDT").next().is_some());
        assert!(h.query_all_by_label("TSLA").next().is_some());
        let panel = &mut h.state_mut().panels_mut().watchlist;
        assert!(panel.symbols().any(|symbol| symbol == "KRAKEN:SOL/USD"));
        assert_eq!(panel.quote("KRAKEN:SOL/USD").unwrap().last, 79.86);
    }
    #[test]
    fn watchlist_accepts_quote_updates() {
        struct TestQuotes;
        impl egui_charts::services::Quotes for TestQuotes {
            fn subscribe(
                &self,
                symbols: &[String],
                events: egui_charts::services::Events<(String, egui_charts::model::Quote)>,
            ) {
                for symbol in symbols {
                    events.push((
                        symbol.clone(),
                        egui_charts::model::Quote::new(110.0, 100.0, chrono::DateTime::UNIX_EPOCH),
                    ));
                }
            }
        }
        let mut terminal = Terminal::new(Arc::new(OfflineFeed)).with_quotes(Arc::new(TestQuotes));
        terminal.panels_mut().watchlist.add_symbol("TSLA");
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    terminal.show(ui);
                },
                terminal,
            );
        h.run_steps(4);
        let quote = h
            .state_mut()
            .panels_mut()
            .watchlist
            .quote("TSLA")
            .cloned()
            .unwrap();
        assert_eq!(quote.last, 110.0);
        assert_eq!(quote.change(), 10.0);
        assert_eq!(quote.change_percent(), 10.0);
    }

    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "requires the saved personal Tradier credential and network access"]
    fn tradier_watchlist_receives_live_quotes() {
        let feed = Arc::new(Feed::new(Settings {
            provider: Provider::Tradier,
            use_vibestrader: true,
        }));
        let mut terminal = Terminal::new(Arc::new(OfflineFeed)).with_quotes(feed);
        terminal.panels_mut().watchlist.add_symbol("TSLA");
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    terminal.show(ui);
                },
                terminal,
            );
        for _ in 0..100 {
            h.step();
            if h.state_mut().panels_mut().watchlist.quote("TSLA").is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("Tradier watchlist quote did not arrive");
    }

    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "opens one live Tradier market stream using the saved personal credential"]
    fn tradier_chart_and_watchlist_share_stream() {
        let feed = Arc::new(Feed::new(Settings {
            provider: Provider::Tradier,
            use_vibestrader: true,
        }));
        let mut terminal = Terminal::new(feed.clone())
            .with_symbol("SPY")
            .with_timeframe(Timeframe::Min5)
            .with_quotes(feed.clone());
        terminal.panels_mut().watchlist.add_symbol("TSLA");
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    terminal.show(ui);
                },
                terminal,
            );
        let mut added = false;
        for _ in 0..200 {
            h.step();
            let has_tsla = h.state_mut().panels_mut().watchlist.quote("TSLA").is_some();
            let status = feed.status();
            let connected =
                status.contains("Stream connected") || status.contains("Last price update");
            if has_tsla && connected && !added {
                h.state_mut().panels_mut().watchlist.add_symbol("NVDA");
                added = true;
            }
            if added && connected && h.state_mut().panels_mut().watchlist.quote("NVDA").is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!(
            "Chart and watchlist did not connect through the shared stream: {}",
            feed.status()
        );
    }

    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "simulates reconnect while fetching real Tradier market history"]
    fn reconnect_restores_a_missing_chart_candle() {
        use egui_charts::widget::SeriesId;
        let feed = Arc::new(Feed::new(Settings {
            provider: Provider::Tradier,
            use_vibestrader: true,
        }));
        let terminal = Terminal::new(feed.clone())
            .with_symbol("TSLA")
            .with_timeframe(Timeframe::Min5)
            .with_quotes(feed.clone());
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    terminal.show(ui);
                },
                terminal,
            );
        for _ in 0..200 {
            h.step();
            let status = feed.status();
            if h.state().chart().bars(SeriesId::MAIN).len() > 20
                && (status.contains("Stream connected") || status.contains("Last price update"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let bars = h.state().chart().bars(SeriesId::MAIN).to_vec();
        assert!(bars.len() > 20, "chart history did not load");
        assert!(
            feed.status().contains("Stream connected")
                || feed.status().contains("Last price update"),
            "stream did not connect: {}",
            feed.status()
        );
        let missing = bars[bars.len() - 10].clone();
        let cutoff = bars.last().unwrap().time + chrono::Duration::minutes(5);
        let retained: Vec<_> = bars
            .into_iter()
            .filter(|bar| bar.time != missing.time)
            .collect();
        h.state_mut().chart_mut().set_data(SeriesId::MAIN, retained);
        feed.simulate_reconnect(missing.time + chrono::Duration::minutes(5), cutoff);
        for _ in 0..200 {
            h.step();
            if feed.status().contains("chart history restored")
                && let Some(restored) = h
                    .state()
                    .chart()
                    .bars(SeriesId::MAIN)
                    .iter()
                    .find(|bar| bar.time == missing.time)
            {
                assert_eq!(
                    (
                        restored.open,
                        restored.high,
                        restored.low,
                        restored.close,
                        restored.volume
                    ),
                    (
                        missing.open,
                        missing.high,
                        missing.low,
                        missing.close,
                        missing.volume
                    )
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("Missing candle was not restored: {}", feed.status());
    }
    #[test]
    fn market_data_settings_are_accessible() {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, app: &mut TerminalApp| app.show_ui(ui),
                TerminalApp::new(),
            );
        h.run_steps(3);
        assert!(!h.state().terminal.chart().chart_options().tooltip.enabled);
        for _ in 0..3 {
            h.get_by_label("TradeCraft menu").click();
            h.run_steps(3);
            if h.query_by_label("Market data settings").is_some() {
                break;
            }
        }
        h.get_by_label("Market data settings").click();
        h.run_steps(3);
        for _ in 0..3 {
            if h.query_by_label("Tradier token").is_some() {
                break;
            }
            h.get_by_label("Tradier · production token").click();
            h.run_steps(3);
        }
        assert!(h.query_by_label("Tradier token").is_some());
    }

    #[test]
    fn interval_menu_only_shows_active_feed_capabilities() {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, app: &mut TerminalApp| app.show_ui(ui),
                TerminalApp::new(),
            );
        h.run_steps(5);
        if let Some(menu) = h.query_by_label("Chart interval") {
            menu.click();
        } else {
            h.get_by_label("5 minutes").click();
        }
        h.run_steps(4);
        for label in ["1 minute", "5 minutes", "15 minutes", "1 day"] {
            assert!(
                h.query_all_by_label(label).next().is_some(),
                "missing {label}"
            );
        }
        for label in [
            "1 tick",
            "100 volume units",
            "100 milliseconds",
            "Custom interval…",
        ] {
            assert!(h.query_by_label(label).is_none(), "unexpected {label}");
        }
    }

    #[test]
    fn interval_menu_accepts_a_different_feeds_capabilities() {
        struct OtherFeed;
        impl Datafeed for OtherFeed {
            fn capabilities(&self, reply: Reply<egui_charts::data::FeedCapabilities>) {
                reply.send(Ok(egui_charts::data::FeedCapabilities::new()
                    .with_timeframes([
                        Timeframe::Min1,
                        Timeframe::Hour1,
                        "7min".parse().unwrap(),
                    ])));
            }
            fn history(&self, _: HistoryRequest, reply: Reply<HistoryPage>) {
                reply.send(Ok(HistoryPage::new(vec![], true)));
            }
        }
        let terminal = Terminal::new(Arc::new(OtherFeed))
            .with_symbol("TEST")
            .with_timeframe(Timeframe::Min1);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut Terminal| {
                    sync_interval_capabilities(terminal);
                    terminal.show(ui);
                },
                terminal,
            );
        h.run_steps(5);
        if let Some(menu) = h.query_by_label("Chart interval") {
            menu.click();
        } else {
            h.get_by_label("1 minute").click();
        }
        h.run_steps(4);
        assert!(h.query_by_label("1 hour").is_some());
        assert!(h.query_by_label("Custom interval…").is_some());
        assert!(h.query_by_label("5 minutes").is_none());
    }

    #[test]
    fn tab_strip_contains_clock_and_session_without_bottom_ranges() {
        for width in [768.0, 1440.0] {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, 760.0))
                .build_ui_state(
                    |ui, app: &mut TerminalApp| app.show_ui(ui),
                    TerminalApp::new(),
                );
            h.run_steps(4);
            let plus = h.get_by_label("New chart tab").rect();
            let session = h
                .query_by_label("Regular trading hours: off (extended hours)")
                .or_else(|| h.query_by_label("Regular trading hours: on"))
                .expect("session control is visible")
                .rect();
            let meridiem = format!(" {} ", chrono::Local::now().format("%p"));
            let timezone = h.get_by_label_contains(&meridiem).rect();
            assert!((plus.center().y - session.center().y).abs() < 8.0);
            assert!(plus.right() < timezone.left());
            assert!(session.right() <= width);
            assert!(h.query_by_label("Go to").is_none());
            assert!(h.query_by_label("1D").is_none());
            h.get_by_label_contains(&meridiem).click();
            h.run_steps(2);
            assert!(h.query_by_label("Chart timezone").is_none());
        }
    }

    #[test]
    fn chart_clock_uses_zone_abbreviation_and_daylight_saving() {
        let mut toolbar = egui_charts::ui::timeframe_toolbar::TimeframeToolbar::default();
        for (zone, instant, expected) in [
            (
                "America/Los_Angeles",
                "2026-01-15T13:00:00Z",
                "5:00:00 AM PST",
            ),
            (
                "America/Los_Angeles",
                "2026-07-15T13:00:00Z",
                "6:00:00 AM PDT",
            ),
            ("America/New_York", "2026-01-15T13:00:00Z", "8:00:00 AM EST"),
            ("UTC", "2026-01-15T13:00:00Z", "1:00:00 PM UTC"),
        ] {
            toolbar.state.timezone = zone.to_owned();
            toolbar.state.curr_time = Some(instant.parse().unwrap());
            assert_eq!(toolbar.formatted_clock(), expected);
        }
    }

    #[test]
    fn chart_settings_offer_local_and_exchange_time() {
        use egui_charts::ui::dialogs::chart_settings::SettingsDialog;
        use egui_charts::widget::settings::ChartSettingsState;
        let mut settings = ChartSettingsState::default();
        settings.timezone = "Local".to_owned();
        let mut dialog = SettingsDialog::new();
        dialog.open(settings);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 1600.0))
            .allow_missing_glyphs()
            .build_ui_state(
                |ui, dialog: &mut SettingsDialog| {
                    dialog.show(ui.ctx(), &egui_charts::theme::Theme::dark());
                },
                dialog,
            );
        h.run_steps(4);
        h.get_by_label("Timezone").scroll_to_me();
        h.run_steps(3);
        h.get_by_label("Timezone").click();
        h.run_steps(2);
        h.get_by_label("Exchange time (UTC if unknown)").click();
        h.run_steps(2);
        assert_eq!(h.state().settings().timezone, "Exchange:UTC");
        h.get_by_label("Timezone").click();
        h.run_steps(2);
        h.get_by_label("Local time").click();
        h.run_steps(2);
        assert_eq!(h.state().settings().timezone, "Local");
    }

    #[test]
    fn exchange_time_follows_instrument_and_local_time_can_be_restored() {
        use egui_charts::{
            model::{BarData, Instrument, TimezoneId},
            scales::TimezoneMode,
            widget::{Chart, settings::ChartSettingsState},
        };
        let mut chart = Chart::new(BarData::new());
        chart.set_timeframe(Timeframe::Min5);
        let mut settings = ChartSettingsState::from_chart(&chart);
        settings.timezone = "Exchange:UTC".to_owned();
        settings.apply_to_chart(&mut chart).unwrap();
        let time = "2026-01-15T13:00:00Z".parse().unwrap();
        chart.set_instrument(Some(
            Instrument::new("US").with_time_zone(TimezoneId::named("America/New_York").unwrap()),
        ));
        assert!(chart.format_time(time).contains("08:00"));
        chart.set_instrument(Some(
            Instrument::new("UK").with_time_zone(TimezoneId::named("Europe/London").unwrap()),
        ));
        assert!(chart.format_time(time).contains("13:00"));
        chart.set_instrument(None);
        assert!(chart.format_time(time).contains("13:00"));
        settings.timezone = "Local".to_owned();
        settings.apply_to_chart(&mut chart).unwrap();
        assert_eq!(
            chart.chart_options().time_scale.timezone,
            TimezoneMode::Local
        );
    }
    #[test]
    fn session_control_switches_extended_hours() {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, app: &mut TerminalApp| app.show_ui(ui),
                TerminalApp::new(),
            );
        h.run_steps(3);
        assert!(h.state().terminal.chart().extended_hours());
        h.get_by_label("Regular trading hours: off (extended hours)")
            .click();
        h.run_steps(3);
        h.get_by_label("Regular trading hours: on").click();
        h.run_steps(3);
        assert!(!h.state().terminal.chart().extended_hours());
    }

    #[test]
    #[ignore = "headless visual inspection"]
    fn render_chart_tabs_reference() {
        use egui_charts::model::Quote;
        use egui_charts::services::{Events, Quotes};
        use egui_charts::ui::top_toolbar::LayoutAction;
        struct PreviewQuotes;
        impl Quotes for PreviewQuotes {
            fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
                for symbol in symbols {
                    let (last, previous) = match symbol.as_str() {
                        "SPX" => (7538.61, 7598.64),
                        "AMD" => (162.85, 166.19),
                        _ => (754.37, 757.09),
                    };
                    events.push((
                        symbol.clone(),
                        Quote::new(last, previous, chrono::Utc::now()),
                    ));
                }
            }
        }
        let mut terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPX")
            .with_timeframe(Timeframe::Min5)
            .with_quotes(Arc::new(PreviewQuotes));
        terminal.set_force_session_control(true);
        terminal.set_time_controls_in_tabs(true);
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1440.0, 760.0))
            .wgpu()
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    t.show(ui);
                },
                terminal,
            );
        h.run_steps(4);
        h.state_mut().run(egui_charts_terminal::Action::TopToolbar(
            TopToolbarAction::ThemeToggle,
        ));
        h.run_steps(3);
        for symbol in ["SPY", "AMD"] {
            h.state_mut().run(egui_charts_terminal::Action::Layout(
                LayoutAction::CreateNew,
            ));
            h.state_mut().chart_mut().set_symbol(symbol);
            h.run_steps(4);
        }
        h.render()
            .unwrap()
            .save(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/chart-tabs-preview.png"),
            )
            .unwrap();
    }
    #[test]
    #[ignore = "headless visual inspection"]
    fn render_app_toolbar() {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .wgpu()
            .build_ui_state(
                |ui, app: &mut TerminalApp| app.show_ui(ui),
                TerminalApp::new(),
            );
        for _ in 0..4 {
            h.step();
        }
        h.render()
            .unwrap()
            .save(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/app-toolbar.png"),
            )
            .unwrap();
    }
    struct OfflineFeed;
    impl Datafeed for OfflineFeed {
        fn history(&self, _: HistoryRequest, reply: Reply<HistoryPage>) {
            let start = chrono::DateTime::<chrono::Utc>::from_timestamp(1_751_376_600, 0)
                .expect("sample timestamp");
            let bars = (0..150)
                .map(|i| {
                    let open = 100.0 + (i as f64 / 8.0).sin() * 4.0 + i as f64 * 0.04;
                    let close = open + (i as f64 / 3.0).sin();
                    egui_charts::model::Bar::new(
                        start + chrono::Duration::minutes(i * 5),
                        open,
                        open.max(close) + 0.5,
                        open.min(close) - 0.5,
                        close,
                        1000.0,
                    )
                })
                .collect();
            reply.send(Ok(HistoryPage::new(bars, true)));
        }
    }
    #[test]
    fn indicator_catalog_opens() {
        let terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPX")
            .with_symbol_search(Arc::new(vec![egui_charts::model::Instrument::new("SPX")]));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    let _ = t.show(ui);
                },
                terminal,
            );
        h.step();
        h.get_by_label("Indicators").click();
        h.step();
        assert_eq!(h.state().chart().symbol(), "SPX");
    }
    #[test]
    fn custom_pine_catalog_adds_studies_and_opens_manager() {
        let library = vec![crate::pine_editor::PineEntry {
            id: "1".into(),
            name: "My Test".into(),
            source: "indicator(\"My Test\")\nplot(close)".into(),
        }];
        let mut terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPY")
            .with_indicator_factory(indicator_factory(&library));
        terminal.set_script_manager_available(true);
        let actions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = actions.clone();
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                move |ui, terminal: &mut Terminal| {
                    observed.lock().unwrap().extend(terminal.show(ui));
                },
                terminal,
            );
        h.run_steps(3);
        h.get_by_label("Indicators").click();
        h.run_steps(3);
        h.get_by_label("Custom Pine").click();
        h.run_steps(3);
        h.get_by_label("My Test").click();
        h.run_steps(3);
        h.get_by_label("My Test").click();
        h.run_steps(3);
        assert_eq!(h.state().chart().indicators().filter(|(_, s)| s.name() == "My Test").count(), 1);
        assert!(
            h.state()
                .chart()
                .indicators()
                .any(|(_, s)| s.name() == "My Test")
        );
        h.get_by_label("+ New script").click();
        h.run_steps(3);
        assert!(actions.lock().unwrap().iter().any(|a| matches!(a, egui_charts_terminal::Action::Indicators(egui_charts::ui::top_toolbar::components::indicator_dialog::IndicatorDialogAction::ManageScripts))));
    }
    #[test]
    fn pine_row_menu_edits_the_requested_script() {
        let library = vec![crate::pine_editor::PineEntry { id: "1".into(), name: "Levels".into(), source: "indicator(\"Levels\")\nplot(close)".into() }];
        let mut terminal = Terminal::new(Arc::new(OfflineFeed)).with_symbol("SPY").with_indicator_factory(indicator_factory(&library));
        terminal.set_script_manager_available(true);
        let actions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = actions.clone();
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(move |ui, terminal: &mut Terminal| { observed.lock().unwrap().extend(terminal.show(ui)); }, terminal);
        h.run_steps(3);
        h.get_by_label("Indicators").click(); h.run_steps(3);
        h.get_by_label("Custom Pine").click(); h.run_steps(3);
        h.get_by_label("Options for Levels").click(); h.run_steps(3);
        h.get_by_label("Edit").click(); h.run_steps(3);
        assert!(actions.lock().unwrap().iter().any(|action| matches!(action,
            egui_charts_terminal::Action::Indicators(egui_charts::ui::top_toolbar::components::indicator_dialog::IndicatorDialogAction::EditScript(id)) if id == "tradecraft-pine-library:1")));
        assert!(!h.state().chart().indicators().any(|(_, study)| study.name() == "Levels"));
    }
    #[test]
    fn duplicate_pine_rows_delete_the_requested_entry_without_adding_it() {
        let library = vec![
            crate::pine_editor::PineEntry { id: "1".into(), name: "Levels".into(), source: "indicator(\"Levels\")\nplot(close)".into() },
            crate::pine_editor::PineEntry { id: "2".into(), name: "Levels".into(), source: "indicator(\"Levels\")\nplot(open)".into() },
        ];
        let mut terminal = Terminal::new(Arc::new(OfflineFeed)).with_symbol("SPY").with_indicator_factory(indicator_factory(&library));
        terminal.set_script_manager_available(true);
        let actions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = actions.clone();
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(move |ui, terminal: &mut Terminal| { observed.lock().unwrap().extend(terminal.show(ui)); }, terminal);
        h.run_steps(3);
        h.get_by_label("Indicators").click();
        h.run_steps(3);
        h.get_by_label("Custom Pine").click();
        h.run_steps(3);
        assert!(h.query_by_label("Levels (2 of 2)").is_some());
        h.get_by_label("Delete Levels (1 of 2)").click();
        h.run_steps(3);
        assert!(actions.lock().unwrap().iter().any(|action| matches!(action,
            egui_charts_terminal::Action::Indicators(egui_charts::ui::top_toolbar::components::indicator_dialog::IndicatorDialogAction::ManageScript(id)) if id == "tradecraft-pine-library:1")));
        assert!(!h.state().chart().indicators().any(|(_, study)| study.name() == "Levels"));
    }
    #[test]
    #[ignore = "headless visual inspection"]
    fn render_custom_pine_catalog() {
        let library = vec![crate::pine_editor::PineEntry {
            id: "example".into(),
            name: "Example average".into(),
            source: "indicator(\"Example average\")\nplot(close)".into(),
        }];
        let mut terminal = Terminal::new(Arc::new(OfflineFeed))
            .with_symbol("SPY")
            .with_indicator_factory(indicator_factory(&library));
        terminal.set_script_manager_available(true);
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1440.0, 900.0))
            .wgpu()
            .build_ui_state(
                |ui, t: &mut Terminal| {
                    t.show(ui);
                },
                terminal,
            );
        h.run_steps(3);
        h.get_by_label("Indicators").click();
        h.run_steps(3);
        h.get_by_label("Custom Pine").click();
        h.run_steps(3);
        h.render()
            .unwrap()
            .save(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/custom-pine-preview.png"),
            )
            .unwrap();
    }
    #[test]
    #[ignore = "headless visual inspection"]
    fn render_terminal() {
        for (width, height) in [(1440.0, 900.0), (768.0, 700.0)] {
            let terminal = Terminal::new(Arc::new(OfflineFeed))
                .with_symbol("SPX")
                .with_timeframe(Timeframe::Min5)
                .with_symbol_search(Arc::new(vec![egui_charts::model::Instrument::new("SPX")]));
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, height))
                .wgpu()
                .build_ui_state(
                    |ui, t: &mut Terminal| {
                        let _ = t.show(ui);
                    },
                    terminal,
                );
            for _ in 0..4 {
                h.step();
            }
            h.render()
                .expect("render")
                .save(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join(format!("../../target/terminal-{width}.png")),
                )
                .expect("save");
        }
    }
}
