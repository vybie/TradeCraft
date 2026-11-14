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

use chrono::{DateTime, Utc};
use std::sync::mpsc;
use tradecraft_core::{Bar, Interval, Trade, market_day_timestamp};
use tradecraft_providers::{
    market::{self, Event},
    settings::{Provider, Settings},
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Indicator {
    Sma {
        period: usize,
    },
    Ema {
        period: usize,
    },
    Rsi {
        period: usize,
    },
    Macd {
        fast: usize,
        slow: usize,
        signal: usize,
    },
    Bands {
        period: usize,
        deviation: f64,
    },
}

impl Indicator {
    fn valid(&self) -> bool {
        let valid_period = |n: usize| (1..=1000).contains(&n);
        match *self {
            Self::Sma { period } | Self::Ema { period } | Self::Rsi { period } => {
                valid_period(period)
            }
            Self::Macd { fast, slow, signal } => {
                valid_period(fast) && valid_period(slow) && valid_period(signal)
            }
            Self::Bands { period, deviation } => {
                valid_period(period) && deviation.is_finite() && (0.1..=10.0).contains(&deviation)
            }
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Workspace {
    pub symbol: String,
    pub interval: Interval,
    pub extended_hours: bool,
    pub settings: Settings,
    pub watchlist: Vec<String>,
    pub indicators: Vec<Indicator>,
    pub chart_style: ChartStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartStyle {
    Candles,
    Bars,
    Line,
}
impl Workspace {
    pub fn new(interval: Interval, settings: Settings) -> Self {
        Self {
            symbol: "SPX".into(),
            interval,
            extended_hours: true,
            settings,
            watchlist: vec!["SPX".into(), "SPY".into(), "QQQ".into()],
            indicators: Vec::new(),
            chart_style: ChartStyle::Candles,
        }
    }
}

/// State commands are shared by the desktop frontend and script runner.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum WorkspaceCommand {
    SelectSymbol {
        symbol: String,
    },
    SetInterval {
        interval: Interval,
    },
    SetExtendedHours {
        enabled: bool,
    },
    SetChartStyle {
        style: ChartStyle,
    },
    SetProvider {
        provider: Provider,
        #[serde(default)]
        use_vibestrader: bool,
    },
    AddWatchlist {
        symbol: String,
    },
    RemoveWatchlist {
        symbol: String,
    },
    AddIndicator {
        indicator: Indicator,
    },
    RemoveIndicator {
        index: usize,
    },
}

pub const WORKSPACE_COMMANDS: &[&str] = &[
    "select_symbol",
    "set_interval",
    "set_extended_hours",
    "set_chart_style",
    "set_provider",
    "add_watchlist",
    "remove_watchlist",
    "add_indicator",
    "remove_indicator",
];

pub fn tradier_token(use_vibestrader: bool) -> Result<String, &'static str> {
    #[cfg(feature = "vibestrader-credential")]
    if use_vibestrader {
        return tradecraft_providers::credentials::vibestrader_token();
    }
    if use_vibestrader {
        return Err("VibesTrader credential feature is disabled");
    }
    tradecraft_providers::credentials::tradecraft_token()
}

fn validated_symbol(symbol: &str) -> Result<String, &'static str> {
    if symbol.len() > 64 {
        return Err("Enter a valid symbol");
    }
    let symbol = symbol.trim().to_ascii_uppercase();
    if symbol.is_empty()
        || symbol.len() > 64
        || !symbol
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '/' | '^'))
    {
        return Err("Enter a valid symbol");
    }
    Ok(symbol)
}

impl Workspace {
    pub fn dispatch(&mut self, command: WorkspaceCommand) -> Result<(), &'static str> {
        match command {
            WorkspaceCommand::SelectSymbol { symbol } => self.symbol = validated_symbol(&symbol)?,
            WorkspaceCommand::SetInterval { interval } => self.interval = interval,
            WorkspaceCommand::SetExtendedHours { enabled } => self.extended_hours = enabled,
            WorkspaceCommand::SetChartStyle { style } => self.chart_style = style,
            WorkspaceCommand::SetProvider {
                provider,
                use_vibestrader,
            } => {
                if use_vibestrader && !cfg!(feature = "vibestrader-credential") {
                    return Err("VibesTrader credential feature is disabled");
                }
                self.settings = Settings {
                    provider,
                    use_vibestrader,
                };
            }
            WorkspaceCommand::AddWatchlist { symbol } => {
                let symbol = validated_symbol(&symbol)?;
                if !self.watchlist.contains(&symbol) {
                    if self.watchlist.len() >= 1000 {
                        return Err("Watchlist limit reached");
                    }
                    self.watchlist.push(symbol);
                }
            }
            WorkspaceCommand::RemoveWatchlist { symbol } => {
                self.watchlist.retain(|item| item != &symbol)
            }
            WorkspaceCommand::AddIndicator { indicator } => {
                if !indicator.valid() {
                    return Err("Invalid indicator settings");
                }
                if self.indicators.len() >= 1000 {
                    return Err("Indicator limit reached");
                }
                self.indicators.push(indicator);
            }
            WorkspaceCommand::RemoveIndicator { index } => {
                if index >= self.indicators.len() {
                    return Err("Indicator not found");
                }
                self.indicators.remove(index);
            }
        }
        Ok(())
    }
    /// Applies an entire bounded JSON command array atomically, without network or credential access.
    pub fn run_script(&mut self, json: &str) -> Result<(), String> {
        if json.len() > 1_000_000 {
            return Err("Script is too large".into());
        }
        let commands: Vec<WorkspaceCommand> =
            serde_json::from_str(json).map_err(|e| e.to_string())?;
        if commands.len() > 1000 {
            return Err("Too many commands".into());
        }
        let mut next = self.clone();
        for command in commands {
            next.dispatch(command).map_err(str::to_owned)?;
        }
        *self = next;
        Ok(())
    }
}

/// Owns one background market stream and its event channel.
pub struct MarketSession {
    runtime: Option<tokio::runtime::Runtime>,
    worker: Option<tokio::task::JoinHandle<()>>,
    tx: mpsc::SyncSender<Event>,
    rx: mpsc::Receiver<Event>,
}
impl Default for MarketSession {
    fn default() -> Self {
        let (tx, rx) = mpsc::sync_channel(8192);
        Self {
            runtime: tokio::runtime::Runtime::new().ok(),
            worker: None,
            tx,
            rx,
        }
    }
}
impl MarketSession {
    pub fn start(
        &mut self,
        provider: Provider,
        symbol: String,
        token: String,
        extended_hours: bool,
        interval: Interval,
    ) -> Result<(), &'static str> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("Unable to start the market-data runtime")?;
        let previous = self.worker.take();
        if let Some(worker) = &previous {
            worker.abort();
        }
        (self.tx, self.rx) = mpsc::sync_channel(8192);
        let tx = self.tx.clone();
        self.worker = Some(runtime.spawn(async move {
            if let Some(worker) = previous {
                let _ = worker.await;
            }
            match provider {
                Provider::Yahoo => market::run_yahoo(symbol, extended_hours, interval, tx).await,
                Provider::Tradier => market::run(symbol, token, extended_hours, interval, tx).await,
            }
        }));
        Ok(())
    }
    pub fn try_recv(&mut self) -> Result<Event, mpsc::TryRecvError> {
        self.rx.try_recv()
    }
    pub fn is_active(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }
    pub fn stop(&mut self) {
        // Dropping the receiver releases any producer blocked by a full queue.
        (self.tx, self.rx) = mpsc::sync_channel(8192);
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}
impl Drop for MarketSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Commands are independent of the frontend and do not perform network I/O.
pub enum Command {
    Reset { interval: Interval },
    ReplaceHistory(Vec<Bar>),
    ApplyTrade(Trade),
}

pub enum ChartChange {
    Reset,
    History(Vec<Bar>),
    Candle(Bar),
    Unchanged,
}

pub struct Engine {
    interval: Interval,
    bars: Vec<Bar>,
    workspace: Workspace,
}

impl Engine {
    pub fn new(interval: Interval) -> Self {
        Self {
            interval,
            bars: Vec::new(),
            workspace: Workspace::new(interval, Settings::default()),
        }
    }
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }
    pub fn set_interval(&mut self, interval: Interval) {
        self.interval = interval;
        self.workspace.interval = interval;
    }
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    pub fn dispatch(&mut self, command: WorkspaceCommand) -> Result<(), &'static str> {
        self.workspace.dispatch(command)?;
        self.interval = self.workspace.interval;
        Ok(())
    }
    /// Validate and persist the selected provider. Tokens are stored in the OS credential store.
    pub fn save_settings(&mut self, settings: Settings, token: &str) -> Result<(), &'static str> {
        let mut next = self.workspace.clone();
        next.dispatch(WorkspaceCommand::SetProvider {
            provider: settings.provider,
            use_vibestrader: settings.use_vibestrader,
        })?;
        if settings.provider == Provider::Tradier {
            if settings.use_vibestrader {
                tradier_token(true)?;
            } else if !token.trim().is_empty() {
                tradecraft_providers::credentials::save_tradecraft_token(token.trim())?;
            } else {
                tradier_token(false)?;
            }
        }
        settings.save()?;
        self.workspace = next;
        Ok(())
    }
    pub fn execute(&mut self, command: Command) -> ChartChange {
        match command {
            Command::Reset { interval } => {
                self.interval = interval;
                self.workspace.interval = interval;
                self.bars.clear();
                ChartChange::Reset
            }
            Command::ReplaceHistory(bars) => {
                self.bars = bars;
                ChartChange::History(self.bars.clone())
            }
            Command::ApplyTrade(trade) => {
                let Some(bar) = candle_for_trade(self.bars.last(), self.interval, &trade) else {
                    return ChartChange::Unchanged;
                };
                if let Some(last) = self.bars.last_mut().filter(|last| last.time == bar.time) {
                    *last = bar.clone();
                } else {
                    self.bars.push(bar.clone());
                }
                ChartChange::Candle(bar)
            }
        }
    }
}

/// Apply a market update to a candle without depending on a chart widget.
pub fn candle_for_trade(previous: Option<&Bar>, interval: Interval, trade: &Trade) -> Option<Bar> {
    let trade_time = DateTime::<Utc>::from_timestamp_millis(trade.timestamp_ms)?;
    let time = if matches!(
        interval,
        Interval::OneDay | Interval::OneWeek | Interval::OneMonth
    ) {
        market_day_timestamp(tradecraft_core::market_period_date(
            trade_time
                .with_timezone(&chrono_tz::America::New_York)
                .date_naive(),
            interval,
        ))
    } else {
        let seconds = trade
            .timestamp_ms
            .div_euclid(interval.seconds() * 1_000)
            .checked_mul(interval.seconds())?;
        DateTime::<Utc>::from_timestamp(seconds, 0)?
    };
    match previous {
        Some(bar) if bar.time == time => {
            let mut bar = bar.clone();
            bar.high = bar.high.max(trade.price);
            bar.low = bar.low.min(trade.price);
            bar.close = trade.price;
            bar.volume += trade.size;
            Some(bar)
        }
        Some(bar) if time < bar.time => None,
        _ => Some(Bar::new(
            time,
            trade.price,
            trade.price,
            trade.price,
            trade.price,
            trade.size,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_and_monthly_trades_follow_new_york_calendar_periods() {
        let trade = |s: &str| Trade {
            timestamp_ms: DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis(),
            price: 100.0,
            size: 1.0,
        };
        let monday = trade("2026-11-09T15:00:00Z");
        let friday = trade("2026-11-13T15:00:00Z");
        let next_week = trade("2026-11-16T15:00:00Z");
        let a = candle_for_trade(None, Interval::OneWeek, &monday).unwrap();
        let b = candle_for_trade(Some(&a), Interval::OneWeek, &friday).unwrap();
        let c = candle_for_trade(Some(&b), Interval::OneWeek, &next_week).unwrap();
        assert_eq!(a.time, b.time);
        assert_ne!(b.time, c.time);
        let month = candle_for_trade(None, Interval::OneMonth, &monday).unwrap();
        let same_month = candle_for_trade(Some(&month), Interval::OneMonth, &next_week).unwrap();
        let december = candle_for_trade(
            Some(&same_month),
            Interval::OneMonth,
            &trade("2026-12-01T15:00:00Z"),
        )
        .unwrap();
        assert_eq!(month.time, same_month.time);
        assert_ne!(same_month.time, december.time);
    }

    #[test]
    fn commands_update_candles_without_a_ui() {
        let mut engine = Engine::new(Interval::FiveMinutes);
        let first = Trade {
            timestamp_ms: 1_751_376_600_000,
            price: 100.0,
            size: 2.0,
        };
        assert!(matches!(
            engine.execute(Command::ApplyTrade(first)),
            ChartChange::Candle(_)
        ));
        let later = Trade {
            timestamp_ms: 1_751_376_840_000,
            price: 101.0,
            size: 3.0,
        };
        engine.execute(Command::ApplyTrade(later));
        assert_eq!(engine.bars().len(), 1);
        assert_eq!(engine.bars()[0].close, 101.0);
        assert_eq!(engine.bars()[0].volume, 5.0);
        engine.execute(Command::Reset {
            interval: Interval::OneMinute,
        });
        assert!(engine.bars().is_empty());
    }

    #[test]
    fn stale_trade_does_not_change_candles() {
        let mut engine = Engine::new(Interval::OneMinute);
        engine.execute(Command::ApplyTrade(Trade {
            timestamp_ms: 1_751_376_600_000,
            price: 100.0,
            size: 1.0,
        }));
        assert!(matches!(
            engine.execute(Command::ApplyTrade(Trade {
                timestamp_ms: 1_751_376_540_000,
                price: 1.0,
                size: 1.0
            })),
            ChartChange::Unchanged
        ));
        assert_eq!(engine.bars()[0].close, 100.0);
    }

    #[test]
    fn script_uses_same_validated_workspace_commands_atomically() {
        let mut workspace = Workspace::new(Interval::FiveMinutes, Settings::default());
        workspace
            .run_script(
                r#"[
            {"command":"select_symbol","symbol":"aapl"},
            {"command":"set_interval","interval":"one_day"},
            {"command":"add_watchlist","symbol":"aapl"},
            {"command":"add_indicator","indicator":{"kind":"ema","period":21}}
        ]"#,
            )
            .unwrap();
        assert_eq!(workspace.symbol, "AAPL");
        assert_eq!(workspace.interval, Interval::OneDay);
        assert!(workspace.watchlist.contains(&"AAPL".to_owned()));
        assert_eq!(workspace.indicators, vec![Indicator::Ema { period: 21 }]);
        assert!(workspace.run_script(r#"[{"command":"select_symbol","symbol":"SPY"},{"command":"remove_indicator","index":99}]"#).is_err());
        assert_eq!(workspace.symbol, "AAPL");
        assert!(
            workspace
                .run_script(
                    r#"[{"command":"add_indicator","indicator":{"kind":"sma","period":0}}]"#
                )
                .is_err()
        );
    }

    #[test]
    fn stopping_session_releases_a_blocked_full_queue() {
        let mut session = MarketSession::default();
        let sender = session.tx.clone();
        for _ in 0..8192 {
            sender.send(Event::Status(String::new())).unwrap();
        }
        let worker = std::thread::spawn(move || sender.send(Event::Status(String::new())));
        session.stop();
        assert!(worker.join().unwrap().is_err());
    }

    #[cfg(not(feature = "vibestrader-credential"))]
    #[test]
    fn community_script_cannot_select_personal_credential() {
        let mut workspace = Workspace::new(Interval::FiveMinutes, Settings::default());
        assert!(
            workspace
                .run_script(
                    r#"[{"command":"set_provider","provider":"tradier","use_vibestrader":true}]"#
                )
                .is_err()
        );
        assert_eq!(workspace.settings.provider, Provider::Yahoo);
    }
}
