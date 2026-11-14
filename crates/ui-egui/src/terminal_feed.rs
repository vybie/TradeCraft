use egui_charts::data::{
    Datafeed, FeedCapabilities, FeedError, FeedStatus, HistoryPage, HistoryRequest, Reply, Sink,
    Subscription, SubscriptionId,
};
use egui_charts::model::{Bar, Instrument, Quote, SymbolType, Timeframe};
use egui_charts::services::{Events, Quotes, SymbolQuery, SymbolSearch};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tradecraft_core::Interval;
use tradecraft_engine::{MarketSession, candle_for_trade};
use tradecraft_providers::{
    market::Event,
    settings::{Provider, Settings},
};
#[derive(Clone)]
pub struct Feed {
    settings: Settings,
    state: Arc<Mutex<State>>,
    pending: Arc<AtomicUsize>,
    search_revision: Arc<AtomicUsize>,
    quote_revision: Arc<AtomicUsize>,
    tradier: Option<Arc<crate::tradier_hub::TradierHub>>,
}
#[derive(Default)]
pub(crate) struct State {
    active: Option<(SubscriptionId, Sink)>,
    pub(crate) status: String,
    pub(crate) last_price_update: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) watchlist_error: String,
    pub(crate) backfill_error: String,
    pub(crate) tick_status: String,
    pub(crate) crypto_status: String,
    pub(crate) calendar_overrides: std::collections::BTreeMap<chrono::NaiveDate, egui_charts::model::SessionOverride>,
}
impl Feed {
    pub fn new(settings: Settings) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let tradier = (settings.provider == Provider::Tradier)
            .then(|| Arc::new(crate::tradier_hub::TradierHub::new(settings, state.clone())));
        Self {
            settings,
            state,
            pending: Arc::new(AtomicUsize::new(0)),
            search_revision: Arc::new(AtomicUsize::new(0)),
            quote_revision: Arc::new(AtomicUsize::new(0)),
            tradier,
        }
    }
    pub fn reconfigure(&self, settings: Settings) -> Self {
        self.quote_revision.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut state) = self.state.lock() {
            state.crypto_status.clear();
        }
        if let Some(hub) = &self.tradier {
            hub.stop();
        }
        let tradier = (settings.provider == Provider::Tradier).then(|| {
            Arc::new(crate::tradier_hub::TradierHub::new(
                settings,
                self.state.clone(),
            ))
        });
        Self {
            settings,
            state: self.state.clone(),
            pending: self.pending.clone(),
            search_revision: self.search_revision.clone(),
            quote_revision: self.quote_revision.clone(),
            tradier,
        }
    }
    pub fn status(&self) -> String {
        self.state
            .lock()
            .map(|s| {
                let price_status =
                    displayed_status(&s.status, s.last_price_update, chrono::Utc::now());
                [
                    price_status,
                    s.watchlist_error.as_str(),
                    s.backfill_error.as_str(),
                    s.tick_status.as_str(),
                    s.crypto_status.as_str(),
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
            })
            .unwrap_or_else(|_| "Market data unavailable".into())
    }
    pub(crate) fn calendar_overrides(&self) -> Vec<egui_charts::model::SessionOverride> {
        self.state.lock().map(|s| s.calendar_overrides.values().cloned().collect()).unwrap_or_default()
    }
    #[cfg(all(test, feature = "vibestrader-credential"))]
    pub(crate) fn simulate_reconnect(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) {
        if let Some(hub) = &self.tradier {
            hub.simulate_reconnect(since, cutoff);
        }
    }
}
fn displayed_status(
    status: &str,
    last_update: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> &str {
    if status.starts_with("Last price update ")
        && let Some(time) = last_update
    {
        let age = now.signed_duration_since(time);
        if age >= chrono::Duration::zero() && age < chrono::Duration::seconds(60) {
            return "Stream connected · receiving prices";
        }
    }
    status
}
fn interval(t: Timeframe) -> Option<Interval> {
    match t {
        Timeframe::Min1 => Some(Interval::OneMinute),
        Timeframe::Min5 => Some(Interval::FiveMinutes),
        Timeframe::Min15 => Some(Interval::FifteenMinutes),
        Timeframe::Day1 => Some(Interval::OneDay),
        Timeframe::Week1 => Some(Interval::OneWeek),
        Timeframe::Month1 => Some(Interval::OneMonth),
        _ => None,
    }
}
pub(crate) fn is_single_tick(timeframe: Timeframe) -> bool {
    matches!(timeframe, Timeframe::Ticks(count) if count.get() == 1)
}

pub(crate) fn tick_bars(trades: Vec<tradecraft_core::Trade>) -> Result<Vec<Bar>, FeedError> {
    let ticks: Vec<_> = trades
        .into_iter()
        .filter_map(|trade| {
            Some(egui_charts::model::Tick::new(
                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(trade.timestamp_ms)?,
                trade.price,
                trade.size,
                None,
            ))
        })
        .collect();
    egui_charts::data::aggregate_trades(&ticks, Timeframe::Ticks(std::num::NonZeroU64::MIN))
}
pub(crate) fn chart_bar(b: tradecraft_core::Bar) -> Bar {
    Bar::new(b.time, b.open, b.high, b.low, b.close, b.volume)
}
pub(crate) fn price_update_status(timestamp_ms: i64) -> Option<String> {
    let timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)?;
    Some(format!(
        "Last price update {}",
        timestamp
            .with_timezone(&chrono::Local)
            .format("%b %-d, %-I:%M:%S %p")
    ))
}
fn status(state: &Arc<Mutex<State>>, id: SubscriptionId, text: String) {
    if let Ok(mut s) = state.lock()
        && s.active.as_ref().is_some_and(|(active, _)| *active == id)
    {
        s.status = text;
    }
}
fn record_price_update(state: &Arc<Mutex<State>>, id: SubscriptionId, timestamp_ms: i64) {
    if let Some(message) = price_update_status(timestamp_ms)
        && let Some(time) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        && let Ok(mut s) = state.lock()
        && s.active.as_ref().is_some_and(|(active, _)| *active == id)
    {
        s.status = message;
        s.last_price_update = Some(time);
    }
}
fn release(state: &Arc<Mutex<State>>, id: SubscriptionId) {
    if let Ok(mut s) = state.lock()
        && s.active.as_ref().is_some_and(|(active, _)| *active == id)
    {
        s.active = None;
    }
}
impl SymbolSearch for Feed {
    fn search(&self, q: SymbolQuery, reply: Reply<Vec<Instrument>>) {
        let revision = self.search_revision.fetch_add(1, Ordering::AcqRel) + 1;
        if self
            .pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 8).then_some(n + 1)
            })
            .is_err()
        {
            reply.send(Err(FeedError::Other(
                "Too many market data requests".into(),
            )));
            return;
        }
        let pending = self.pending.clone();
        let current = self.search_revision.clone();
        let settings = self.settings;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let result = if current.load(Ordering::Acquire) != revision {
                Ok(Vec::new())
            } else {
                (|| -> Result<Vec<Instrument>, String> {
                    let crypto_only = q.symbol_type == Some(SymbolType::Crypto);
                    let token = if settings.provider == Provider::Tradier && !crypto_only {
                        tradecraft_engine::tradier_token(settings.use_vibestrader)
                            .map_err(str::to_owned)?
                    } else {
                        String::new()
                    };
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "Unable to start market data runtime".to_owned())?;
                    let client = tradecraft_providers::reference::client()?;
                    let matches = if crypto_only {
                        Vec::new()
                    } else {
                        rt.block_on(tradecraft_providers::reference::search(
                            &client,
                            settings.provider,
                            &token,
                            &q.text,
                        ))?
                    };
                    let mut instruments: Vec<_> = matches
                        .into_iter()
                        .map(|item| {
                            let mut instrument = Instrument::new(item.symbol);
                            if !item.description.is_empty() {
                                instrument = instrument.with_description(item.description);
                            }
                            if !item.exchange.is_empty() {
                                instrument = instrument.with_exchange(item.exchange);
                            }
                            let kind = match item.kind.to_ascii_lowercase().as_str() {
                                "stock" | "equity" => Some(SymbolType::Stock),
                                "index" => Some(SymbolType::Index),
                                "etf" | "mutual_fund" | "mutualfund" => Some(SymbolType::Fund),
                                "option" => Some(SymbolType::Option),
                                _ => None,
                            };
                            if let Some(kind) = kind {
                                instrument = instrument.with_symbol_type(kind);
                            }
                            instrument
                        })
                        .collect();
                    if q.symbol_type.is_none() || crypto_only {
                        match rt.block_on(tradecraft_providers::crypto::catalog(&client)) {
                            Ok(pairs) => instruments.extend(
                                rt.block_on(tradecraft_providers::crypto_names::search_catalog(
                                    &client, &pairs, &q.text,
                                ))
                                .into_iter()
                                .map(|market| {
                                    Instrument::new(format!("KRAKEN:{}", market.pair))
                                        .with_description(market.name)
                                        .with_exchange("KRAKEN")
                                        .with_source("Kraken")
                                        .with_symbol_type(SymbolType::Crypto)
                                }),
                            ),
                            Err(error) if crypto_only => return Err(error),
                            Err(_) => {}
                        }
                        if q.text.eq_ignore_ascii_case("BTCUSD") {
                            instruments.push(
                                Instrument::new("BTCUSD")
                                    .with_description("Bitcoin")
                                    .with_exchange("KRAKEN")
                                    .with_source("Kraken")
                                    .with_symbol_type(SymbolType::Crypto),
                            );
                        }
                    }
                    Ok(instruments)
                })()
            };
            pending.fetch_sub(1, Ordering::AcqRel);
            match result {
                Ok(instruments) => instruments.search(q, reply),
                Err(error) => reply.send(Err(FeedError::Network(error))),
            }
        });
    }
}

impl Quotes for Feed {
    fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
        let pairs: Vec<_> = symbols
            .iter()
            .filter_map(|s| tradecraft_providers::crypto::pair(s).map(|p| (s.clone(), p)))
            .collect();
        if !pairs.is_empty() {
            let name_pairs: Vec<String> = pairs.iter().map(|(_, pair)| pair.clone()).collect();
            if name_pairs.iter().any(|pair| {
                pair.split_once('/').is_some_and(|(base, _)| {
                    tradecraft_providers::crypto_names::cached_name(base).is_none()
                })
            }) {
                std::thread::spawn(move || {
                    let Ok(client) = tradecraft_providers::reference::client() else {
                        return;
                    };
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    rt.block_on(tradecraft_providers::crypto_names::warm_watchlist_names(
                        &client,
                        &name_pairs,
                    ));
                });
            }
            let revision = self.quote_revision.fetch_add(1, Ordering::AcqRel) + 1;
            if let Ok(mut state) = self.state.lock() {
                state.crypto_status = "Kraken crypto connecting".into();
            }
            let events = events.clone();
            let active_events = events.clone();
            let current = self.quote_revision.clone();
            let status_revision = current.clone();
            let state = self.state.clone();
            std::thread::spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let mut subscribed: Vec<String> =
                    pairs.iter().map(|(_, pair)| pair.clone()).collect();
                subscribed.sort_unstable();
                subscribed.dedup();
                rt.block_on(tradecraft_providers::crypto::stream_tickers(
                    subscribed,
                    || active_events.is_active(),
                    |pair, q| {
                        for (symbol, _) in pairs.iter().filter(|(_, p)| *p == pair) {
                            events
                                .push((symbol.clone(), Quote::new(q.last, q.previous_24h, q.time)));
                        }
                    },
                    |message| {
                        if status_revision.load(Ordering::Acquire) == revision
                            && let Ok(mut state) = state.lock()
                        {
                            state.crypto_status = message.into();
                        }
                    },
                ));
                if current.load(Ordering::Acquire) == revision
                    && let Ok(mut state) = state.lock()
                {
                    state.crypto_status.clear();
                }
            });
        }
        let symbols: Vec<String> = symbols
            .iter()
            .filter(|s| {
                tradecraft_providers::crypto::pair(s).is_none()
                    && !s.is_empty()
                    && s.len() <= 64
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '/'))
            })
            .cloned()
            .collect();
        if symbols.is_empty() {
            return;
        }
        if let Some(hub) = &self.tradier {
            hub.quotes(symbols, events);
            return;
        }
        let state = self.state.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "Unable to start quote service".to_owned())?;
                let client = tradecraft_providers::reference::client()?;
                while events.is_active() {
                    let mut last_error = None;
                    for symbol in &symbols {
                        if !events.is_active() {
                            break;
                        }
                        match rt.block_on(tradecraft_providers::reference::yahoo_quote(
                            &client, symbol,
                        )) {
                            Ok(Some(quote)) => events.push((
                                quote.symbol,
                                Quote::new(quote.last, quote.previous_close, quote.time),
                            )),
                            Ok(None) => {}
                            Err(error) => {
                                let limited = error.contains("HTTP 429");
                                last_error = Some(error);
                                if limited {
                                    break;
                                }
                            }
                        }
                    }
                    if events.is_active()
                        && let Ok(mut state) = state.lock()
                    {
                        state.watchlist_error = last_error
                            .map(|e| format!("Watchlist: {e}"))
                            .unwrap_or_default();
                    }
                    for _ in 0..60 {
                        if !events.is_active() {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                Ok(())
            })();
            if events.is_active()
                && let Err(error) = result
                && let Ok(mut state) = state.lock()
            {
                state.watchlist_error = format!("Watchlist: {error}");
            }
        });
    }
}
impl Datafeed for Feed {
    fn capabilities(&self, reply: Reply<FeedCapabilities>) {
        let mut timeframes = vec![
            Timeframe::Min1,
            Timeframe::Min5,
            Timeframe::Min15,
            Timeframe::Day1,
        ];
        if self.settings.provider == Provider::Tradier {
            timeframes.extend([Timeframe::Week1, Timeframe::Month1]);
            timeframes.push(Timeframe::Ticks(std::num::NonZeroU64::MIN));
        }
        reply.send(Ok(FeedCapabilities::new()
            .with_timeframes(timeframes)
            .with_services([])));
    }
    fn resolve(&self, symbol: &str, reply: Reply<Instrument>) {
        let mut timeframes = vec![
            Timeframe::Min1,
            Timeframe::Min5,
            Timeframe::Min15,
            Timeframe::Day1,
        ];
        if self.settings.provider == Provider::Tradier
            && tradecraft_providers::crypto::pair(symbol).is_none()
        {
            timeframes.extend([Timeframe::Week1, Timeframe::Month1]);
            timeframes.push(Timeframe::Ticks(std::num::NonZeroU64::MIN));
        }
        let instrument = Instrument::new(symbol).with_supported_timeframes(timeframes);
        reply.send(Ok(instrument));
    }
    fn history(&self, req: HistoryRequest, reply: Reply<HistoryPage>) {
        if matches!(req.timeframe, Timeframe::Week1 | Timeframe::Month1)
            && (self.settings.provider != Provider::Tradier
                || tradecraft_providers::crypto::pair(&req.symbol).is_some())
        {
            reply.send(Err(FeedError::NotSupported));
            return;
        }
        if is_single_tick(req.timeframe)
            && self.settings.provider == Provider::Tradier
            && tradecraft_providers::crypto::pair(&req.symbol).is_none()
        {
            if self
                .pending
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 8).then_some(n + 1)
                })
                .is_err()
            {
                reply.send(Err(FeedError::Other("Too many history requests".into())));
                return;
            }
            let pending = self.pending.clone();
            let settings = self.settings;
            std::thread::spawn(move || {
                let result = (|| -> Result<HistoryPage, String> {
                    let token = tradecraft_engine::tradier_token(settings.use_vibestrader)
                        .map_err(str::to_owned)?;
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "Unable to start tick history".to_owned())?;
                    let before = req
                        .before
                        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::minutes(2));
                    let Some((start, end)) =
                        rt.block_on(tradecraft_providers::market::tradier_tick_window(
                            &req.symbol,
                            &token,
                            before,
                            req.extended_hours,
                        ))?
                    else {
                        return Ok(HistoryPage::new(Vec::new(), true));
                    };
                    let ticks = rt.block_on(tradecraft_providers::market::tradier_ticks_range(
                        &req.symbol,
                        &token,
                        start,
                        end,
                        req.extended_hours,
                    ))?;
                    let mut bars = tick_bars(ticks).map_err(|e| e.to_string())?;
                    if let Some(before) = req.before {
                        bars.retain(|bar| bar.time.timestamp_millis() < before.timestamp_millis());
                    }
                    if let Some(since) = req.since {
                        bars.retain(|bar| bar.time >= since);
                    }
                    let keep = bars.len().saturating_sub(req.count);
                    if keep > 0 {
                        bars.drain(..keep);
                    }
                    Ok(HistoryPage::new(bars, false))
                })();
                pending.fetch_sub(1, Ordering::Release);
                reply.send(result.map_err(FeedError::Network));
            });
            return;
        }
        let Some(interval) = interval(req.timeframe) else {
            reply.send(Err(FeedError::NotSupported));
            return;
        };
        if self
            .pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 8).then_some(n + 1)
            })
            .is_err()
        {
            reply.send(Err(FeedError::Other("Too many history requests".into())));
            return;
        }
        let pending = self.pending.clone();
        let settings = self.settings;
        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<tradecraft_core::Bar>, String> {
                let rt = tokio::runtime::Runtime::new()
                    .map_err(|_| "Unable to start market data runtime".to_owned())?;
                if tradecraft_providers::crypto::pair(&req.symbol).is_some() {
                    let client = tradecraft_providers::reference::client()?;
                    return rt.block_on(tradecraft_providers::crypto::history(
                        &client,
                        &req.symbol,
                        interval,
                    ));
                }
                match settings.provider {
                    Provider::Tradier => {
                        let token = tradecraft_engine::tradier_token(settings.use_vibestrader)
                            .map_err(str::to_owned)?;
                        rt.block_on(tradecraft_providers::market::tradier_history(
                            &req.symbol,
                            &token,
                            req.extended_hours,
                            interval,
                        ))
                    }
                    Provider::Yahoo => {
                        let (tx, rx) = std::sync::mpsc::sync_channel(16);
                        let task = rt.spawn(tradecraft_providers::market::run_yahoo(
                            req.symbol.clone(),
                            req.extended_hours,
                            interval,
                            tx,
                        ));
                        let deadline = std::time::Instant::now() + Duration::from_secs(25);
                        let mut result = Err("History request timed out".into());
                        while std::time::Instant::now() < deadline {
                            match rx.recv_timeout(Duration::from_millis(250)) {
                                Ok(Event::History(bars)) => {
                                    result = Ok(bars);
                                    break;
                                }
                                Ok(Event::Stopped(e)) => {
                                    result = Err(e);
                                    break;
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                                _ => {}
                            }
                        }
                        task.abort();
                        result
                    }
                }
            })();
            pending.fetch_sub(1, Ordering::Release);
            match result {
                Ok(bars) => {
                    let mut bars: Vec<_> = bars
                        .into_iter()
                        .filter(|b| {
                            req.before.is_none_or(|x| b.time < x)
                                && req.since.is_none_or(|x| b.time >= x)
                        })
                        .collect();
                    let start = bars.len().saturating_sub(req.count);
                    reply.send(Ok(HistoryPage::new(
                        bars.drain(start..).map(chart_bar).collect(),
                        start == 0,
                    )))
                }
                Err(e) => reply.send(Err(FeedError::Network(e))),
            }
        });
    }
    fn subscribe(&self, sub: Subscription) {
        if tradecraft_providers::crypto::pair(&sub.symbol).is_some() {
            if matches!(sub.timeframe, Timeframe::Week1 | Timeframe::Month1) {
                sub.sink.set_status(FeedStatus::Disconnected);
                return;
            }
            let Some(interval) = interval(sub.timeframe) else {
                sub.sink.set_status(FeedStatus::Disconnected);
                return;
            };
            let sink = sub.sink;
            let id = sub.id;
            let shared = self.state.clone();
            std::thread::spawn(move || {
                if !sink.is_active() {
                    return;
                }
                if let Ok(mut state) = shared.lock() {
                    state.active = Some((id, sink.clone()));
                }
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    sink.set_status(FeedStatus::Disconnected);
                    return;
                };
                let active_sink = sink.clone();
                let bar_sink = sink.clone();
                let status_sink = sink.clone();
                rt.block_on(tradecraft_providers::crypto::stream_candles(
                    &sub.symbol,
                    interval,
                    || active_sink.is_active(),
                    |bar| bar_sink.push_bar(chart_bar(bar)),
                    |live, message| {
                        status(&shared, id, message.into());
                        status_sink.set_status(if live {
                            FeedStatus::Live
                        } else {
                            FeedStatus::Connecting
                        });
                    },
                ));
                release(&shared, id);
            });
            return;
        }
        if let Some(hub) = &self.tradier {
            hub.chart(sub);
            return;
        }
        let Some(interval) = interval(sub.timeframe) else {
            sub.sink.set_status(FeedStatus::Disconnected);
            return;
        };
        let sink = sub.sink;
        let id = sub.id;
        let shared = self.state.clone();
        std::thread::spawn(move || {
            if !sink.is_active() {
                return;
            }
            if let Ok(mut state) = shared.lock() {
                state.active = Some((id, sink.clone()));
                state.status = "Connecting to market data…".into();
            } else {
                sink.set_status(FeedStatus::Disconnected);
                return;
            }
            let mut session = MarketSession::default();
            if let Err(e) = session.start(
                Provider::Yahoo,
                sub.symbol,
                String::new(),
                sub.extended_hours,
                interval,
            ) {
                status(&shared, id, e.into());
                sink.set_status(FeedStatus::Disconnected);
                release(&shared, id);
                return;
            }
            sink.set_status(FeedStatus::Connecting);
            let mut last = None;
            while sink.is_active() {
                match session.try_recv() {
                    Ok(Event::History(bars)) => {
                        last = bars.last().cloned();
                        for b in bars {
                            sink.push_bar(chart_bar(b));
                        }
                    }
                    Ok(Event::Trade(trade)) => {
                        if let Some(bar) = candle_for_trade(last.as_ref(), interval, &trade) {
                            sink.push_bar(chart_bar(bar.clone()));
                            last = Some(bar);
                            record_price_update(&shared, id, trade.timestamp_ms);
                        }
                    }
                    Ok(Event::Status(message)) => status(&shared, id, message),
                    Ok(Event::Stopped(message)) => {
                        status(&shared, id, message);
                        sink.set_status(FeedStatus::Disconnected);
                        break;
                    }
                    Ok(Event::Snapshot { .. }) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
            session.stop();
            release(&shared, id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kraken_search_renders_name_and_preserves_pair() {
        use egui_charts::ui::dialogs::symbol_search::SymbolSearchDialog;
        let ctx = egui::Context::default();
        let source: Arc<dyn SymbolSearch> = Arc::new(vec![
            Instrument::new("KRAKEN:SOL/USD")
                .with_description("Solana")
                .with_exchange("KRAKEN")
                .with_source("Kraken")
                .with_symbol_type(SymbolType::Crypto),
        ]);
        let mut search = SymbolSearchDialog::new();
        search.set_search_service(Some(source));
        search.open_with_query("SOL".into());
        let mut found = false;
        for _ in 0..3 {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1440.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let _ = search.show(ui.ctx(), &[]);
                },
            );
            let texts: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect();
            if texts.contains(&"Solana") && texts.contains(&"SOL/USD") {
                found = true;
            }
            output.drop_without_applying_deltas();
        }
        assert!(found, "the result should show Solana and its USD market");
    }
    #[test]
    #[ignore = "requires Kraken public market metadata"]
    fn kraken_live_search_selects_solana_usd() {
        use egui_charts::ui::dialogs::symbol_search::SymbolSearchDialog;
        use egui_kittest::kittest::Queryable;
        let feed = Arc::new(Feed::new(Settings::default()));
        let mut search = SymbolSearchDialog::new();
        search.set_search_service(Some(feed));
        search.open_with_query("sol".into());
        let mut harness = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1200.0, 800.0))
            .build_ui_state(
                |ui, state: &mut (SymbolSearchDialog, Option<Instrument>)| {
                    if let Some(selected) = state.0.show(ui.ctx(), &[]) {
                        state.1 = Some(selected);
                    }
                },
                (search, None),
            );
        harness.run_steps(3);
        harness.get_by_label("Crypto").click();
        for _ in 0..150 {
            harness.step();
            if harness.query_by_label("Solana").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        harness.get_by_label("Solana").click();
        harness.run_steps(3);
        assert_eq!(
            harness.state().1.as_ref().map(|item| item.symbol.as_str()),
            Some("KRAKEN:SOL/USD")
        );
    }
    #[test]
    #[ignore = "headless visual inspection"]
    fn render_kraken_name_search() {
        use egui_charts::ui::dialogs::symbol_search::SymbolSearchDialog;
        let source: Arc<dyn SymbolSearch> = Arc::new(vec![
            Instrument::new("KRAKEN:SOL/USD")
                .with_description("Solana")
                .with_exchange("KRAKEN")
                .with_source("Kraken")
                .with_symbol_type(SymbolType::Crypto),
            Instrument::new("KRAKEN:SOLV/USD")
                .with_description("Solv Protocol")
                .with_exchange("KRAKEN")
                .with_source("Kraken")
                .with_symbol_type(SymbolType::Crypto),
        ]);
        let mut search = SymbolSearchDialog::new();
        search.set_search_service(Some(source));
        search.open_with_query("sol".into());
        let mut harness = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size(egui::vec2(1200.0, 800.0))
            .wgpu()
            .build_ui_state(
                |ui, search: &mut SymbolSearchDialog| {
                    let _ = search.show(ui.ctx(), &[]);
                },
                search,
            );
        harness.run_steps(4);
        harness
            .render()
            .unwrap()
            .save(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/kraken-search.png"),
            )
            .unwrap();
    }
    #[test]
    #[ignore = "requires Kraken public market-data network access"]
    fn kraken_chart_loads_history_and_live_candles() {
        use egui_charts::widget::SeriesId;
        let feed = Arc::new(Feed::new(Settings::default()));
        let terminal = egui_charts_terminal::Terminal::new(feed.clone())
            .with_symbol("KRAKEN:ETH/USD")
            .with_timeframe(Timeframe::Min5);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut egui_charts_terminal::Terminal| {
                    let _ = terminal.show(ui);
                },
                terminal,
            );
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        while std::time::Instant::now() < deadline {
            harness.step();
            if !harness.state().chart().bars(SeriesId::MAIN).is_empty()
                && feed.status().contains("Kraken chart live")
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "Kraken chart failed to load history and live candles: {}",
            feed.status()
        );
    }
    struct HistoryOnly(Feed);
    impl Datafeed for HistoryOnly {
        fn capabilities(&self, reply: Reply<FeedCapabilities>) {
            self.0.capabilities(reply);
        }
        fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
            self.0.history(request, reply);
        }
    }
    #[test]
    fn only_supported_timeframes_map_to_provider() {
        assert_eq!(interval(Timeframe::Min1), Some(Interval::OneMinute));
        assert_eq!(interval(Timeframe::Min5), Some(Interval::FiveMinutes));
        assert_eq!(interval(Timeframe::Min15), Some(Interval::FifteenMinutes));
        assert_eq!(interval(Timeframe::Day1), Some(Interval::OneDay));
        assert_eq!(interval(Timeframe::Week1), Some(Interval::OneWeek));
        assert_eq!(interval(Timeframe::Month1), Some(Interval::OneMonth));
        assert_eq!(interval(Timeframe::Hour1), None);
    }
    #[test]
    fn single_tick_bars_keep_each_print_even_with_matching_milliseconds() {
        let trades = [2.0, 3.0, 4.0]
            .into_iter()
            .map(|size| tradecraft_core::Trade {
                timestamp_ms: 1794516900006,
                price: 354.925,
                size,
            })
            .collect();
        let bars = tick_bars(trades).unwrap();
        assert_eq!(bars.len(), 3);
        assert_eq!(
            bars.iter().map(|b| b.volume).collect::<Vec<_>>(),
            [2.0, 3.0, 4.0]
        );
        assert!(bars.windows(2).all(|w| w[0].time < w[1].time));
        assert!(is_single_tick(Timeframe::Ticks(std::num::NonZeroU64::MIN)));
        assert!(!is_single_tick(Timeframe::Ticks(
            std::num::NonZeroU64::new(10).unwrap()
        )));
    }
    #[test]
    fn price_update_status_rejects_invalid_timestamps() {
        assert!(
            price_update_status(1_794_520_703_358)
                .is_some_and(|s| s.starts_with("Last price update "))
        );
        assert!(price_update_status(i64::MAX).is_none());
    }

    #[test]
    fn price_status_is_stable_when_active_and_shows_time_when_quiet() {
        let now = chrono::Utc::now();
        let last =
            price_update_status((now - chrono::Duration::seconds(5)).timestamp_millis()).unwrap();
        assert_eq!(
            displayed_status(&last, Some(now - chrono::Duration::seconds(5)), now),
            "Stream connected · receiving prices"
        );
        assert_eq!(
            displayed_status(&last, Some(now - chrono::Duration::seconds(60)), now),
            last
        );
        assert_eq!(
            displayed_status("Stream disconnected", Some(now), now),
            "Stream disconnected"
        );
        assert_eq!(
            displayed_status(&last, Some(now + chrono::Duration::seconds(1)), now),
            last
        );
    }
    #[test]
    #[ignore = "requires a saved Tradier token and network access"]
    fn tradier_history_uses_saved_credentials() {
        let settings = Settings::load();
        let token = tradecraft_engine::tradier_token(settings.use_vibestrader)
            .expect("saved Tradier credential");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let bars = rt
            .block_on(tradecraft_providers::market::tradier_history(
                "SPY",
                &token,
                true,
                Interval::FiveMinutes,
            ))
            .expect("Tradier history");
        assert!(!bars.is_empty(), "Tradier returned no SPY bars");
    }
    #[test]
    #[ignore = "requires a saved Tradier token and network access"]
    fn terminal_chart_loads_tradier_history_without_a_stream() {
        use egui_charts::widget::SeriesId;
        let mut settings = Settings::load();
        settings.provider = Provider::Tradier;
        let feed = Arc::new(HistoryOnly(Feed::new(settings)));
        let terminal = egui_charts_terminal::Terminal::new(feed)
            .with_symbol("SPY")
            .with_timeframe(Timeframe::Min5);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut egui_charts_terminal::Terminal| {
                    let _ = terminal.show(ui);
                },
                terminal,
            );
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            harness.step();
            if !harness.state().chart().bars(SeriesId::MAIN).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Terminal did not load Tradier history within 20 seconds");
    }
    #[test]
    #[ignore = "requires a saved Tradier token and opens one live market stream"]
    fn terminal_connects_tradier_live_stream() {
        use egui_charts::widget::SeriesId;
        let mut settings = Settings::load();
        settings.provider = Provider::Tradier;
        let feed = Arc::new(Feed::new(settings));
        let terminal = egui_charts_terminal::Terminal::new(feed.clone())
            .with_symbol("SPY")
            .with_timeframe(Timeframe::Min5);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(
                |ui, terminal: &mut egui_charts_terminal::Terminal| {
                    let _ = terminal.show(ui);
                },
                terminal,
            );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            harness.step();
            if !harness.state().chart().bars(SeriesId::MAIN).is_empty()
                && feed.status().starts_with("Stream connected")
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "Terminal did not connect a Tradier stream: {}",
            feed.status()
        );
    }
}
