mod credentials;
mod market;
mod settings;
mod ui;
use chrono::{DateTime, Utc};
use egui_charts::{
    model::{Bar, BarData, Timeframe},
    scales::{DefaultTimeFormatter, TimezoneMode},
    widget::{Chart, SeriesId},
};
use market::{Event, Interval, Trade};
use settings::{Provider, Settings};
use std::sync::mpsc;

struct TradeCraft {
    symbol: String,
    active_symbol: String,
    token: String,
    settings: Settings,
    draft_settings: Settings,
    settings_open: bool,
    settings_message: String,
    active_token: Option<String>,
    extended_hours: bool,
    interval: Interval,
    indicators: Vec<(SeriesId, String)>,
    sma_period: usize,
    ema_period: usize,
    rsi_period: usize,
    macd_fast: usize,
    macd_slow: usize,
    macd_signal: usize,
    bands_period: usize,
    bands_deviation: f64,
    watchlist: Vec<String>,
    watchlist_entry: String,
    chart: Chart,
    status: String,
    last: Option<f64>,
    last_trade_ms: Option<i64>,
    rx: mpsc::Receiver<Event>,
    tx: mpsc::Sender<Event>,
    runtime: tokio::runtime::Runtime,
    worker: Option<tokio::task::JoinHandle<()>>,
}
impl TradeCraft {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let settings = Settings::load();
        let mut chart = Chart::new(BarData::from_bars(Vec::new()));
        chart.set_timeframe(Timeframe::Min5);
        let mut options = chart.chart_options().clone();
        options.time_scale.timezone = TimezoneMode::Local;
        chart.set_chart_options(options);
        chart.set_time_formatter(Some(std::sync::Arc::new(DefaultTimeFormatter {
            use_24_hour: false,
            show_seconds: false,
            date_format: None,
        })));
        Self {
            symbol: "SPX".into(),
            active_symbol: String::new(),
            token: String::new(),
            settings,
            draft_settings: settings,
            settings_open: false,
            settings_message: String::new(),
            active_token: None,
            extended_hours: true,
            interval: Interval::FiveMinutes,
            indicators: Vec::new(),
            sma_period: 20,
            ema_period: 50,
            rsi_period: 14,
            macd_fast: 12,
            macd_slow: 26,
            macd_signal: 9,
            bands_period: 20,
            bands_deviation: 2.0,
            watchlist: vec!["SPX".into(), "SPY".into(), "QQQ".into()],
            watchlist_entry: String::new(),
            chart,
            status: "Disconnected".into(),
            last: None,
            last_trade_ms: None,
            rx,
            tx,
            runtime: tokio::runtime::Runtime::new().expect("runtime"),
            worker: None,
        }
    }
    fn disconnect(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        (self.tx, self.rx) = mpsc::channel();
        self.active_token = None;
        self.status = "Disconnected".into();
    }
    fn connect(&mut self) {
        let symbol = self.symbol.trim().to_ascii_uppercase();
        if symbol.is_empty()
            || !symbol
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '/' | '^'))
        {
            self.status = "Enter a valid symbol".into();
            return;
        }
        let token = if self.settings.provider == Provider::Yahoo {
            String::new()
        } else if let Some(token) = &self.active_token {
            token.clone()
        } else {
            let result = selected_tradier_token(self.settings.use_vibestrader);
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
        let previous = self.worker.take();
        if let Some(worker) = &previous {
            worker.abort();
        }
        (self.tx, self.rx) = mpsc::channel();
        self.active_symbol = symbol.clone();
        self.last = None;
        self.last_trade_ms = None;
        self.chart.set_symbol(&symbol);
        self.chart.set_timeframe(self.interval.timeframe());
        self.chart.set_data(SeriesId::MAIN, Vec::new());
        self.status = "Loading history…".into();
        self.active_token = Some(token.clone());
        let tx = self.tx.clone();
        let extended_hours = self.extended_hours;
        let interval = self.interval;
        let provider = self.settings.provider;
        self.worker = Some(self.runtime.spawn(async move {
            if let Some(worker) = previous {
                let _ = worker.await;
            }
            match provider {
                Provider::Yahoo => market::run_yahoo(symbol, extended_hours, interval, tx).await,
                Provider::Tradier => market::run(symbol, token, extended_hours, interval, tx).await,
            }
        }));
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::History(bars) => self.chart.set_data(SeriesId::MAIN, bars),
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
                self.worker = None;
                self.active_token = None;
                self.status = message;
            }
        }
    }
    fn add_indicator(&mut self, index: usize) {
        use egui_charts::studies::{BollingerBands, EMA, MACD, RSI, SMA};
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
        let (id, _) = self.indicators.remove(index);
        self.chart.remove_series(id);
    }
    fn save_settings(&mut self) {
        if self.draft_settings.provider == Provider::Tradier {
            if cfg!(feature = "vibestrader-credential") && self.draft_settings.use_vibestrader {
                if let Err(error) = selected_tradier_token(true) {
                    self.settings_message = error.into();
                    return;
                }
            } else if !self.token.trim().is_empty() {
                if let Err(error) = credentials::save_tradecraft_token(self.token.trim()) {
                    self.settings_message = error.into();
                    return;
                }
            } else if let Err(error) = credentials::tradecraft_token() {
                self.settings_message = error.into();
                return;
            }
        }
        if let Err(error) = self.draft_settings.save() {
            self.settings_message = error.into();
            return;
        }
        self.token.clear();
        self.settings = self.draft_settings;
        self.settings_open = false;
        self.active_token = None;
        self.connect();
    }
    fn apply_trade(&mut self, trade: Trade) {
        let Some(trade_time) = DateTime::<Utc>::from_timestamp_millis(trade.timestamp_ms) else {
            return;
        };
        let time = if self.interval == Interval::OneDay {
            market::day_timestamp(
                trade_time
                    .with_timezone(&chrono_tz::America::New_York)
                    .date_naive(),
            )
        } else if let Some(time) = DateTime::<Utc>::from_timestamp(
            trade
                .timestamp_ms
                .div_euclid(self.interval.seconds() * 1_000)
                * self.interval.seconds(),
            0,
        ) {
            time
        } else {
            return;
        };
        let previous = self.chart.bars(SeriesId::MAIN).last().cloned();
        let bar = match previous {
            Some(mut bar) if bar.time == time => {
                bar.high = bar.high.max(trade.price);
                bar.low = bar.low.min(trade.price);
                bar.close = trade.price;
                bar.volume += trade.size;
                bar
            }
            Some(bar) if time < bar.time => return,
            _ => Bar::new(
                time,
                trade.price,
                trade.price,
                trade.price,
                trade.price,
                trade.size,
            ),
        };
        self.chart.update(SeriesId::MAIN, bar);
    }
}
fn selected_tradier_token(use_vibestrader: bool) -> Result<String, &'static str> {
    #[cfg(feature = "vibestrader-credential")]
    if use_vibestrader {
        return credentials::vibestrader_token();
    }
    let _ = use_vibestrader;
    credentials::tradecraft_token()
}
fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--check-delayed") {
        check_delayed();
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--check-live") {
        check_live();
        return Ok(());
    }
    eframe::run_native(
        "TradeCraft",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1200.0, 760.0])
                .with_min_inner_size([640.0, 480.0]),
            ..Default::default()
        },
        Box::new(|_| {
            let mut app = TradeCraft::new();
            app.connect();
            Ok(Box::new(app))
        }),
    )
}

fn check_delayed() {
    let symbol = std::env::args().nth(2).unwrap_or_else(|| "SPX".into());
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (tx, rx) = mpsc::channel();
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
    let token = match selected_tradier_token(Settings::load().use_vibestrader) {
        Ok(token) => token,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let mut app = TradeCraft::new();
    app.extended_hours = !std::env::args().any(|arg| arg == "--regular-hours");
    app.active_symbol = "SPX".into();
    app.chart.set_symbol("SPX");
    let worker = app.runtime.spawn(market::run(
        "SPX".into(),
        token,
        app.extended_hours,
        app.interval,
        app.tx.clone(),
    ));
    let end = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let mut history = false;
    let mut connected = false;
    let mut trades = 0;
    while std::time::Instant::now() < end {
        match app.rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(event) => {
                match &event {
                    Event::History(bars) => {
                        history = !bars.is_empty();
                        let extended = bars
                            .iter()
                            .filter(|bar| {
                                let local =
                                    bar.time.with_timezone(&chrono_tz::America::New_York).time();
                                local < chrono::NaiveTime::from_hms_opt(9, 30, 0).unwrap()
                                    || local >= chrono::NaiveTime::from_hms_opt(16, 0, 0).unwrap()
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
    if let Some(ms) = app.last_trade_ms {
        if let Some(time) = DateTime::<Utc>::from_timestamp_millis(ms) {
            println!("Last trade time: {}", time.to_rfc3339());
        }
    }
    if !history || !connected {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod chart_tests {
    use super::*;
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
        assert!(TradeCraft::new().extended_hours);
    }
    #[test]
    fn daily_ticks_share_the_new_york_market_date() {
        let mut app = TradeCraft::new();
        app.interval = Interval::OneDay;
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
            app.interval = interval;
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
