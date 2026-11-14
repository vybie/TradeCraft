//! Public Kraken spot-market reference data, quotes and candles.
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tradecraft_core::{Bar, Interval};

#[derive(Debug, PartialEq)]
pub struct BitcoinQuote {
    pub last: f64,
    pub previous_24h: f64,
    pub time: DateTime<Utc>,
}

fn parse_ticker(message: &str) -> Option<(String, BitcoinQuote)> {
    let data: Value = serde_json::from_str(message).ok()?;
    if data["channel"] != "ticker" || !matches!(data["type"].as_str(), Some("snapshot" | "update"))
    {
        return None;
    }
    let item = data["data"].as_array()?.first()?;
    let symbol = item["symbol"].as_str()?.to_owned();
    let last = item["last"].as_f64()?;
    let previous_24h = last - item["change"].as_f64()?;
    let time = DateTime::parse_from_rfc3339(item["timestamp"].as_str()?)
        .ok()?
        .with_timezone(&Utc);
    if !last.is_finite() || last <= 0.0 || !previous_24h.is_finite() || previous_24h <= 0.0 {
        return None;
    }
    Some((
        symbol,
        BitcoinQuote {
            last,
            previous_24h,
            time,
        },
    ))
}

pub fn pair(symbol: &str) -> Option<String> {
    let value = if symbol.eq_ignore_ascii_case("BTCUSD") {
        "BTC/USD"
    } else {
        symbol.strip_prefix("KRAKEN:")?
    };
    let (base, quote) = value.split_once('/')?;
    if base.is_empty()
        || quote.is_empty()
        || base.len() > 16
        || quote.len() > 16
        || !base
            .bytes()
            .chain(quote.bytes())
            .all(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(format!(
        "{}/{}",
        base.to_ascii_uppercase(),
        quote.to_ascii_uppercase()
    ))
}

static CATALOG: OnceLock<Mutex<Option<(std::time::Instant, Vec<String>)>>> = OnceLock::new();
pub async fn catalog(_client: &reqwest::Client) -> Result<Vec<String>, String> {
    let cache = CATALOG.get_or_init(|| Mutex::new(None));
    if let Ok(cached) = cache.lock()
        && let Some((when, pairs)) = &*cached
        && when.elapsed() < Duration::from_secs(3600)
    {
        return Ok(pairs.clone());
    }
    let mut pairs = tokio::time::timeout(Duration::from_secs(12), async {
        let config = WebSocketConfig::default()
            .max_message_size(Some(4 * 1024 * 1024))
            .max_frame_size(Some(4 * 1024 * 1024));
        let (mut ws, _) = tokio_tungstenite::connect_async_with_config(
            "wss://ws.kraken.com/v2",
            Some(config),
            false,
        )
        .await
        .map_err(|_| "Kraken catalog unavailable")?;
        ws.send(Message::Text(
            json!({"method":"subscribe", "params":{"channel":"instrument"}})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|_| "Kraken catalog unavailable")?;
        while let Some(message) = ws.next().await {
            if let Ok(Message::Text(text)) = message
                && let Ok(data) = serde_json::from_str::<Value>(&text)
            {
                if data["channel"] == "instrument" && data["type"] == "snapshot" {
                    let pairs = data["data"]["pairs"]
                        .as_array()
                        .ok_or("Kraken catalog unavailable")?;
                    let values: Vec<String> = pairs
                        .iter()
                        .filter(|v| v["status"] == "online")
                        .filter(|v| {
                            !matches!(
                                v["base"].as_str(),
                                Some("USD" | "EUR" | "GBP" | "CAD" | "AUD" | "CHF" | "JPY")
                            )
                        })
                        .filter_map(|v| v["symbol"].as_str().map(str::to_owned))
                        .collect();
                    return Ok::<_, &str>(values);
                }
            }
        }
        Err("Kraken catalog unavailable")
    })
    .await
    .map_err(|_| "Kraken catalog timed out")?
    .map_err(str::to_owned)?;
    pairs.sort_unstable();
    pairs.dedup();
    if let Ok(mut cached) = cache.lock() {
        *cached = Some((std::time::Instant::now(), pairs.clone()));
    }
    Ok(pairs)
}

fn minutes(interval: Interval) -> u32 {
    match interval {
        Interval::OneMinute => 1,
        Interval::FiveMinutes => 5,
        Interval::FifteenMinutes => 15,
        Interval::OneDay => 1440,
        Interval::OneWeek => 10080,
        Interval::OneMonth => 43200,
    }
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|x: &f64| x.is_finite())
}

fn checked_bar(
    time: DateTime<Utc>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
) -> Option<Bar> {
    (open > 0.0
        && close > 0.0
        && low > 0.0
        && high >= open.max(close)
        && low <= open.min(close)
        && volume >= 0.0)
        .then(|| Bar::new(time, open, high, low, close, volume))
}

pub async fn history(
    client: &reqwest::Client,
    symbol: &str,
    interval: Interval,
) -> Result<Vec<Bar>, String> {
    let pair = pair(symbol).ok_or("Invalid Kraken symbol")?;
    let response = client
        .get("https://api.kraken.com/0/public/OHLC")
        .query(&[("pair", pair), ("interval", minutes(interval).to_string())])
        .send()
        .await
        .map_err(|_| "Kraken history unavailable".to_owned())?;
    if !response.status().is_success() {
        return Err("Kraken history unavailable".into());
    }
    let data = crate::market::read_json(response).await?;
    if data["error"].as_array().is_some_and(|e| !e.is_empty()) {
        return Err("Kraken history unavailable for this pair".into());
    }
    let rows = data["result"]
        .as_object()
        .and_then(|o| o.iter().find_map(|(k, v)| (k != "last").then_some(v)))
        .and_then(Value::as_array)
        .ok_or("Kraken history unavailable")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let row = row.as_array()?;
            checked_bar(
                DateTime::from_timestamp(row.first()?.as_i64()?, 0)?,
                number(row.get(1)?)?,
                number(row.get(2)?)?,
                number(row.get(3)?)?,
                number(row.get(4)?)?,
                number(row.get(6)?)?,
            )
        })
        .collect())
}

fn candle(value: &Value) -> Option<Bar> {
    let time = DateTime::parse_from_rfc3339(value["interval_begin"].as_str()?)
        .ok()?
        .with_timezone(&Utc);
    checked_bar(
        time,
        number(&value["open"])?,
        number(&value["high"])?,
        number(&value["low"])?,
        number(&value["close"])?,
        number(&value["volume"])?,
    )
}

pub async fn stream_candles(
    symbol: &str,
    interval: Interval,
    mut active: impl FnMut() -> bool,
    mut bar: impl FnMut(Bar),
    mut status: impl FnMut(bool, &str),
) {
    let Some(pair) = pair(symbol) else {
        status(false, "Invalid Kraken symbol");
        return;
    };
    let Ok(client) = crate::reference::client() else {
        status(false, "Kraken unavailable");
        return;
    };
    while active() {
        status(false, "Kraken chart connecting");
        // Refill the recent history before each subscription; its snapshot bridges this request.
        let Ok(history) = history(&client, symbol, interval).await else {
            status(false, "Kraken chart history unavailable; retrying");
            for _ in 0..5 {
                if !active() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            continue;
        };
        for b in history {
            if !active() {
                return;
            }
            bar(b);
        }
        let config = WebSocketConfig::default()
            .max_message_size(Some(128 * 1024))
            .max_frame_size(Some(128 * 1024));
        let connect = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async_with_config(
                "wss://ws.kraken.com/v2",
                Some(config),
                false,
            ),
        )
        .await;
        if let Ok(Ok((mut ws, _))) = connect {
            let request = json!({"method":"subscribe", "params":{"channel":"ohlc", "symbol":[pair], "interval":minutes(interval)}});
            if ws
                .send(Message::Text(request.to_string().into()))
                .await
                .is_ok()
            {
                while active() {
                    match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                        Ok(Some(Ok(Message::Text(text)))) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                                if v["channel"] == "ohlc" {
                                    if let Some(rows) = v["data"].as_array() {
                                        for row in rows {
                                            if row["symbol"] == pair
                                                && let Some(b) = candle(row)
                                            {
                                                bar(b);
                                                status(true, "Kraken chart live");
                                            }
                                        }
                                    }
                                } else if v["method"] == "subscribe" && v["success"] == false {
                                    break;
                                }
                            }
                        }
                        Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        if active() {
            status(false, "Kraken chart disconnected; retrying");
            for _ in 0..5 {
                if !active() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

pub async fn stream_tickers(
    pairs: Vec<String>,
    mut active: impl FnMut() -> bool,
    mut quote: impl FnMut(String, BitcoinQuote),
    mut status: impl FnMut(&str),
) {
    while active() {
        status("Kraken crypto connecting");
        let config = WebSocketConfig::default()
            .max_message_size(Some(128 * 1024))
            .max_frame_size(Some(128 * 1024));
        let connect = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async_with_config(
                "wss://ws.kraken.com/v2",
                Some(config),
                false,
            ),
        )
        .await;
        if let Ok(Ok((mut ws, _))) = connect {
            let request =
                json!({"method":"subscribe", "params":{"channel":"ticker", "symbol":pairs}});
            if ws
                .send(Message::Text(request.to_string().into()))
                .await
                .is_ok()
            {
                while active() {
                    match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                        Ok(Some(Ok(Message::Text(text)))) => {
                            if let Some((symbol, value)) = parse_ticker(&text) {
                                status("Kraken crypto live · 24h change");
                                quote(symbol, value);
                            } else if serde_json::from_str::<Value>(&text).ok().is_some_and(|v| {
                                v["method"] == "subscribe" && v["success"] == false
                            }) {
                                break;
                            }
                        }
                        Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        if active() {
            status("Kraken crypto disconnected; retrying");
            for _ in 0..5 {
                if !active() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticker_uses_rolling_24_hour_change_and_rejects_unrelated_data() {
        let message = r#"{"channel":"ticker","type":"snapshot","data":[{"symbol":"BTC/USD","last":100.0,"change":-5.0,"timestamp":"2026-11-12T01:02:03Z"}]}"#;
        let (_, quote) = parse_ticker(message).unwrap();
        assert_eq!((quote.last, quote.previous_24h), (100.0, 105.0));
        assert_eq!(
            parse_ticker(&message.replace("BTC/USD", "ETH/USD"))
                .unwrap()
                .0,
            "ETH/USD"
        );
    }

    #[test]
    fn qualified_pairs_and_candles_are_validated() {
        assert_eq!(pair("BTCUSD").as_deref(), Some("BTC/USD"));
        assert_eq!(pair("KRAKEN:ETH/USD").as_deref(), Some("ETH/USD"));
        assert!(pair("BRK/A").is_none());
        assert!(pair("KRAKEN:ETH/USD/EXTRA").is_none());
        let row = json!({"interval_begin":"2026-11-12T01:00:00Z","open":100.0,"high":110.0,"low":90.0,"close":105.0,"volume":2.0});
        let bar = candle(&row).unwrap();
        assert_eq!(
            (bar.open, bar.high, bar.low, bar.close, bar.volume),
            (100.0, 110.0, 90.0, 105.0, 2.0)
        );
        let mut bad = row;
        bad["low"] = json!(120.0);
        assert!(candle(&bad).is_none());
    }

    #[test]
    #[ignore = "requires network access to Kraken's public spot market"]
    fn live_catalog_history_and_candles() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client = crate::reference::client().unwrap();
            let catalog = catalog(&client).await.unwrap();
            assert!(catalog.iter().any(|s| s == "BTC/USD"));
            assert!(catalog.iter().any(|s| s == "ETH/USD"));
            assert!(catalog.iter().any(|s| s == "DOGE/USD"));
            for symbol in ["BTCUSD", "KRAKEN:ETH/USD", "KRAKEN:DOGE/USD"] {
                for interval in [Interval::FiveMinutes, Interval::OneDay] {
                    assert!(
                        !history(&client, symbol, interval).await.unwrap().is_empty(),
                        "{symbol} {interval:?}"
                    );
                }
            }
            let live = std::cell::Cell::new(false);
            let count = std::cell::Cell::new(0);
            tokio::time::timeout(
                Duration::from_secs(25),
                stream_candles(
                    "KRAKEN:ETH/USD",
                    Interval::FiveMinutes,
                    || !live.get(),
                    |_| count.set(count.get() + 1),
                    |is_live, _| {
                        if is_live {
                            live.set(true);
                        }
                    },
                ),
            )
            .await
            .expect("Kraken live candles timed out");
            assert!(live.get() && count.get() > 0);
        });
    }

    #[test]
    #[ignore = "requires network access to Kraken's public market-data stream"]
    fn live_public_ticker_delivers_a_valid_quote() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let received = std::cell::Cell::new(false);
        runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(20),
                stream_tickers(
                    vec!["BTC/USD".into()],
                    || !received.get(),
                    |_, quote| {
                        assert!(quote.last > 0.0 && quote.previous_24h > 0.0);
                        received.set(true);
                    },
                    |_| {},
                ),
            )
            .await
            .expect("Kraken quote timed out");
        });
        assert!(received.get());
    }
}
