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
    tradier: Option<Arc<crate::tradier_hub::TradierHub>>,
}
#[derive(Default)]
pub(crate) struct State {
    active: Option<(SubscriptionId, Sink)>,
    pub(crate) status: String,
    pub(crate) watchlist_error: String,
    pub(crate) backfill_error: String,
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
            tradier,
        }
    }
    pub fn reconfigure(&self, settings: Settings) -> Self {
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
            tradier,
        }
    }
    pub fn status(&self) -> String {
        self.state
            .lock()
            .map(|s| {
                [
                    s.status.as_str(),
                    s.watchlist_error.as_str(),
                    s.backfill_error.as_str(),
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
            })
            .unwrap_or_else(|_| "Market data unavailable".into())
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
fn interval(t: Timeframe) -> Option<Interval> {
    match t {
        Timeframe::Min1 => Some(Interval::OneMinute),
        Timeframe::Min5 => Some(Interval::FiveMinutes),
        Timeframe::Min15 => Some(Interval::FifteenMinutes),
        Timeframe::Day1 => Some(Interval::OneDay),
        _ => None,
    }
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
                    let token = if settings.provider == Provider::Tradier {
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
                    let matches = rt.block_on(tradecraft_providers::reference::search(
                        &client,
                        settings.provider,
                        &token,
                        &q.text,
                    ))?;
                    Ok(matches
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
                        .collect())
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
        let symbols: Vec<String> = symbols
            .iter()
            .filter(|s| {
                !s.is_empty()
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
        reply.send(Ok(FeedCapabilities::new()
            .with_timeframes([
                Timeframe::Min1,
                Timeframe::Min5,
                Timeframe::Min15,
                Timeframe::Day1,
            ])
            .with_services([])));
    }
    fn history(&self, req: HistoryRequest, reply: Reply<HistoryPage>) {
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
                            if let Some(message) = price_update_status(trade.timestamp_ms) {
                                status(&shared, id, message);
                            }
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
        assert_eq!(interval(Timeframe::Hour1), None);
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
