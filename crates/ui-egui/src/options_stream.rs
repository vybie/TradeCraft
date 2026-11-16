//! Latest option prices from the existing market socket. Memory is bounded by subscribed symbols.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use tradecraft_core::Trade;
use tradecraft_providers::{
    options::Contract,
    shared_stream::{QuoteSide, StreamEvent},
};

#[derive(Default)]
pub(crate) struct Prices {
    bid: Option<QuoteSide>,
    ask: Option<QuoteSide>,
    trade: Option<(Trade, Option<f64>)>,
}

fn timestamp(value: &serde_json::Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .unwrap_or(0)
}

fn merge_side(current: &mut Option<QuoteSide>, incoming: &Option<QuoteSide>) {
    if let Some(side) = incoming
        && current
            .as_ref()
            .is_none_or(|old| side.timestamp_ms >= old.timestamp_ms)
    {
        *current = Some(side.clone());
    }
}

impl Prices {
    fn push(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Quote { bid, ask, .. } => {
                merge_side(&mut self.bid, bid);
                merge_side(&mut self.ask, ask);
            }
            StreamEvent::Trade { trade, volume, .. } => {
                if self
                    .trade
                    .as_ref()
                    .is_none_or(|(old, _)| trade.timestamp_ms >= old.timestamp_ms)
                {
                    self.trade = Some((trade.clone(), *volume));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn apply(&self, contract: &mut Contract) {
        if !contract.fields.is_object() {
            contract.fields = serde_json::json!({});
        }
        for (side, price, date, size, exchange) in [
            (&self.bid, "bid", "bid_date", "bidsize", "bidexch"),
            (&self.ask, "ask", "ask_date", "asksize", "askexch"),
        ] {
            if let Some(side) = side
                && side.timestamp_ms >= timestamp(&contract.fields[date])
            {
                if price == "bid" {
                    contract.bid = Some(side.price);
                } else {
                    contract.ask = Some(side.price);
                }
                contract.fields[price] = side.price.into();
                contract.fields[date] = side.timestamp_ms.into();
                if let Some(value) = side.size {
                    contract.fields[size] = value.into();
                }
                if let Some(value) = &side.exchange {
                    contract.fields[exchange] = value.clone().into();
                }
            }
        }
        if let Some((trade, volume)) = &self.trade
            && trade.timestamp_ms >= timestamp(&contract.fields["trade_date"])
        {
            contract.last = Some(trade.price);
            contract.fields["last"] = trade.price.into();
            contract.fields["last_volume"] = trade.size.into();
            contract.fields["trade_date"] = trade.timestamp_ms.into();
            if let Some(volume) = volume {
                contract.volume = Some(*volume);
                contract.fields["volume"] = (*volume).into();
            }
            if let Some(previous) = contract.fields["prevclose"]
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.0)
            {
                let change = trade.price - previous;
                contract.change = Some(change);
                contract.change_percentage = Some(change / previous * 100.0);
                contract.fields["change"] = change.into();
                contract.fields["change_percentage"] = (change / previous * 100.0).into();
            }
        }
    }

    pub(crate) fn last_trade(&self) -> Option<&Trade> {
        self.trade.as_ref().map(|(trade, _)| trade)
    }
}

pub(crate) struct State {
    pub prices: HashMap<String, Prices>,
    pub connected: bool,
    pub message: String,
    pub revision: u64,
    pub reconnects: u64,
    ctx: egui::Context,
}

pub(crate) struct Watcher {
    pub symbols: HashSet<String>,
    pub state: Weak<Mutex<State>>,
}

impl Watcher {
    pub fn new(symbols: Vec<String>, ctx: egui::Context) -> (Self, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(State {
            prices: HashMap::new(),
            connected: false,
            message: "Connecting option quotes…".into(),
            revision: 0,
            reconnects: 0,
            ctx,
        }));
        (
            Self {
                symbols: symbols.into_iter().collect(),
                state: Arc::downgrade(&state),
            },
            state,
        )
    }

    pub fn push(&self, event: &StreamEvent) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        match event {
            StreamEvent::Quote { symbol, .. } | StreamEvent::Trade { symbol, .. } => {
                if !self.symbols.contains(symbol) {
                    return;
                }
                state.prices.entry(symbol.clone()).or_default().push(event);
                if state.connected {
                    state.message = "Live option quotes · receiving market events".into();
                }
            }
            StreamEvent::Status { connected, message } => {
                if *connected && !state.connected {
                    state.reconnects += 1;
                }
                state.connected = *connected;
                state.message = if *connected {
                    "Live option quotes · waiting for market events".into()
                } else {
                    message.clone()
                };
            }
            StreamEvent::RejectedSymbol(symbol) => {
                if !self.symbols.contains(symbol) {
                    return;
                }
                state.message = format!("Tradier rejected option symbol {symbol}");
            }
        }
        state.revision = state.revision.wrapping_add(1);
        state.ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Contract {
        Contract {
            fields: serde_json::json!({"prevclose": 2.0, "bid_date": 100, "ask_date": 100, "trade_date": 100}),
            symbol: "OPTION".into(),
            kind: "call".into(),
            strike: 500.0,
            bid: Some(2.0),
            ask: Some(2.1),
            last: Some(2.0),
            change: None,
            change_percentage: None,
            volume: None,
            open_interest: Some(50.0),
            iv: Some(0.2),
            delta: Some(0.5),
            gamma: None,
            theta: None,
            vega: None,
        }
    }

    fn side(price: f64, timestamp_ms: i64) -> QuoteSide {
        QuoteSide {
            price,
            timestamp_ms,
            size: Some(3.0),
            exchange: Some("X".into()),
        }
    }

    #[test]
    fn stream_updates_bid_ask_and_last_independently_without_regressing_snapshot() {
        let mut prices = Prices::default();
        prices.push(&StreamEvent::Quote {
            symbol: "OPTION".into(),
            bid: Some(side(2.5, 200)),
            ask: Some(side(2.6, 300)),
        });
        prices.push(&StreamEvent::Quote {
            symbol: "OPTION".into(),
            bid: Some(side(2.4, 150)),
            ask: Some(side(2.7, 400)),
        });
        prices.push(&StreamEvent::Trade {
            symbol: "OPTION".into(),
            trade: Trade {
                price: 2.55,
                size: 1.0,
                timestamp_ms: 250,
            },
            volume: Some(100.0),
        });
        let mut contract = contract();
        prices.apply(&mut contract);
        assert_eq!(
            (contract.bid, contract.ask, contract.last),
            (Some(2.5), Some(2.7), Some(2.55))
        );
        assert_eq!(contract.volume, Some(100.0));
        assert_eq!(contract.fields["bidsize"], 3.0);
        assert_eq!(contract.fields["bid_date"], 200);
        assert!((contract.change_percentage.unwrap() - 27.5).abs() < 1e-9);
        assert_eq!(contract.delta, Some(0.5));
        // A fresher REST side must survive replay of older stream data.
        contract.ask = Some(2.9);
        contract.fields["ask_date"] = 500.into();
        prices.apply(&mut contract);
        assert_eq!(contract.ask, Some(2.9));
    }

    #[test]
    fn bursts_coalesce_by_symbol_and_drop_unrelated_events() {
        let (watcher, state) = Watcher::new(vec!["OPTION".into()], egui::Context::default());
        for timestamp_ms in 100..10_000 {
            watcher.push(&StreamEvent::Quote {
                symbol: "OPTION".into(),
                bid: Some(side(2.5, timestamp_ms)),
                ask: None,
            });
        }
        watcher.push(&StreamEvent::Quote {
            symbol: "OTHER".into(),
            bid: Some(side(999.0, 10_000)),
            ask: None,
        });
        let state = state.lock().unwrap();
        assert_eq!(state.prices.len(), 1);
        assert_eq!(
            state.prices["OPTION"].bid.as_ref().unwrap().timestamp_ms,
            9999
        );
    }

    #[test]
    fn subscription_expires_when_view_drops_and_reconnects_are_reported() {
        let (watcher, state) = Watcher::new(vec!["OPTION".into()], egui::Context::default());
        for connected in [true, false, true] {
            watcher.push(&StreamEvent::Status {
                connected,
                message: "Disconnected".into(),
            });
        }
        assert_eq!(state.lock().unwrap().reconnects, 2);
        drop(state);
        assert!(watcher.state.upgrade().is_none());
    }
}
