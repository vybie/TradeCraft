use super::{Interval, Provider, TradeCraft};
use chrono::{DateTime, Utc};
use egui::{Color32, RichText};

const SURFACE: Color32 = Color32::from_rgb(19, 23, 34);
const PLOT: Color32 = Color32::from_rgb(19, 23, 34);
const DIVIDER: Color32 = Color32::from_rgb(43, 48, 62);
const TEXT: Color32 = Color32::from_rgb(220, 223, 230);
const MUTED: Color32 = Color32::from_rgb(151, 157, 171);
const BLUE: Color32 = Color32::from_rgb(83, 145, 255);

fn theme() -> egui_charts::theme::Theme {
    let mut theme = egui_charts::theme::Theme::dark();
    theme.ui.panel_bg = SURFACE;
    theme.ui.panel_bg_secondary = SURFACE;
    theme.ui.panel_bg_floating = Color32::from_rgb(30, 34, 45);
    theme.ui.border = DIVIDER;
    theme.ui.border_control = DIVIDER;
    theme.ui.text = TEXT;
    theme.ui.text_secondary = MUTED;
    theme.ui.text_muted = MUTED;
    theme.ui.accent = BLUE;
    theme.chart.bg = PLOT;
    theme.chart.bg_axis = PLOT;
    theme.chart.grid_line = Color32::from_rgb(35, 39, 50);
    theme.chart.grid_line_major = DIVIDER;
    theme.chart.axis_text = MUTED;
    theme.chart.candle_up = Color32::from_rgb(38, 166, 154);
    theme.chart.candle_down = Color32::from_rgb(239, 83, 80);
    theme
}

impl TradeCraft {
    fn indicator_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(300.0);
        ui.strong("Indicators");
        ui.add_space(5.0);
        ui.label(RichText::new("Choose settings and add to chart").color(MUTED));
        ui.separator();
        ui.horizontal(|ui| {
            let label = ui.label("SMA length");
            ui.add(egui::DragValue::new(&mut self.sma_period).range(1..=1000))
                .labelled_by(label.id);
            if ui.button("Add SMA").clicked() {
                self.add_indicator(0);
            }
        });
        ui.horizontal(|ui| {
            let label = ui.label("EMA length");
            ui.add(egui::DragValue::new(&mut self.ema_period).range(1..=1000))
                .labelled_by(label.id);
            if ui.button("Add EMA").clicked() {
                self.add_indicator(1);
            }
        });
        ui.horizontal(|ui| {
            let label = ui.label("RSI length");
            ui.add(egui::DragValue::new(&mut self.rsi_period).range(1..=1000))
                .labelled_by(label.id);
            if ui.button("Add RSI").clicked() {
                self.add_indicator(2);
            }
        });
        ui.horizontal(|ui| {
            let fast = ui.label("Fast");
            ui.add(egui::DragValue::new(&mut self.macd_fast).range(1..=1000))
                .labelled_by(fast.id);
            let slow = ui.label("Slow");
            ui.add(egui::DragValue::new(&mut self.macd_slow).range(1..=1000))
                .labelled_by(slow.id);
            let signal = ui.label("Signal");
            ui.add(egui::DragValue::new(&mut self.macd_signal).range(1..=1000))
                .labelled_by(signal.id);
            if ui.button("Add MACD").clicked() {
                self.add_indicator(3);
            }
        });
        ui.horizontal(|ui| {
            let length = ui.label("Bands length");
            ui.add(egui::DragValue::new(&mut self.bands_period).range(1..=1000))
                .labelled_by(length.id);
            let deviation = ui.label("Deviation");
            ui.add(
                egui::DragValue::new(&mut self.bands_deviation)
                    .range(0.1..=10.0)
                    .speed(0.1),
            )
            .labelled_by(deviation.id);
            if ui.button("Add Bands").clicked() {
                self.add_indicator(4);
            }
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(5.0);
            ui.label(RichText::new("TC").strong().color(BLUE).size(16.0));
            ui.separator();
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.symbol)
                    .desired_width(82.0)
                    .hint_text("Symbol"),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Go").clicked() || enter {
                self.connect();
            }
            ui.separator();
            for interval in [
                Interval::OneMinute,
                Interval::FiveMinutes,
                Interval::FifteenMinutes,
                Interval::OneDay,
            ] {
                if ui
                    .selectable_label(self.interval == interval, interval.label())
                    .clicked()
                    && self.interval != interval
                {
                    self.interval = interval;
                    self.chart.set_timeframe(interval.timeframe());
                    if let Some(token) = self.active_token.clone() {
                        self.start(self.active_symbol.clone(), token);
                    }
                }
            }
            ui.separator();
            let style = match self.chart.chart_type() {
                egui_charts::ChartType::Bars => "Bars",
                egui_charts::ChartType::Line => "Line",
                _ => "Candles",
            };
            ui.menu_button(style, |ui| {
                for (label, kind) in [
                    ("Candles", egui_charts::ChartType::Candles),
                    ("Bars", egui_charts::ChartType::Bars),
                    ("Line", egui_charts::ChartType::Line),
                ] {
                    if ui
                        .selectable_label(self.chart.chart_type() == kind, label)
                        .clicked()
                    {
                        self.chart.set_chart_type(kind);
                        ui.close();
                    }
                }
            });
            ui.separator();
            ui.menu_button("Indicators", |ui| self.indicator_menu(ui));
            ui.separator();
            if ui.button("Settings").clicked() {
                self.draft_settings = self.settings;
                self.settings_message.clear();
                self.token.clear();
                self.settings_open = true;
            }
        });
    }

    fn watchlist_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.strong("Watchlist");
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.watchlist_entry)
                    .desired_width(112.0)
                    .hint_text("Add symbol"),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Add").clicked() || enter {
                let symbol = self.watchlist_entry.trim().to_ascii_uppercase();
                if !symbol.is_empty()
                    && symbol
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '/'))
                    && !self.watchlist.contains(&symbol)
                {
                    self.watchlist.push(symbol);
                    self.watchlist_entry.clear();
                }
            }
        });
        ui.add_space(7.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Symbol").small().color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                ui.label(RichText::new("Last").small().color(MUTED));
            });
        });
        ui.add_space(4.0);
        let mut selected = None;
        let mut remove = None;
        for (index, symbol) in self.watchlist.iter().enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(5.0);
                    if ui
                        .selectable_label(
                            self.active_symbol == *symbol
                                || (self.active_symbol.is_empty() && self.symbol == *symbol),
                            symbol,
                        )
                        .clicked()
                    {
                        selected = Some(symbol.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("×")
                            .on_hover_text("Remove from watchlist")
                            .clicked()
                        {
                            remove = Some(index);
                        }
                        let value = if self.active_symbol == *symbol {
                            self.last
                                .map(|v| format!("{v:.2}"))
                                .unwrap_or_else(|| "—".into())
                        } else {
                            "—".into()
                        };
                        ui.label(RichText::new(value).color(TEXT));
                    });
                });
            });
        }
        if let Some(index) = remove {
            self.watchlist.remove(index);
        }
        if let Some(symbol) = selected {
            self.symbol = symbol;
            if self.worker.is_some() {
                self.connect();
            }
        }
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Price shown for active chart only")
                    .small()
                    .color(MUTED),
            );
        });
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.strong(if self.active_symbol.is_empty() {
                &self.symbol
            } else {
                &self.active_symbol
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new(match self.settings.provider {
                    Provider::Yahoo => "Yahoo · delayed",
                    Provider::Tradier => "Tradier market data",
                })
                .small()
                .color(MUTED),
            );
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            match self.last {
                Some(value) => {
                    ui.label(RichText::new(format!("{value:.2}")).size(22.0).strong());
                }
                None => {
                    ui.label(RichText::new("No current price").color(MUTED));
                }
            }
        });
        if let Some(ms) = self
            .last_trade_ms
            .and_then(DateTime::<Utc>::from_timestamp_millis)
        {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!(
                        "Last update {}",
                        ms.with_timezone(&chrono::Local).format("%-I:%M:%S %p")
                    ))
                    .small()
                    .color(MUTED),
                );
            });
        }
    }
}

impl TradeCraft {
    fn settings_page(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("settings_header")
            .exact_size(46.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    if ui.button("Back to chart").clicked() {
                        self.token.clear();
                        self.settings_open = false;
                    }
                    ui.separator();
                    ui.strong("Settings");
                });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(26.0);
            ui.horizontal(|ui| {
                ui.add_space(28.0);
                ui.vertical(|ui| {
                    ui.set_max_width(620.0);
                    ui.heading("Market data");
                    ui.add_space(5.0);
                    ui.label(RichText::new("Choose how TradeCraft loads chart prices.").color(MUTED));
                    ui.add_space(18.0);
                    ui.strong("Data provider");
                    ui.add_space(6.0);
                    ui.radio_value(&mut self.draft_settings.provider, Provider::Yahoo, "Yahoo Finance — free, delayed");
                    ui.radio_value(&mut self.draft_settings.provider, Provider::Tradier, "Tradier — account required");
                    ui.add_space(10.0);
                    match self.draft_settings.provider {
                        Provider::Yahoo => {
                            ui.label("No account or API key is needed. Chart bars are delayed by at least 15 minutes and refresh every minute.");
                            ui.add_space(5.0);
                            ui.label(RichText::new("Unofficial source; availability and coverage can change. Yahoo restricts automated access, so data rights need review before distribution.").color(MUTED));
                        }
                        Provider::Tradier => {
                            ui.label("Connect your own Tradier production API token for market data.");
                            ui.add_space(10.0);
                            #[cfg(feature = "vibestrader-credential")]
                            ui.checkbox(&mut self.draft_settings.use_vibestrader, "Use the existing VibesTrader credential on this computer");
                            if !cfg!(feature = "vibestrader-credential") || !self.draft_settings.use_vibestrader {
                                ui.add_space(8.0);
                                let label = ui.label("Tradier production token");
                                ui.add(egui::TextEdit::singleline(&mut self.token).password(true).desired_width(340.0).hint_text("Enter token or leave blank to keep saved token")).labelled_by(label.id);
                                ui.label(RichText::new("Saved in Windows Credential Locker. Leave blank to keep your existing token.").small().color(MUTED));
                            }
                        }
                    }
                    ui.add_space(20.0);
                    if !self.settings_message.is_empty() { ui.colored_label(egui::Color32::from_rgb(255, 120, 120), &self.settings_message); ui.add_space(5.0); }
                    ui.horizontal(|ui| {
                        if ui.button("Save settings").clicked() { self.save_settings(); }
                        if ui.button("Cancel").clicked() { self.token.clear(); self.settings_open = false; }
                    });
                });
            });
        });
    }

    fn show_ui(&mut self, ui: &mut egui::Ui) {
        while let Ok(event) = self.rx.try_recv() {
            self.event(event);
        }
        egui_charts::theme::apply_to_egui(ui.ctx(), &theme());
        ui.set_style(ui.ctx().global_style());
        if self.settings_open {
            self.settings_page(ui);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        egui::Panel::top("toolbar").exact_size(42.0).show(ui, |ui| {
            ui.add_space(7.0);
            self.top_bar(ui);
        });
        egui::Panel::bottom("status")
            .exact_size(32.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui
                        .checkbox(&mut self.extended_hours, "Extended hours")
                        .on_hover_text("Include premarket and after-hours prices")
                        .changed()
                    {
                        if let Some(token) = self.active_token.clone() {
                            self.start(self.active_symbol.clone(), token);
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new(&self.status).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(chrono::Local::now().format("%-I:%M:%S %p").to_string())
                                .color(MUTED),
                        );
                        ui.label(RichText::new("Local").color(MUTED));
                    });
                });
            });
        egui::Panel::right("watchlist")
            .default_size(240.0)
            .min_size(200.0)
            .resizable(true)
            .show(ui, |ui| self.watchlist_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if !self.indicators.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(7.0);
                    let mut remove = None;
                    for (index, (id, label)) in self.indicators.iter().enumerate() {
                        ui.push_id(id.0, |ui| {
                            if ui
                                .small_button(format!("{label} ×"))
                                .on_hover_text("Remove indicator")
                                .clicked()
                            {
                                remove = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove {
                        self.remove_indicator(index);
                    }
                });
            }
            if self.active_symbol.is_empty() {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Select a symbol and press Go to load the chart.").color(MUTED),
                );
            } else {
                self.chart.show(ui);
            }
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

impl eframe::App for TradeCraft {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_ui(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_charts::model::Bar;
    use egui_charts::widget::SeriesId;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn toolbar_and_watchlist_controls_work() {
        let app = TradeCraft::new();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(|ui, app: &mut TradeCraft| app.show_ui(ui), app);
        harness.step();
        harness.get_by_label("15m").click();
        harness.step();
        assert_eq!(harness.state().interval, Interval::FifteenMinutes);
        harness.get_by_label("1D").click();
        harness.step();
        assert_eq!(harness.state().interval, Interval::OneDay);
        harness.get_by_label("SPY").click();
        harness.step();
        assert_eq!(harness.state().symbol, "SPY");
        harness.get_by_label("Indicators").click();
        harness.run();
        harness.get_by_label("Add SMA").click();
        harness.run();
        assert_eq!(harness.state().indicators.len(), 1);
    }

    #[test]
    fn settings_page_opens_and_cancels_without_changing_provider() {
        let mut app = TradeCraft::new();
        app.settings = super::super::Settings::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(|ui, app: &mut TradeCraft| app.show_ui(ui), app);
        harness.step();
        harness.get_by_label("Settings").click();
        harness.run();
        assert!(harness.state().settings_open);
        harness.get_by_label("Tradier — account required").click();
        harness.run();
        assert_eq!(harness.state().draft_settings.provider, Provider::Tradier);
        assert_eq!(
            harness
                .query_by_label("Use the existing VibesTrader credential on this computer")
                .is_some(),
            cfg!(feature = "vibestrader-credential")
        );
        assert!(harness.query_by_label("Tradier production token").is_some());
        harness.get_by_label("Cancel").click();
        harness.run();
        assert_eq!(harness.state().settings.provider, Provider::Yahoo);
    }

    #[test]
    #[ignore = "manual headless visual inspection"]
    fn render_settings_layout() {
        for provider in [Provider::Yahoo, Provider::Tradier] {
            let mut app = TradeCraft::new();
            app.settings_open = true;
            app.draft_settings.provider = provider;
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1200.0, 760.0))
                .wgpu()
                .build_ui_state(|ui, app: &mut TradeCraft| app.show_ui(ui), app);
            for _ in 0..3 {
                harness.step();
            }
            let name = if provider == Provider::Yahoo {
                "yahoo"
            } else {
                "tradier"
            };
            harness
                .render()
                .expect("headless render")
                .save(format!("target/settings-{name}.png"))
                .expect("save preview");
        }
    }

    #[test]
    #[ignore = "manual headless visual inspection"]
    fn render_reference_layout() {
        for (width, height) in [(1440.0, 900.0), (768.0, 700.0)] {
            let mut app = TradeCraft::new();
            app.symbol = "SAMPLE".into();
            app.active_symbol = "SAMPLE".into();
            app.chart.set_symbol("SAMPLE");
            app.status = "Example data".into();
            let start = DateTime::<Utc>::from_timestamp(1_751_376_600, 0).unwrap();
            let bars = (0..160)
                .map(|i| {
                    let open = 100.0 + (i as f64 / 8.0).sin() * 4.0 + i as f64 * 0.04;
                    let close = open + (i as f64 * 1.3).sin() * 0.8;
                    Bar::new(
                        start + chrono::Duration::minutes(i * 5),
                        open,
                        open.max(close) + 0.3,
                        open.min(close) - 0.3,
                        close,
                        100.0,
                    )
                })
                .collect();
            app.chart.set_data(SeriesId::MAIN, bars);
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, height))
                .wgpu()
                .build_ui_state(|ui, app: &mut TradeCraft| app.show_ui(ui), app);
            for _ in 0..3 {
                harness.step();
            }
            harness
                .render()
                .expect("headless render")
                .save(format!("target/ui-preview-{}.png", width as u32))
                .expect("save preview");
        }
    }
}
