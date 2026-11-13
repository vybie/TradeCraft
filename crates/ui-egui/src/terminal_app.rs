use crate::terminal_feed::Feed;
use crate::terminal_storage::FileStorage;
use egui_charts::model::Timeframe;
use egui_charts::ui::top_toolbar::TopToolbarAction;
use egui_charts_terminal::Terminal;
use std::sync::Arc;
use tradecraft_providers::settings::{Provider, Settings};
pub struct TerminalApp {
    terminal: Terminal,
    feed: Arc<Feed>,
    storage: Arc<FileStorage>,
    engine: tradecraft_engine::Engine,
    settings: Settings,
    draft: Settings,
    token: String,
    settings_open: bool,
    error: String,
    close_deadline: Option<std::time::Instant>,
    menu_icon: Option<egui::TextureHandle>,
}
impl TerminalApp {
    pub fn new() -> Self {
        let settings = Settings::load();
        let feed = Arc::new(Feed::new(settings));
        let storage = Arc::new(FileStorage::default());
        let terminal = Self::make_terminal(feed.clone(), storage.clone(), "SPX", Timeframe::Min5);
        Self {
            terminal,
            feed,
            storage,
            engine: tradecraft_engine::Engine::new(tradecraft_core::Interval::FiveMinutes),
            settings,
            draft: settings,
            token: String::new(),
            settings_open: false,
            error: String::new(),
            close_deadline: None,
            menu_icon: None,
        }
    }
    fn make_terminal(
        feed: Arc<Feed>,
        storage: Arc<FileStorage>,
        symbol: &str,
        timeframe: Timeframe,
    ) -> Terminal {
        let mut terminal = Terminal::new(feed.clone())
            .with_symbol(symbol)
            .with_timeframe(timeframe)
            .with_symbol_search(feed.clone())
            .with_quotes(feed)
            .with_storage(storage)
            .with_alerts(Arc::new(egui_charts::services::AlertEngine::new()));
        terminal.set_force_session_control(true);
        let toolbar = terminal.top_toolbar_mut();
        toolbar
            .host_menu_entries
            .push(("market-data-settings".into(), "Market data settings".into()));
        terminal.chart_mut().set_extended_hours(true);
        let mut options = terminal.chart().chart_options().clone();
        options.time_scale.timezone = egui_charts::scales::TimezoneMode::Local;
        terminal.chart_mut().set_chart_options(options);
        terminal.chart_mut().set_time_formatter(Some(Arc::new(
            egui_charts::scales::DefaultTimeFormatter {
                use_24_hour: false,
                show_seconds: false,
                date_format: None,
            },
        )));
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
                self.terminal =
                    Self::make_terminal(feed.clone(), self.storage.clone(), &symbol, timeframe);
                self.terminal.chart_mut().set_extended_hours(extended);
                self.feed = feed;
                self.error.clear()
            }
            Err(e) => self.error = e.into(),
        }
    }
}
impl eframe::App for TerminalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.show_ui(ui);
    }
}
impl TerminalApp {
    fn show_ui(&mut self, ui: &mut egui::Ui) {
        let status = self.feed.status();
        let source = match self.settings.provider {
            Provider::Yahoo => "Yahoo · delayed at least 15 min",
            Provider::Tradier => "Tradier",
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
        toolbar.host_status = Some(label);
        for action in self.terminal.show_with(ui, |ui, _, chart| {
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
        if ui.input(|i| i.viewport().close_requested()) && self.close_deadline.is_none() {
            self.close_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        }
        if let Some(deadline) = self.close_deadline {
            if self.terminal.flush_storage() || std::time::Instant::now() >= deadline {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use egui_charts::data::{Datafeed, HistoryPage, HistoryRequest, Reply};
    use egui_kittest::kittest::Queryable;
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
        h.get_by_label("TradeCraft menu").click();
        h.run_steps(3);
        h.get_by_label("Market data settings").click();
        h.run_steps(3);
        h.get_by_label("Tradier · production token").click();
        h.run_steps(3);
        assert!(h.query_by_label("Tradier token").is_some());
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
