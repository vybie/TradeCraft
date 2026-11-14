#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unimplemented,
        clippy::todo,
        clippy::unreachable
    )
)]

mod terminal_app;
mod pine_engine;
mod pine_editor;
mod terminal_feed;
mod terminal_storage;
mod tradier_backfill;
mod tradier_hub;
mod tradier_watchlist;
mod ui;
mod window_settings;
use chrono::{DateTime, Utc};
use egui_charts::{
    model::{Bar, BarData, Timeframe},
    scales::{DefaultTimeFormatter, TickMarkType, TimeFormatter, TimezoneMode},
    widget::{Chart, SeriesId},
};
use market::Event;
use settings::{Provider, Settings};
use std::sync::mpsc;
use tradecraft_core::{Interval, Trade};
use tradecraft_engine::{Indicator, WorkspaceCommand};
use tradecraft_providers::{market, settings};

#[derive(Clone)]
struct ChartTimeFormatter;

impl TimeFormatter for ChartTimeFormatter {
    fn format(&self, time: DateTime<Utc>, kind: TickMarkType) -> String {
        match kind {
            TickMarkType::DayOfMonth | TickMarkType::Month => time.format("%b %-d").to_string(),
            TickMarkType::Year => time.format("%b %-d, %Y").to_string(),
            _ => self.default_formatter().format(time, kind),
        }
    }

    fn format_crosshair(&self, time: DateTime<Utc>, bar_seconds: i64) -> String {
        self.default_formatter().format_crosshair(time, bar_seconds)
    }

    fn clone_box(&self) -> Box<dyn TimeFormatter> {
        Box::new(self.clone())
    }
}

impl ChartTimeFormatter {
    fn default_formatter(&self) -> DefaultTimeFormatter {
        DefaultTimeFormatter {
            use_24_hour: false,
            show_seconds: false,
            date_format: None,
        }
    }
}

struct TradeCraft {
    symbol: String,
    active_symbol: String,
    token: String,
    draft_settings: Settings,
    settings_open: bool,
    settings_message: String,
    active_token: Option<String>,
    indicators: Vec<(SeriesId, String)>,
    sma_period: usize,
    ema_period: usize,
    rsi_period: usize,
    macd_fast: usize,
    macd_slow: usize,
    macd_signal: usize,
    bands_period: usize,
    bands_deviation: f64,
    watchlist_entry: String,
    chart: Chart,
    engine: tradecraft_engine::Engine,
    status: String,
    last: Option<f64>,
    last_trade_ms: Option<i64>,
    session: tradecraft_engine::MarketSession,
}

impl TradeCraft {
    fn new() -> Self {
        let settings = Settings::load();
        let mut engine = tradecraft_engine::Engine::new(Interval::FiveMinutes);
        let _ = engine.dispatch(tradecraft_engine::WorkspaceCommand::SetProvider {
            provider: settings.provider,
            use_vibestrader: settings.use_vibestrader,
        });
        let mut chart = Chart::new(BarData::from_bars(Vec::new()));
        chart.set_timeframe(Timeframe::Min5);
        let mut options = chart.chart_options().clone();
        options.time_scale.timezone = TimezoneMode::Local;
        chart.set_chart_options(options);
        chart.set_time_formatter(Some(std::sync::Arc::new(ChartTimeFormatter)));
        Self {
            symbol: "SPX".into(),
            active_symbol: String::new(),
            token: String::new(),
            draft_settings: settings,
            settings_open: false,
            settings_message: String::new(),
            active_token: None,
            indicators: Vec::new(),
            sma_period: 20,
            ema_period: 50,
            rsi_period: 14,
            macd_fast: 12,
            macd_slow: 26,
            macd_signal: 9,
            bands_period: 20,
            bands_deviation: 2.0,
            watchlist_entry: String::new(),
            chart,
            engine,
            status: "Disconnected".into(),
            last: None,
            last_trade_ms: None,
            session: tradecraft_engine::MarketSession::default(),
        }
    }
    fn connect(&mut self) {
        if let Err(error) =
            self.engine
                .dispatch(tradecraft_engine::WorkspaceCommand::SelectSymbol {
                    symbol: self.symbol.clone(),
                })
        {
            self.status = error.into();
            return;
        }
        let symbol = self.engine.workspace().symbol.clone();
        let token = if self.engine.workspace().settings.provider == Provider::Yahoo {
            String::new()
        } else if let Some(token) = &self.active_token {
            token.clone()
        } else {
            let result =
                tradecraft_engine::tradier_token(self.engine.workspace().settings.use_vibestrader);
            match result {
                Ok(token) => token,
                Err(error) => {
                    self.status = error.into();
                    return;
                }
            }
        };
        self.start(symbol, token);
    }
    fn start(&mut self, symbol: String, token: String) {
        if let Err(error) = self.session.start(
            self.engine.workspace().settings.provider,
            symbol.clone(),
            token.clone(),
            self.engine.workspace().extended_hours,
            self.engine.workspace().interval,
        ) {
            self.status = error.into();
            return;
        }
        self.active_symbol = symbol.clone();
        self.last = None;
        self.last_trade_ms = None;
        self.chart.set_symbol(&symbol);
        self.chart
            .set_timeframe(chart_timeframe(self.engine.workspace().interval));
        self.engine.execute(tradecraft_engine::Command::Reset {
            interval: self.engine.workspace().interval,
        });
        self.chart.set_data(SeriesId::MAIN, Vec::new());
        self.status = "Loading history…".into();
        self.active_token = Some(token.clone());
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::History(bars) => {
                if let tradecraft_engine::ChartChange::History(bars) = self
                    .engine
                    .execute(tradecraft_engine::Command::ReplaceHistory(bars))
                {
                    self.chart
                        .set_data(SeriesId::MAIN, bars.into_iter().map(chart_bar).collect());
                }
            }
            Event::Snapshot {
                price,
                timestamp_ms,
            } => {
                self.last = Some(price);
                self.last_trade_ms = Some(timestamp_ms);
            }
            Event::Status(status) => self.status = status,
            Event::Trade(trade) => {
                if self.last_trade_ms.is_none_or(|t| trade.timestamp_ms >= t) {
                    self.last = Some(trade.price);
                    self.last_trade_ms = Some(trade.timestamp_ms);
                    self.status = "Stream connected".into();
                    self.apply_trade(trade);
                }
            }
            Event::Stopped(message) => {
                self.session.stop();
                self.active_token = None;
                self.status = message;
            }
        }
    }
    fn add_indicator(&mut self, index: usize) {
        use egui_charts::studies::{BollingerBands, EMA, MACD, RSI, SMA};
        let indicator = match index {
            0 => Indicator::Sma {
                period: self.sma_period,
            },
            1 => Indicator::Ema {
                period: self.ema_period,
            },
            2 => Indicator::Rsi {
                period: self.rsi_period,
            },
            3 => Indicator::Macd {
                fast: self.macd_fast,
                slow: self.macd_slow,
                signal: self.macd_signal,
            },
            4 => Indicator::Bands {
                period: self.bands_period,
                deviation: self.bands_deviation,
            },
            _ => return,
        };
        if let Err(error) = self
            .engine
            .dispatch(WorkspaceCommand::AddIndicator { indicator })
        {
            self.status = error.into();
            return;
        }
        let (mut study, label): (Box<dyn egui_charts::studies::Indicator>, String) = match index {
            0 => (
                Box::new(SMA::new(self.sma_period)),
                format!("SMA {}", self.sma_period),
            ),
            1 => (
                Box::new(EMA::new(self.ema_period)),
                format!("EMA {}", self.ema_period),
            ),
            2 => (
                Box::new(RSI::new(self.rsi_period)),
                format!("RSI {}", self.rsi_period),
            ),
            3 => (
                Box::new(MACD::new(self.macd_fast, self.macd_slow, self.macd_signal)),
                format!(
                    "MACD {}, {}, {}",
                    self.macd_fast, self.macd_slow, self.macd_signal
                ),
            ),
            4 => (
                Box::new(BollingerBands::new(self.bands_period, self.bands_deviation)),
                format!("Bands {}, {}", self.bands_period, self.bands_deviation),
            ),
            _ => return,
        };
        if index < 2 {
            const COLORS: [egui::Color32; 6] = [
                egui::Color32::from_rgb(82, 165, 255),
                egui::Color32::from_rgb(255, 190, 92),
                egui::Color32::from_rgb(215, 131, 255),
                egui::Color32::from_rgb(92, 207, 181),
                egui::Color32::from_rgb(255, 128, 151),
                egui::Color32::from_rgb(211, 216, 124),
            ];
            study.set_colors(vec![COLORS[self.indicators.len() % COLORS.len()]]);
        }
        let id = self.chart.add_indicator(study);
        self.indicators.push((id, label));
    }
    fn remove_indicator(&mut self, index: usize) {
        if index >= self.indicators.len() {
            return;
        }
        if self
            .engine
            .dispatch(tradecraft_engine::WorkspaceCommand::RemoveIndicator { index })
            .is_err()
        {
            return;
        }
        let (id, _) = self.indicators.remove(index);
        self.chart.remove_series(id);
    }
    fn save_settings(&mut self) {
        if let Err(error) = self.engine.save_settings(self.draft_settings, &self.token) {
            self.settings_message = error.into();
            return;
        }
        self.token.clear();
        self.settings_open = false;
        self.active_token = None;
        self.connect();
    }
    fn apply_trade(&mut self, trade: Trade) {
        self.engine.set_interval(self.engine.workspace().interval);
        if let tradecraft_engine::ChartChange::Candle(bar) = self
            .engine
            .execute(tradecraft_engine::Command::ApplyTrade(trade))
        {
            self.chart.update(SeriesId::MAIN, chart_bar(bar));
        }
    }
}
fn chart_bar(bar: tradecraft_core::Bar) -> Bar {
    Bar::new(bar.time, bar.open, bar.high, bar.low, bar.close, bar.volume)
}
fn chart_timeframe(interval: Interval) -> Timeframe {
    match interval {
        Interval::OneMinute => Timeframe::Min1,
        Interval::FiveMinutes => Timeframe::Min5,
        Interval::FifteenMinutes => Timeframe::Min15,
        Interval::OneDay => Timeframe::Day1,
        Interval::OneWeek => Timeframe::Week1,
        Interval::OneMonth => Timeframe::Month1,
    }
}
pub fn run() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--commands") {
        for command in tradecraft_engine::WORKSPACE_COMMANDS {
            println!("{command}");
        }
        return Ok(());
    }
    let mut script_args = std::env::args().skip_while(|arg| arg != "--script");
    if script_args.next().is_some() {
        let Some(path) = script_args.next() else {
            eprintln!("Usage: tradecraft --script <path>");
            std::process::exit(1);
        };
        use std::io::Read;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let file = std::fs::File::open(path)?;
            let mut script = String::new();
            file.take(1_000_001).read_to_string(&mut script)?;
            let mut workspace =
                tradecraft_engine::Workspace::new(Interval::FiveMinutes, Settings::default());
            workspace.run_script(&script)?;
            println!("{}", serde_json::to_string_pretty(&workspace)?);
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Script failed: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--check-delayed") {
        check_delayed();
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--check-live") {
        check_live();
        return Ok(());
    }
    let window_settings = window_settings::WindowSettings::load();
    eframe::run_native(
        "TradeCraft",
        eframe::NativeOptions {
            viewport: window_settings
                .viewport()
                .with_min_inner_size([640.0, 480.0])
                .with_icon(std::sync::Arc::new(egui::IconData {
                    rgba: include_bytes!("../../../apps/tradecraft/assets/tradecraft-64.rgba")
                        .to_vec(),
                    width: 64,
                    height: 64,
                })),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut app = terminal_app::TerminalApp::new_with_window_settings(window_settings);
            app.init_tray(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

fn check_delayed() {
    let symbol = std::env::args().nth(2).unwrap_or_else(|| "SPX".into());
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("Unable to start the market-data runtime");
        std::process::exit(1);
    };
    let (tx, rx) = mpsc::sync_channel(8192);
    let task = runtime.spawn(market::run_yahoo(
        symbol.clone(),
        true,
        if std::env::args().any(|arg| arg == "--daily") {
            Interval::OneDay
        } else {
            Interval::FiveMinutes
        },
        tx,
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Event::Status(status)) => println!("{status}"),
            Ok(Event::History(bars)) => {
                println!("{symbol}: {} delayed bars", bars.len());
                if let Some(bar) = bars.last() {
                    println!("Latest bar UTC: {}", bar.time.to_rfc3339());
                }
                task.abort();
                return;
            }
            Ok(Event::Stopped(error)) => {
                eprintln!("{error}");
                task.abort();
                std::process::exit(1);
            }
            _ => {}
        }
    }
    eprintln!("Timed out waiting for delayed data");
    task.abort();
    std::process::exit(1);
}

fn check_live() {
    let token = match tradecraft_engine::tradier_token(Settings::load().use_vibestrader) {
        Ok(token) => token,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let mut app = TradeCraft::new();
    let extended_hours = !std::env::args().any(|arg| arg == "--regular-hours");
    let _ = app
        .engine
        .dispatch(tradecraft_engine::WorkspaceCommand::SetExtendedHours {
            enabled: extended_hours,
        });
    app.active_symbol = "SPX".into();
    app.chart.set_symbol("SPX");
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        eprintln!("Unable to start the market-data runtime");
        std::process::exit(1);
    };
    let (tx, rx) = mpsc::sync_channel(8192);
    let worker = runtime.spawn(market::run(
        "SPX".into(),
        token,
        app.engine.workspace().extended_hours,
        app.engine.workspace().interval,
        tx,
    ));
    let end = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let mut history = false;
    let mut connected = false;
    let mut trades = 0;
    while std::time::Instant::now() < end {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(event) => {
                match &event {
                    Event::History(bars) => {
                        history = !bars.is_empty();
                        let extended = bars
                            .iter()
                            .filter(|bar| {
                                let local =
                                    bar.time.with_timezone(&chrono_tz::America::New_York).time();
                                local
                                    < chrono::NaiveTime::from_hms_opt(9, 30, 0)
                                        .unwrap_or(chrono::NaiveTime::MIN)
                                    || local
                                        >= chrono::NaiveTime::from_hms_opt(16, 0, 0)
                                            .unwrap_or(chrono::NaiveTime::MIN)
                            })
                            .count();
                        println!(
                            "History: {} bars ({extended} outside standard hours)",
                            bars.len()
                        );
                    }
                    Event::Status(status) if status.starts_with("Stream connected") => {
                        connected = true;
                        println!("{status}");
                    }
                    Event::Trade(_) => trades += 1,
                    Event::Stopped(error) => {
                        eprintln!("{error}");
                        worker.abort();
                        std::process::exit(1);
                    }
                    _ => {}
                }
                app.event(event);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    worker.abort();
    println!(
        "Trade updates: {trades}; chart bars: {}",
        app.chart.bars(SeriesId::MAIN).len()
    );
    if let Some(ms) = app.last_trade_ms
        && let Some(time) = DateTime::<Utc>::from_timestamp_millis(ms)
    {
        println!("Last trade time: {}", time.to_rfc3339());
    }
    if !history || !connected {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod chart_tests {
    use super::*;
    #[test]
    fn chart_date_boundaries_show_local_month_and_day() {
        let formatter = egui_charts::scales::TimeFormatterBuilder::new()
            .with_formatter(Box::new(ChartTimeFormatter))
            .with_timezone(TimezoneMode::Timezone("America/Los_Angeles".parse().unwrap()))
            .build();
        let instant = DateTime::parse_from_rfc3339("2026-11-13T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(formatter.format(instant, TickMarkType::DayOfMonth), "Nov 13");
        assert_eq!(formatter.format(instant, TickMarkType::Time), "12:00 AM");
        assert_eq!(formatter.format(instant, TickMarkType::Year), "Nov 13, 2026");
        assert_eq!(
            formatter.format(instant - chrono::Duration::minutes(1), TickMarkType::DayOfMonth),
            "Nov 12"
        );
    }
    #[test]
    fn trades_update_the_current_bar_and_open_the_next() {
        let mut app = TradeCraft::new();
        app.event(Event::Trade(Trade {
            timestamp_ms: 1_751_376_600_000,
            price: 100.0,
            size: 2.0,
        }));
        app.event(Event::Trade(Trade {
            timestamp_ms: 1_751_376_840_000,
            price: 101.0,
            size: 3.0,
        }));
        let bars = app.chart.bars(SeriesId::MAIN);
        assert_eq!(bars.len(), 1);
        assert_eq!(
            (bars[0].open, bars[0].high, bars[0].close, bars[0].volume),
            (100.0, 101.0, 101.0, 5.0)
        );
        app.event(Event::Trade(Trade {
            timestamp_ms: 1_751_376_900_000,
            price: 102.0,
            size: 4.0,
        }));
        assert_eq!(app.chart.bars(SeriesId::MAIN).len(), 2);
    }
    #[test]
    fn extended_hours_are_on_by_default() {
        assert!(TradeCraft::new().engine.workspace().extended_hours);
    }
    #[test]
    fn daily_ticks_share_the_new_york_market_date() {
        let mut app = TradeCraft::new();
        app.engine.set_interval(Interval::OneDay);
        for timestamp_ms in [1_751_407_200_000, 1_751_418_000_000] {
            app.event(Event::Trade(Trade {
                timestamp_ms,
                price: 100.0,
                size: 1.0,
            }));
        }
        assert_eq!(app.chart.bars(SeriesId::MAIN).len(), 1);
    }
    #[test]
    fn each_interval_groups_ticks_at_its_own_boundary() {
        for interval in [
            Interval::OneMinute,
            Interval::FiveMinutes,
            Interval::FifteenMinutes,
        ] {
            let mut app = TradeCraft::new();
            app.engine.set_interval(interval);
            let start = 1_751_376_600_000i64.div_euclid(interval.seconds() * 1000)
                * interval.seconds()
                * 1000;
            for timestamp_ms in [start, start + interval.seconds() * 1000 - 1] {
                app.event(Event::Trade(Trade {
                    timestamp_ms,
                    price: 100.0,
                    size: 1.0,
                }));
            }
            assert_eq!(app.chart.bars(SeriesId::MAIN).len(), 1);
            app.event(Event::Trade(Trade {
                timestamp_ms: start + interval.seconds() * 1000,
                price: 101.0,
                size: 1.0,
            }));
            assert_eq!(app.chart.bars(SeriesId::MAIN).len(), 2);
        }
    }
    #[test]
    fn indicators_can_be_added_and_removed_without_affecting_others() {
        let mut app = TradeCraft::new();
        app.sma_period = 13;
        app.add_indicator(0);
        app.sma_period = 21;
        app.add_indicator(0);
        assert_eq!(app.chart.indicators().count(), 2);
        app.remove_indicator(0);
        assert_eq!(app.chart.indicators().count(), 1);
        assert_eq!(app.indicators[0].1, "SMA 21");
    }
    #[test]
    fn chart_uses_local_twelve_hour_time() {
        let app = TradeCraft::new();
        let timestamp = DateTime::<Utc>::from_timestamp(1_751_376_600, 0).unwrap();
        let expected = timestamp
            .with_timezone(&chrono::Local)
            .format("%I:%M %p")
            .to_string();
        assert!(app.chart.format_time(timestamp).contains(&expected));
        assert_eq!(
            app.chart.chart_options().time_scale.timezone,
            TimezoneMode::Local
        );
    }
}
