//! Shares one Tradier market stream between chart and quote subscriptions.
use crate::terminal_feed::State;
use chrono::{DateTime, Datelike, Timelike, Utc};
use egui_charts::data::{FeedStatus, Subscription, SubscriptionId};
use egui_charts::model::{ExtendedHoursQuote, ExtendedSession, Quote, Tick};
use egui_charts::services::Events;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tradecraft_providers::{
    market, reference,
    settings::Settings,
    shared_stream::{self, StreamEvent},
};

static STREAM_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

enum Command {
    Chart(Box<Subscription>),
    Quotes(Vec<String>, Events<(String, Quote)>),
    Stop,
    #[cfg(all(test, feature = "vibestrader-credential"))]
    SimulateReconnect {
        since: DateTime<Utc>,
        cutoff: DateTime<Utc>,
    },
}

pub struct TradierHub {
    commands: mpsc::UnboundedSender<Command>,
}

impl TradierHub {
    pub fn new(settings: Settings, state: Arc<Mutex<State>>) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let token = tradecraft_engine::tradier_token(settings.use_vibestrader)
                    .map_err(str::to_owned)?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "Unable to start shared stream".to_owned())?;
                runtime.block_on(run(token, receiver, state.clone()));
                Ok(())
            })();
            if let Err(error) = result
                && let Ok(mut state) = state.lock()
            {
                state.status = error;
            }
        });
        Self { commands }
    }
    pub fn chart(&self, sub: Subscription) {
        sub.sink.set_status(FeedStatus::Connecting);
        if let Err(error) = self.commands.send(Command::Chart(Box::new(sub)))
            && let Command::Chart(sub) = error.0
        {
            sub.sink.set_status(FeedStatus::Disconnected);
        }
    }
    pub fn quotes(&self, symbols: Vec<String>, events: Events<(String, Quote)>) {
        let _ = self.commands.send(Command::Quotes(symbols, events));
    }
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
    #[cfg(all(test, feature = "vibestrader-credential"))]
    pub fn simulate_reconnect(&self, since: DateTime<Utc>, cutoff: DateTime<Utc>) {
        let _ = self
            .commands
            .send(Command::SimulateReconnect { since, cutoff });
    }
}
impl Drop for TradierHub {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Watcher {
    symbols: HashSet<String>,
    events: Events<(String, Quote)>,
}

struct Chart {
    sub: Subscription,
    recovery: Option<Recovery>,
    recovered_before: Option<DateTime<Utc>>,
    last_received_at: DateTime<Utc>,
}
struct Recovery {
    generation: u64,
    since: DateTime<Utc>,
    cutoff: DateTime<Utc>,
    ready_at: DateTime<Utc>,
    lag: chrono::Duration,
    delay_calibrated: bool,
    delay_candidate: Option<(DateTime<Utc>, DateTime<Utc>)>,
    retention_days: i64,
    in_flight: bool,
    retry_at: Option<std::time::Instant>,
    buffered: Vec<Tick>,
    observed_minutes: HashSet<DateTime<Utc>>,
    truncated: bool,
}
#[derive(Clone, Copy)]
enum BackfillStage {
    Completed,
    Reconcile,
}
struct BackfillResult {
    id: SubscriptionId,
    generation: u64,
    stage: BackfillStage,
    result: Result<BackfillData, String>,
}
struct BackfillData {
    bars: Vec<tradecraft_core::Bar>,
    covered_minutes: HashSet<DateTime<Utc>>,
}
const MAX_BUFFERED_TICKS: usize = 100_000;

fn update_quote(quote: &mut Quote, price: f64, time: DateTime<Utc>, regular: Option<bool>) -> bool {
    if time < quote.time {
        return false;
    }
    match regular {
        Some(true) => {
            quote.last = price;
            quote.time = time;
            true
        }
        Some(false) => {
            if quote.extended.is_some_and(|previous| time < previous.time) {
                return false;
            }
            let eastern = time.with_timezone(&chrono_tz::America::New_York);
            let session = if eastern.hour() < 12 {
                ExtendedSession::PreMarket
            } else {
                ExtendedSession::PostMarket
            };
            quote.extended = Some(ExtendedHoursQuote {
                session,
                last: price,
                time,
            });
            true
        }
        None => false,
    }
}

async fn run(
    token: String,
    mut commands: mpsc::UnboundedReceiver<Command>,
    state: Arc<Mutex<State>>,
) {
    let Ok(client) = reference::client() else {
        return;
    };
    let (symbols_tx, symbols_rx) = watch::channel(Vec::<String>::new());
    let (stream_tx, mut stream_rx) = mpsc::channel(8192);
    let stream_token = token.clone();
    #[cfg(all(test, feature = "vibestrader-credential"))]
    let simulated_stream = stream_tx.clone();
    let stream_task = tokio::spawn(async move {
        let _guard = STREAM_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        shared_stream::run(stream_token, symbols_rx, stream_tx).await;
    });
    let (snapshot_tx, mut snapshot_rx) = mpsc::unbounded_channel();
    let (calendar_tx, mut calendar_rx) = mpsc::unbounded_channel();
    let (backfill_tx, mut backfill_rx) = mpsc::unbounded_channel();
    let mut calendars = HashMap::new();
    let mut calendar_pending = HashSet::new();
    let mut calendar_retry = HashMap::new();
    let mut charts: Vec<Chart> = Vec::new();
    let mut watchers: Vec<Watcher> = Vec::new();
    let mut quotes: HashMap<String, Quote> = HashMap::new();
    let mut rejected_symbols = HashSet::new();
    let mut current = Vec::new();
    let mut connected = false;
    let mut ever_connected = false;
    let mut outage_since: Option<DateTime<Utc>> = None;
    #[cfg(all(test, feature = "vibestrader-credential"))]
    let mut simulated_cutoff: Option<DateTime<Utc>> = None;
    let mut recovery_generation = 0u64;
    let mut snapshot_retry: Option<std::time::Instant> = None;
    let mut snapshot_day = Utc::now()
        .with_timezone(&chrono_tz::America::New_York)
        .date_naive();
    let mut refresh = tokio::time::interval(Duration::from_millis(250));
    let eastern = Utc::now().with_timezone(&chrono_tz::America::New_York);
    let key = (eastern.year(), eastern.month());
    calendar_pending.insert(key);
    let calendar_client = client.clone();
    let calendar_token = token.clone();
    let initial_calendar = calendar_tx.clone();
    tokio::spawn(async move {
        let _ = initial_calendar.send((
            key,
            reference::tradier_calendar(&calendar_client, &calendar_token, key.0, key.1).await,
        ));
    });
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Chart(sub)) => { sub.sink.set_status(if connected { FeedStatus::Live } else { FeedStatus::Connecting }); charts.push(Chart { sub: *sub, recovery: None, recovered_before: None, last_received_at: Utc::now() }); },
                Some(Command::Quotes(symbols, events)) => {
                    let symbols: HashSet<_> = symbols.into_iter().collect();
                    for symbol in &symbols { if let Some(quote) = quotes.get(symbol) { events.push((symbol.clone(), quote.clone())); } }
                    watchers.push(Watcher { symbols, events });
                },
                #[cfg(all(test, feature = "vibestrader-credential"))]
                Some(Command::SimulateReconnect { since, cutoff }) => {
                    outage_since = Some(since);
                    simulated_cutoff = Some(cutoff);
                    let _ = simulated_stream.send(StreamEvent::Status { connected: true, message: "Simulated reconnect".into() }).await;
                },
                Some(Command::Stop) | None => break,
            },
            event = stream_rx.recv() => match event {
                Some(StreamEvent::Status { connected: is_connected, message }) => {
                    let now = Utc::now();
                    if !is_connected && ever_connected {
                        let oldest = charts.iter().map(|chart| chart.recovery.as_ref().map_or(chart.last_received_at, |recovery| recovery.since.min(chart.last_received_at))).min().unwrap_or(now);
                        outage_since = Some(outage_since.map_or(oldest, |previous| previous.min(oldest)));
                        for chart in &mut charts { chart.recovery = None; }
                    }
                    connected = is_connected;
                    if connected && let Some(since) = outage_since.take() {
                        recovery_generation = recovery_generation.wrapping_add(1);
                        for chart in &mut charts {
                            let Some(interval) = crate::tradier_backfill::interval(chart.sub.timeframe) else { continue; };
                            let retention = match (interval, chart.sub.extended_hours) {
                                (tradecraft_core::Interval::OneMinute, true) => 10,
                                (tradecraft_core::Interval::OneMinute, false) => 20,
                                (tradecraft_core::Interval::OneDay, _) => 730,
                                (_, true) => 18,
                                (_, false) => 40,
                            };
                            let oldest = now - chrono::Duration::days(retention);
                            let start = since - chrono::Duration::seconds(interval.seconds());
                            let truncated = start < oldest;
                            let start = start.max(oldest);
                            chart.recovered_before = None;
                            #[cfg(all(test, feature = "vibestrader-credential"))]
                            let cutoff = simulated_cutoff.unwrap_or_else(|| crate::tradier_backfill::next_minute(now));
                            #[cfg(not(all(test, feature = "vibestrader-credential")))]
                            let cutoff = crate::tradier_backfill::next_minute(now);
                            chart.sub.sink.set_status(FeedStatus::Connecting);
                            request_backfill(&chart.sub, recovery_generation, BackfillStage::Completed, start, now, &token, backfill_tx.clone());
                            chart.recovery = Some(Recovery { generation: recovery_generation, since: start, cutoff, ready_at: cutoff + chrono::Duration::seconds(3), lag: chrono::Duration::zero(), delay_calibrated: false, delay_candidate: None, retention_days: retention, in_flight: false, retry_at: None, buffered: Vec::new(), observed_minutes: HashSet::new(), truncated });
                        }
                        #[cfg(all(test, feature = "vibestrader-credential"))]
                        { simulated_cutoff = None; }
                    } else {
                        for chart in &charts { chart.sub.sink.set_status(if connected { FeedStatus::Live } else { FeedStatus::Disconnected }); }
                    }
                    if let Ok(mut state) = state.lock() {
                        state.status = if connected && charts.iter().any(|chart| chart.recovery.is_some()) { "Stream connected · restoring chart history…".into() } else { message };
                    }
                    if connected && ever_connected && !current.is_empty() { request_snapshot(&token, current.clone(), snapshot_tx.clone()); }
                    ever_connected |= connected;
                },
                Some(StreamEvent::Trade { symbol, trade }) => {
                    let Some(time) = chrono::DateTime::<Utc>::from_timestamp_millis(trade.timestamp_ms) else { continue; };
                    let eastern = time.with_timezone(&chrono_tz::America::New_York);
                    let key = (eastern.year(), eastern.month());
                    if !calendars.contains_key(&key) && !calendar_pending.contains(&key)
                        && calendar_retry.get(&key).is_none_or(|retry: &std::time::Instant| std::time::Instant::now() >= *retry)
                    {
                        calendar_pending.insert(key);
                        let client = client.clone(); let token = token.clone(); let tx = calendar_tx.clone();
                        tokio::spawn(async move { let _ = tx.send((key, reference::tradier_calendar(&client, &token, key.0, key.1).await)); });
                    }
                    let regular = calendars.get(&key).map(|calendar| tradecraft_providers::market::inside_regular_session(calendar, eastern.date_naive(), eastern.time()));
                    for chart in &mut charts {
                        if chart.sub.symbol == symbol && chart.sub.sink.is_active() { chart.last_received_at = Utc::now(); }
                        if chart.sub.symbol == symbol && chart.sub.sink.is_active() && chart.recovered_before.is_none_or(|cutoff| time >= cutoff) && (chart.sub.extended_hours || regular == Some(true)) {
                            let tick = Tick::new(time, trade.price, trade.size, None);
                            if let Some(recovery) = &mut chart.recovery && !recovery.delay_calibrated {
                                let now = Utc::now();
                                let delayed = crate::tradier_backfill::delayed_cutoff(now, time);
                                let confirmed = recovery.delay_candidate.and_then(|previous| crate::tradier_backfill::confirmed_delay(previous, now, time));
                                if delayed.is_none() { recovery.delay_calibrated = true; }
                                else if confirmed.is_none() { recovery.delay_candidate = Some((time, now)); }
                                if let Some((cutoff, lag)) = confirmed {
                                    recovery.delay_calibrated = true;
                                    recovery_generation = recovery_generation.wrapping_add(1);
                                    recovery.generation = recovery_generation;
                                    recovery.cutoff = cutoff;
                                    recovery.ready_at = cutoff + lag + chrono::Duration::seconds(3);
                                    recovery.lag = lag;
                                    let oldest = now - chrono::Duration::days(recovery.retention_days);
                                    let requested = recovery.since - lag;
                                    recovery.truncated |= requested < oldest;
                                    recovery.since = requested.max(oldest);
                                    recovery.in_flight = false;
                                    recovery.retry_at = None;
                                    recovery.buffered.clear();
                                    recovery.observed_minutes.clear();
                                    request_backfill(&chart.sub, recovery.generation, BackfillStage::Completed, recovery.since, time, &token, backfill_tx.clone());
                                }
                            }
                            if let Some(recovery) = &mut chart.recovery && time >= recovery.cutoff {
                                if recovery.buffered.len() >= MAX_BUFFERED_TICKS {
                                    recovery_generation = recovery_generation.wrapping_add(1);
                                    recovery.generation = recovery_generation;
                                    recovery.cutoff = crate::tradier_backfill::next_minute(time);
                                    recovery.ready_at = recovery.cutoff + recovery.lag + chrono::Duration::seconds(3);
                                    recovery.in_flight = false;
                                    recovery.retry_at = None;
                                    recovery.buffered.clear();
                                    recovery.observed_minutes.clear();
                                    if let Ok(mut state) = state.lock() { state.status = "History recovery restarting after high activity".into(); }
                                } else { recovery.buffered.push(tick); }
                            } else {
                                if let Some(recovery) = &mut chart.recovery
                                    && let Some(interval) = crate::tradier_backfill::interval(chart.sub.timeframe)
                                    && time >= crate::tradier_backfill::partial_start(&chart.sub.clock, interval, recovery.cutoff) {
                                        let minute = time.timestamp().div_euclid(60) * 60;
                                        if let Some(minute) = DateTime::<Utc>::from_timestamp(minute, 0) { recovery.observed_minutes.insert(minute); }
                                }
                                chart.sub.sink.push_tick(tick);
                            }
                        }
                    }
                    if let Some(quote) = quotes.get_mut(&symbol) && update_quote(quote, trade.price, time, regular) {
                        for watcher in &watchers { if watcher.events.is_active() && watcher.symbols.contains(&symbol) { watcher.events.push((symbol.clone(), quote.clone())); } }
                    }
                    if !charts.iter().any(|chart| chart.recovery.is_some()) && let Some(message) = super::terminal_feed::price_update_status(trade.timestamp_ms) && let Ok(mut state) = state.lock() { state.status = message; }
                },
                Some(StreamEvent::RejectedSymbol(symbol)) => {
                    if current.contains(&symbol) {
                        rejected_symbols.insert(symbol.clone());
                        for chart in &charts { if chart.sub.symbol == symbol { chart.sub.sink.set_status(FeedStatus::Disconnected); } }
                        if let Ok(mut state) = state.lock() { state.watchlist_error = format!("Tradier rejected symbol {symbol}"); }
                    }
                },
                None => break,
            },
            result = snapshot_rx.recv() => if let Some(result) = result {
                match result {
                    Ok(snapshot) => {
                        if let Ok(mut state) = state.lock() && state.watchlist_error.starts_with("Watchlist: ") { state.watchlist_error.clear(); }
                        for item in snapshot {
                            if !current.contains(&item.symbol) { continue; }
                            let quote = quotes.entry(item.symbol.clone()).or_insert_with(|| Quote::new(item.last, item.previous_close, item.time));
                            quote.prev_close = item.previous_close;
                            if item.time > quote.time { quote.last = item.last; quote.time = item.time; }
                            for watcher in &watchers { if watcher.events.is_active() && watcher.symbols.contains(&item.symbol) { watcher.events.push((item.symbol.clone(), quote.clone())); } }
                        }
                    },
                    Err(error) => {
                        snapshot_retry = Some(std::time::Instant::now() + Duration::from_secs(60));
                        if let Ok(mut state) = state.lock() { state.watchlist_error = format!("Watchlist: {error}"); }
                    },
                }
            },
            result = backfill_rx.recv() => if let Some(result) = result {
                if let Some(chart) = charts.iter_mut().find(|chart| chart.sub.id == result.id)
                    && let Some(recovery) = &mut chart.recovery
                    && recovery.generation == result.generation && chart.sub.sink.is_active()
                {
                    match (result.stage, result.result) {
                        (BackfillStage::Completed, Ok(data)) => {
                            for bar in data.bars { chart.sub.sink.push_bar(super::terminal_feed::chart_bar(bar)); }
                        },
                        (BackfillStage::Completed, Err(_)) => {
                            if let Ok(mut state) = state.lock() { state.status = "Stream connected · history recovery will retry".into(); }
                        },
                        (BackfillStage::Reconcile, Ok(data)) if !recovery.observed_minutes.is_subset(&data.covered_minutes) => {
                            recovery.in_flight = false;
                            recovery.retry_at = Some(std::time::Instant::now() + Duration::from_secs(10));
                            if let Ok(mut state) = state.lock() { state.status = "Waiting for Tradier history to catch up; retrying".into(); }
                        },
                        (BackfillStage::Reconcile, Ok(data)) => {
                            for bar in data.bars { chart.sub.sink.push_bar(super::terminal_feed::chart_bar(bar)); }
                            recovery.buffered.sort_by_key(|tick| tick.ts);
                            for tick in recovery.buffered.drain(..) { chart.sub.sink.push_tick(tick); }
                            let truncated = recovery.truncated;
                            chart.recovered_before = data.covered_minutes.iter().max().map(|minute| *minute + chrono::Duration::minutes(1));
                            chart.recovery = None;
                            chart.sub.sink.set_status(if connected { FeedStatus::Live } else { FeedStatus::Disconnected });
                            if let Ok(mut state) = state.lock() && truncated { state.backfill_error = "Some history is beyond Tradier's available range".into(); }
                        },
                        (BackfillStage::Reconcile, Err(error)) => {
                            recovery.in_flight = false;
                            recovery.retry_at = Some(std::time::Instant::now() + Duration::from_secs(10));
                            if let Ok(mut state) = state.lock() { state.status = format!("History recovery failed; retrying: {error}"); }
                        },
                    }
                }
                if connected && !charts.iter().any(|chart| chart.recovery.is_some()) && let Ok(mut state) = state.lock()
                    && (state.status.contains("restoring chart history") || state.status.starts_with("History recovery") || state.status.starts_with("Waiting for Tradier history")) {
                        state.status = "Stream connected · chart history restored".into();
                    }
            },
            result = calendar_rx.recv() => if let Some((key, result)) = result {
                calendar_pending.remove(&key);
                match result {
                    Ok(calendar) => { calendars.insert(key, calendar); if let Ok(mut state) = state.lock() && state.watchlist_error.starts_with("Market calendar unavailable") { state.watchlist_error.clear(); } },
                    Err(_) => { calendar_retry.insert(key, std::time::Instant::now() + Duration::from_secs(60)); if let Ok(mut state) = state.lock() { state.watchlist_error = "Market calendar unavailable; regular-hours updates paused".into(); } },
                }
            },
            _ = refresh.tick() => {
                charts.retain(|chart| chart.sub.sink.is_active());
                watchers.retain(|watcher| watcher.events.is_active());
                let mut symbols = BTreeSet::new();
                for chart in &charts { symbols.insert(chart.sub.symbol.clone()); }
                for watcher in &watchers { symbols.extend(watcher.symbols.iter().cloned()); }
                rejected_symbols.retain(|symbol| symbols.contains(symbol));
                if rejected_symbols.is_empty() && let Ok(mut state) = state.lock()
                    && state.watchlist_error.starts_with("Tradier rejected symbol ") { state.watchlist_error.clear(); }
                symbols.retain(|symbol| !rejected_symbols.contains(symbol));
                let next: Vec<_> = symbols.into_iter().collect();
                if next != current {
                    let new_symbols: Vec<_> = next.iter().filter(|symbol| !current.contains(symbol)).cloned().collect();
                    current = next.clone();
                    quotes.retain(|symbol, _| current.contains(symbol));
                    let _ = symbols_tx.send(next);
                    if !new_symbols.is_empty() { request_snapshot(&token, new_symbols, snapshot_tx.clone()); }
                }
                let today = Utc::now().with_timezone(&chrono_tz::America::New_York).date_naive();
                if today != snapshot_day {
                    snapshot_day = today;
                    if !current.is_empty() { request_snapshot(&token, current.clone(), snapshot_tx.clone()); }
                }
                if snapshot_retry.is_some_and(|retry| std::time::Instant::now() >= retry) {
                    snapshot_retry = None;
                    if !current.is_empty() { request_snapshot(&token, current.clone(), snapshot_tx.clone()); }
                }
                if connected {
                    let now = Utc::now();
                    for chart in &mut charts {
                        if let Some(recovery) = &mut chart.recovery
                            && !recovery.in_flight
                            && now >= recovery.ready_at
                            && recovery.retry_at.is_none_or(|retry| std::time::Instant::now() >= retry)
                        {
                            recovery.in_flight = true;
                            recovery.retry_at = None;
                            request_backfill(&chart.sub, recovery.generation, BackfillStage::Reconcile, recovery.since, recovery.cutoff, &token, backfill_tx.clone());
                        }
                    }
                }
            }
        }
    }
    stream_task.abort();
}

fn request_snapshot(
    token: &str,
    symbols: Vec<String>,
    tx: mpsc::UnboundedSender<Result<Vec<reference::MarketQuote>, String>>,
) {
    let token = token.to_owned();
    tokio::spawn(async move {
        let client = match reference::client() {
            Ok(client) => client,
            Err(error) => {
                let _ = tx.send(Err(error));
                return;
            }
        };
        for chunk in symbols.chunks(100) {
            let result = reference::tradier_quotes(&client, &token, chunk).await;
            let failed = result.is_err();
            if tx.send(result).is_err() || failed {
                break;
            }
        }
    });
}

fn request_backfill(
    sub: &Subscription,
    generation: u64,
    stage: BackfillStage,
    since: DateTime<Utc>,
    cutoff: DateTime<Utc>,
    token: &str,
    tx: mpsc::UnboundedSender<BackfillResult>,
) {
    let id = sub.id;
    let symbol = sub.symbol.clone();
    let clock = sub.clock.clone();
    let timeframe = sub.timeframe;
    let extended = sub.extended_hours;
    let token = token.to_owned();
    tokio::spawn(async move {
        let result = async {
            let interval =
                crate::tradier_backfill::interval(timeframe).ok_or("Unsupported chart interval")?;
            match stage {
                BackfillStage::Completed => {
                    let bars = market::tradier_history_range(
                        &symbol,
                        &token,
                        extended,
                        interval,
                        since,
                        Some(cutoff),
                    )
                    .await?;
                    Ok(BackfillData {
                        bars: crate::tradier_backfill::completed(bars, &clock, interval, cutoff),
                        covered_minutes: HashSet::new(),
                    })
                }
                BackfillStage::Reconcile => {
                    let minute_start =
                        crate::tradier_backfill::partial_start(&clock, interval, cutoff);
                    let (history, minutes) = tokio::join!(
                        market::tradier_history_range(
                            &symbol,
                            &token,
                            extended,
                            interval,
                            since,
                            Some(cutoff)
                        ),
                        market::tradier_history_range(
                            &symbol,
                            &token,
                            extended,
                            tradecraft_core::Interval::OneMinute,
                            minute_start,
                            Some(cutoff)
                        ),
                    );
                    let history = history?;
                    let minutes = minutes?;
                    let covered_minutes = minutes
                        .iter()
                        .filter(|bar| bar.time + chrono::Duration::minutes(1) <= cutoff)
                        .map(|bar| bar.time)
                        .collect();
                    let bars = crate::tradier_backfill::reconcile(
                        history, minutes, &clock, interval, cutoff,
                    );
                    Ok(BackfillData {
                        bars,
                        covered_minutes,
                    })
                }
            }
        }
        .await;
        let _ = tx.send(BackfillResult {
            id,
            generation,
            stage,
            result,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_prices_preserve_previous_close_and_ignore_stale_or_unknown_sessions() {
        let time = DateTime::<Utc>::from_timestamp(1_751_376_600, 0).unwrap();
        let mut quote = Quote::new(100.0, 95.0, time);
        assert!(update_quote(
            &mut quote,
            102.0,
            time + chrono::Duration::seconds(1),
            Some(true)
        ));
        assert_eq!(quote.last, 102.0);
        assert_eq!(quote.prev_close, 95.0);
        assert!(update_quote(
            &mut quote,
            102.5,
            time + chrono::Duration::seconds(1),
            Some(true)
        ));
        assert!(!update_quote(&mut quote, 99.0, time, Some(true)));
        assert!(!update_quote(
            &mut quote,
            99.0,
            time + chrono::Duration::seconds(2),
            None
        ));
        assert!(update_quote(
            &mut quote,
            103.0,
            time + chrono::Duration::seconds(3),
            Some(false)
        ));
        assert_eq!(quote.last, 102.5);
        assert_eq!(quote.extended.unwrap().last, 103.0);
    }
}
