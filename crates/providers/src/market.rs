use chrono::{Datelike, Duration, NaiveTime, TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::mpsc::SyncSender;
use tokio_tungstenite::tungstenite::Message;
use tradecraft_core::{Bar, Interval, Trade, market_day_timestamp as day_timestamp};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

fn append_bounded(buffer: &mut Vec<u8>, chunk: &[u8]) -> Result<(), &'static str> {
    if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(buffer.len()) {
        return Err("Market data response exceeds size limit");
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

pub(crate) async fn read_json(mut response: reqwest::Response) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err("Market data response exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Unable to read market data")?
    {
        append_bounded(&mut bytes, &chunk)?;
    }
    serde_json::from_slice(&bytes).map_err(|_| "Market data response is invalid".into())
}

pub enum Event {
    History(Vec<Bar>),
    Snapshot { price: f64, timestamp_ms: i64 },
    Trade(Trade),
    Status(String),
    Stopped(String),
}

pub async fn run_yahoo(
    symbol: String,
    extended_hours: bool,
    interval: Interval,
    tx: SyncSender<Event>,
) {
    if let Err(error) = yahoo_loop(&symbol, extended_hours, interval, &tx).await {
        let _ = tx.send(Event::Stopped(error));
    }
}

async fn yahoo_loop(
    symbol: &str,
    extended_hours: bool,
    interval: Interval,
    tx: &SyncSender<Event>,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("Mozilla/5.0 TradeCraft/0.1")
        .build()
        .map_err(|_| "Unable to initialize network client".to_owned())?;
    let yahoo_symbol = if symbol == "SPX" { "^GSPC" } else { symbol };
    let mut url = reqwest::Url::parse("https://query1.finance.yahoo.com/v8/finance/chart/")
        .map_err(|_| "Invalid data source URL")?;
    url.path_segments_mut()
        .map_err(|_| "Invalid data source URL")?
        .pop_if_empty()
        .push(yahoo_symbol);
    loop {
        let response = client
            .get(url.clone())
            .query(&[
                (
                    "range",
                    if interval == Interval::OneDay {
                        "2y"
                    } else {
                        "5d"
                    },
                ),
                (
                    "interval",
                    if interval == Interval::OneDay {
                        "1d"
                    } else {
                        interval.label()
                    },
                ),
                (
                    "includePrePost",
                    if extended_hours { "true" } else { "false" },
                ),
            ])
            .send()
            .await
            .map_err(|_| "Yahoo unavailable; try again later".to_owned())?;
        if !response.status().is_success() {
            return Err(format!("Yahoo unavailable (HTTP {})", response.status()));
        }
        let value = read_json(response).await?;
        let cutoff = Utc::now().timestamp() - 15 * 60;
        let mut bars = parse_yahoo_bars(&value, cutoff, interval);
        let mut omitted = if interval == Interval::OneDay {
            0
        } else {
            omit_suspect_yahoo_bars(&mut bars, interval)
        };
        let mut daily_snapshot_time = None;
        if interval == Interval::OneDay {
            let intraday_response = client
                .get(url.clone())
                .query(&[
                    ("range", "5d"),
                    ("interval", "5m"),
                    (
                        "includePrePost",
                        if extended_hours { "true" } else { "false" },
                    ),
                ])
                .send()
                .await
                .map_err(|_| "Yahoo unavailable; try again later")?
                .error_for_status()
                .map_err(|_| "Yahoo intraday data unavailable")?;
            let intraday = read_json(intraday_response).await?;
            let today = Utc::now()
                .with_timezone(&chrono_tz::America::New_York)
                .date_naive();
            let mut current = parse_yahoo_bars(&intraday, cutoff, Interval::FiveMinutes);
            current.retain(|bar| {
                bar.time
                    .with_timezone(&chrono_tz::America::New_York)
                    .date_naive()
                    == today
            });
            omitted += omit_suspect_yahoo_bars(&mut current, Interval::FiveMinutes);
            daily_snapshot_time = current
                .last()
                .filter(|bar| {
                    bar.time
                        .with_timezone(&chrono_tz::America::New_York)
                        .date_naive()
                        == today
                })
                .map(|bar| (bar.time.timestamp() + 300) * 1000);
            let mut daily: Option<Bar> = None;
            for bar in current.into_iter().filter(|bar| {
                bar.time
                    .with_timezone(&chrono_tz::America::New_York)
                    .date_naive()
                    == today
            }) {
                if let Some(day) = daily.as_mut() {
                    day.high = day.high.max(bar.high);
                    day.low = day.low.min(bar.low);
                    day.close = bar.close;
                    day.volume += bar.volume;
                } else {
                    daily = Some(Bar::new(
                        day_timestamp(today),
                        bar.open,
                        bar.high,
                        bar.low,
                        bar.close,
                        bar.volume,
                    ));
                }
            }
            if let Some(day) = daily {
                bars.push(day);
            }
        }
        if bars.is_empty() {
            return Err("No delayed chart data available for this symbol".into());
        }
        if let Some(last) = bars.last() {
            let _ = tx.send(Event::Snapshot {
                price: last.close,
                timestamp_ms: if interval == Interval::OneDay {
                    daily_snapshot_time.unwrap_or(last.time.timestamp_millis())
                } else {
                    (last.time.timestamp() + interval.seconds()) * 1000
                },
            });
        }
        let status = if omitted > 0 {
            format!("Yahoo · delayed 15+ min · {omitted} suspect candles omitted")
        } else {
            "Yahoo · delayed at least 15 min".into()
        };
        let _ = tx.send(Event::Status(status));
        tx.send(Event::History(bars))
            .map_err(|_| "Chart closed".to_owned())?;
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

// Yahoo sometimes reports zero-volume extended-hours wicks far outside the
// surrounding prices. Omit the whole bar, rather than invent replacement OHLC.
fn omit_suspect_yahoo_bars(bars: &mut Vec<Bar>, interval: Interval) -> usize {
    let mut keep = vec![true; bars.len()];
    for i in 1..bars.len().saturating_sub(1) {
        let (prev, bar, next) = (&bars[i - 1], &bars[i], &bars[i + 1]);
        let eastern = bar.time.with_timezone(&chrono_tz::America::New_York).time();
        let regular = NaiveTime::from_hms_opt(9, 30, 0).unwrap_or(NaiveTime::MIN) <= eastern
            && eastern < NaiveTime::from_hms_opt(16, 0, 0).unwrap_or(NaiveTime::MIN);
        if regular
            || bar.volume != 0.0
            || prev.close <= 0.0
            || next.close <= 0.0
            || (bar.time - prev.time).num_seconds() > interval.seconds() * 3
            || (next.time - bar.time).num_seconds() > interval.seconds() * 3
        {
            continue;
        }
        let reference = (prev.close + next.close) / 2.0;
        let nearby_agree = (prev.close - next.close).abs() / reference < 0.02;
        let endpoint_near =
            (bar.open / reference - 1.0).abs() < 0.02 || (bar.close / reference - 1.0).abs() < 0.02;
        if nearby_agree
            && endpoint_near
            && (bar.low < reference * 0.95 || bar.high > reference * 1.05)
        {
            keep[i] = false;
        }
    }
    let before = bars.len();
    let mut index = 0;
    bars.retain(|_| {
        let result = keep[index];
        index += 1;
        result
    });
    before - bars.len()
}

fn parse_yahoo_bars(value: &Value, cutoff: i64, interval: Interval) -> Vec<Bar> {
    let result = &value["chart"]["result"][0];
    let Some(timestamps) = result["timestamp"].as_array() else {
        return Vec::new();
    };
    let quote = &result["indicators"]["quote"][0];
    let mut bars = Vec::new();
    for (i, timestamp) in timestamps.iter().enumerate() {
        let Some(ts) = timestamp.as_i64() else {
            continue;
        };
        if interval != Interval::OneDay && ts.saturating_add(interval.seconds()) > cutoff {
            continue;
        }
        let Some(mut time) = Utc.timestamp_opt(ts, 0).single() else {
            continue;
        };
        if interval == Interval::OneDay {
            let date = time
                .with_timezone(&chrono_tz::America::New_York)
                .date_naive();
            let Some(cutoff_time) = Utc.timestamp_opt(cutoff, 0).single() else {
                continue;
            };
            if date
                >= cutoff_time
                    .with_timezone(&chrono_tz::America::New_York)
                    .date_naive()
            {
                continue;
            }
            time = day_timestamp(date);
        }
        let (Some(open), Some(high), Some(low), Some(close)) = (
            number(&quote["open"][i]),
            number(&quote["high"][i]),
            number(&quote["low"][i]),
            number(&quote["close"][i]),
        ) else {
            continue;
        };
        bars.push(Bar::new(
            time,
            open,
            high,
            low,
            close,
            number(&quote["volume"][i]).unwrap_or(0.0),
        ));
    }
    bars.sort_by_key(|bar| bar.time);
    bars.dedup_by_key(|bar| bar.time);
    bars
}

fn tradier_interval(interval: Interval) -> &'static str {
    match interval {
        Interval::OneMinute => "1min",
        Interval::FiveMinutes => "5min",
        Interval::FifteenMinutes => "15min",
        Interval::OneDay => "daily",
        Interval::OneWeek => "weekly",
        Interval::OneMonth => "monthly",
    }
}

pub async fn tradier_history(
    symbol: &str,
    token: &str,
    extended_hours: bool,
    interval: Interval,
) -> Result<Vec<Bar>, String> {
    let days = match interval {
        Interval::OneDay => 730,
        Interval::OneWeek | Interval::OneMonth => 365 * 30,
        _ => 7,
    };
    tradier_history_range(
        symbol,
        token,
        extended_hours,
        interval,
        Utc::now() - Duration::days(days),
        None,
    )
    .await
}

/// Read bounded historical ticks. Availability varies by symbol and session; the end is exclusive.
pub async fn tradier_ticks_range(
    symbol: &str,
    token: &str,
    since: chrono::DateTime<Utc>,
    until: chrono::DateTime<Utc>,
    extended_hours: bool,
) -> Result<Vec<Trade>, String> {
    if since >= until || until - since > Duration::minutes(10) {
        return Err("Invalid tick history range".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "Unable to initialize market data client".to_owned())?;
    let mut ticks = Vec::new();
    let mut cursor =
        chrono::DateTime::<Utc>::from_timestamp(since.timestamp().div_euclid(60) * 60, 0)
            .ok_or("Invalid tick history range")?;
    while cursor < until {
        let next = cursor + Duration::minutes(1);
        let start = cursor
            .with_timezone(&chrono_tz::America::New_York)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let end = next
            .with_timezone(&chrono_tz::America::New_York)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let response = client
            .get("https://api.tradier.com/v1/markets/timesales")
            .bearer_auth(token)
            .header("Accept", "application/json")
            .query(&[
                ("symbol", symbol),
                ("interval", "tick"),
                ("start", &start),
                ("end", &end),
                ("session_filter", if extended_hours { "all" } else { "open" }),
            ])
            .send()
            .await
            .map_err(|_| "Tradier tick history unavailable".to_owned())?;
        if !response.status().is_success() {
            return Err(format!(
                "Tradier tick history unavailable (HTTP {})",
                response.status()
            ));
        }
        let data = read_json(response).await?;
        if data.get("series").is_none() {
            return Err("Tradier did not return tick history".into());
        }
        let rows: Vec<&Value> = match &data["series"]["data"] {
            Value::Array(rows) => rows.iter().collect(),
            Value::Object(_) => vec![&data["series"]["data"]],
            Value::Null => Vec::new(),
            _ => return Err("Invalid Tradier tick history".into()),
        };
        if ticks.len().saturating_add(rows.len()) > 100_000 {
            return Err("Tradier tick history exceeds the safe limit".into());
        }
        for row in rows {
            if let Some(tick) = parse_history_tick(row)
                && tick.timestamp_ms >= since.timestamp_millis()
                && tick.timestamp_ms < until.timestamp_millis()
                && tick.timestamp_ms >= cursor.timestamp_millis()
                && tick.timestamp_ms < next.timestamp_millis()
            {
                ticks.push(tick);
            }
        }
        cursor = next;
    }
    ticks.sort_by_key(|tick| tick.timestamp_ms);
    Ok(ticks)
}

/// Select a recent tick-history window, using observed minute bars for extended sessions.
pub async fn tradier_tick_window(
    symbol: &str,
    token: &str,
    before: chrono::DateTime<Utc>,
    extended_hours: bool,
) -> Result<Option<(chrono::DateTime<Utc>, chrono::DateTime<Utc>)>, String> {
    if before < Utc::now() - Duration::days(5) {
        return Ok(None);
    }
    if extended_hours {
        let oldest = Utc::now() - Duration::days(5);
        let bars = tradier_history_range(
            symbol, token, true, Interval::OneMinute, oldest, Some(before),
        )
        .await?;
        return Ok(extended_tick_window(&bars, before, oldest));
    }
    let client = crate::reference::client()?;
    let eastern = before.with_timezone(&chrono_tz::America::New_York);
    let mut cache = HashMap::new();
    for offset in 0..=7 {
        let Some(date) = eastern
            .date_naive()
            .checked_sub_signed(Duration::days(offset))
        else {
            break;
        };
        let key = (date.year(), date.month());
        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(key) {
            entry.insert(crate::reference::tradier_calendar(&client, token, key.0, key.1).await?);
        }
        let Some(calendar) = cache.get(&key) else {
            continue;
        };
        let entries = match &calendar["calendar"]["days"]["day"] {
            Value::Array(days) => days.iter().collect::<Vec<_>>(),
            Value::Object(_) => vec![&calendar["calendar"]["days"]["day"]],
            _ => Vec::new(),
        };
        let date_text = date.format("%Y-%m-%d").to_string();
        let Some(day) = entries
            .into_iter()
            .find(|d| d["date"].as_str() == Some(&date_text))
        else {
            continue;
        };
        let Some(start) = day["open"]["start"]
            .as_str()
            .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
        else {
            continue;
        };
        let Some(end) = day["open"]["end"]
            .as_str()
            .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
        else {
            continue;
        };
        let Some(open) = chrono_tz::America::New_York
            .from_local_datetime(&date.and_time(start))
            .single()
            .map(|t| t.to_utc())
        else {
            continue;
        };
        let Some(close) = chrono_tz::America::New_York
            .from_local_datetime(&date.and_time(end))
            .single()
            .map(|t| t.to_utc())
        else {
            continue;
        };
        let end = before.min(close);
        if end <= open {
            continue;
        }
        let start = open.max(end - Duration::minutes(5));
        return Ok(Some((start, end)));
    }
    Ok(None)
}

fn extended_tick_window(
    bars: &[Bar],
    before: chrono::DateTime<Utc>,
    oldest: chrono::DateTime<Utc>,
) -> Option<(chrono::DateTime<Utc>, chrono::DateTime<Utc>)> {
    let last = bars.iter().filter(|bar| bar.time < before).max_by_key(|bar| bar.time)?;
    let end = before.min(last.time + Duration::minutes(1));
    Some(((end - Duration::minutes(5)).max(oldest), end))
}

fn parse_history_tick(value: &Value) -> Option<Trade> {
    let timestamp_ms = value["timestamp"].as_i64()?;
    let price = number(&value["price"])?;
    let size = number(&value["volume"])?;
    if price <= 0.0
        || size < 0.0
        || chrono::DateTime::<Utc>::from_timestamp_millis(timestamp_ms).is_none()
    {
        return None;
    }
    Some(Trade {
        timestamp_ms,
        price,
        size,
    })
}

/// Fetches historical bars from a specific UTC instant; request times are expressed in New York time.
pub async fn tradier_history_range(
    symbol: &str,
    token: &str,
    extended_hours: bool,
    interval: Interval,
    since: chrono::DateTime<Utc>,
    until: Option<chrono::DateTime<Utc>>,
) -> Result<Vec<Bar>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "Unable to initialize network client".to_owned())?;
    let local = since.with_timezone(&chrono_tz::America::New_York);
    let start = local.format("%Y-%m-%d %H:%M").to_string();
    let daily_start = local.format("%Y-%m-%d").to_string();
    let calendar = matches!(
        interval,
        Interval::OneDay | Interval::OneWeek | Interval::OneMonth
    );
    let mut request = client
        .get(if calendar {
            "https://api.tradier.com/v1/markets/history"
        } else {
            "https://api.tradier.com/v1/markets/timesales"
        })
        .bearer_auth(token)
        .header("Accept", "application/json");
    request = if calendar {
        request.query(&[
            ("symbol", symbol),
            ("interval", tradier_interval(interval)),
            ("start", &daily_start),
        ])
    } else {
        request.query(&[
            ("symbol", symbol),
            ("interval", tradier_interval(interval)),
            ("start", &start),
            (
                "session_filter",
                if extended_hours { "all" } else { "open" },
            ),
        ])
    };
    if let Some(until) = until {
        let local = until.with_timezone(&chrono_tz::America::New_York);
        let end = if calendar {
            local.format("%Y-%m-%d").to_string()
        } else {
            local.format("%Y-%m-%d %H:%M").to_string()
        };
        request = request.query(&[("end", end)]);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "History request failed; check the connection".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "History request failed (HTTP {})",
            response.status()
        ));
    }
    let json = read_json(response).await?;
    if json
        .get(if calendar { "history" } else { "series" })
        .is_none()
    {
        return Err("Tradier did not return chart history".into());
    }
    let data = if calendar {
        &json["history"]["day"]
    } else {
        &json["series"]["data"]
    };
    let rows: Vec<&Value> = match data {
        Value::Array(rows) => rows.iter().collect(),
        Value::Object(_) => vec![data],
        Value::Null => Vec::new(),
        _ => return Err("Unexpected history response".into()),
    };
    let mut bars: Vec<Bar> = rows
        .into_iter()
        .filter_map(|row| {
            if calendar {
                parse_daily_bar(row)
            } else {
                parse_bar(row)
            }
        })
        .collect();
    bars.sort_by_key(|bar| bar.time);
    bars.dedup_by_key(|bar| bar.time);
    Ok(bars)
}

pub async fn run(
    symbol: String,
    token: String,
    extended_hours: bool,
    interval: Interval,
    tx: SyncSender<Event>,
) {
    if let Err(error) = run_inner(&symbol, &token, extended_hours, interval, &tx).await {
        let _ = tx.send(Event::Stopped(error));
    }
}

async fn run_inner(
    symbol: &str,
    token: &str,
    extended_hours: bool,
    interval: Interval,
    tx: &SyncSender<Event>,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "Unable to initialize network client".to_owned())?;
    let bars = tradier_history(symbol, token, extended_hours, interval).await?;
    tx.send(Event::History(bars))
        .map_err(|_| "Chart closed".to_owned())?;
    let _ = tx.send(Event::Status("Connecting to live prices…".into()));
    let response = client
        .post("https://api.tradier.com/v1/markets/events/session")
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Content-Length", "0")
        .body("")
        .send()
        .await
        .map_err(|_| "Unable to create streaming session".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "Streaming session failed (HTTP {})",
            response.status()
        ));
    }
    let json = read_json(response).await?;
    let session = json["stream"]["sessionid"]
        .as_str()
        .ok_or("Missing streaming session")?;
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    let (mut socket, _) = tokio_tungstenite::connect_async_with_config(
        "wss://ws.tradier.com/v1/markets/events",
        Some(config),
        false,
    )
    .await
    .map_err(|_| "Unable to connect to Tradier stream".to_owned())?;
    socket
        .send(Message::Text(
            json!({"symbols":[symbol],"filter":["trade"],"sessionid":session,"linebreak":true})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|_| "Unable to subscribe to Tradier stream".to_owned())?;
    let _ = tx.send(Event::Status(
        "Stream connected · waiting for price updates".into(),
    ));
    let mut calendars = HashMap::new();
    while let Some(message) = socket.next().await {
        let message =
            message.map_err(|_| "Stream disconnected; reconnect to refresh history".to_owned())?;
        if let Message::Text(text) = message {
            for line in text.lines() {
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if value.get("error").is_some() {
                    return Err("Tradier rejected the stream subscription".into());
                }
                if value["type"] == "trade"
                    && value["symbol"].as_str() == Some(symbol)
                    && let Some(trade) = parse_trade(&value)
                {
                    if !extended_hours {
                        let Some(time) =
                            chrono::DateTime::<Utc>::from_timestamp_millis(trade.timestamp_ms)
                        else {
                            continue;
                        };
                        let eastern = time.with_timezone(&chrono_tz::America::New_York);
                        let key = (eastern.year(), eastern.month());
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            calendars.entry(key)
                        {
                            let response = client
                                .get("https://api.tradier.com/v1/markets/calendar")
                                .bearer_auth(token)
                                .header("Accept", "application/json")
                                .query(&[("year", key.0.to_string()), ("month", key.1.to_string())])
                                .send()
                                .await
                                .map_err(|_| {
                                    "Market calendar unavailable; cannot filter regular hours"
                                        .to_owned()
                                })?;
                            if !response.status().is_success() {
                                return Err(
                                    "Market calendar unavailable; cannot filter regular hours"
                                        .into(),
                                );
                            }
                            let calendar = read_json(response).await?;
                            if calendar["calendar"]["days"]["day"].is_null() {
                                return Err(
                                    "Market calendar missing; cannot filter regular hours".into()
                                );
                            }
                            entry.insert(calendar);
                        }
                        if !calendars.get(&key).is_some_and(|calendar| {
                            inside_regular_session(calendar, eastern.date_naive(), eastern.time())
                        }) {
                            continue;
                        }
                    }
                    tx.send(Event::Trade(trade))
                        .map_err(|_| "Chart closed".to_owned())?;
                }
            }
        } else if matches!(message, Message::Close(_)) {
            break;
        }
    }
    Err("Stream disconnected; reconnect to refresh history".into())
}

pub fn inside_regular_session(calendar: &Value, date: chrono::NaiveDate, time: NaiveTime) -> bool {
    let day = &calendar["calendar"]["days"]["day"];
    let rows: Vec<&Value> = match day {
        Value::Array(rows) => rows.iter().collect(),
        Value::Object(_) => vec![day],
        _ => return false,
    };
    let date_string = date.format("%Y-%m-%d").to_string();
    let Some(day) = rows
        .into_iter()
        .find(|row| row["date"].as_str() == Some(&date_string))
    else {
        return false;
    };
    let start = day["open"]["start"]
        .as_str()
        .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok());
    let end = day["open"]["end"]
        .as_str()
        .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok());
    matches!((start, end), (Some(start), Some(end)) if start <= time && time < end)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n: &f64| n.is_finite())
}
fn parse_bar(value: &Value) -> Option<Bar> {
    let time = Utc
        .timestamp_opt(value["timestamp"].as_i64()?, 0)
        .single()?;
    Some(Bar::new(
        time,
        number(&value["open"])?,
        number(&value["high"])?,
        number(&value["low"])?,
        number(&value["close"])?,
        number(&value["volume"]).unwrap_or(0.0),
    ))
}
fn parse_daily_bar(value: &Value) -> Option<Bar> {
    let date = chrono::NaiveDate::parse_from_str(value["date"].as_str()?, "%Y-%m-%d").ok()?;
    Some(Bar::new(
        day_timestamp(date),
        number(&value["open"])?,
        number(&value["high"])?,
        number(&value["low"])?,
        number(&value["close"])?,
        number(&value["volume"]).unwrap_or(0.0),
    ))
}
pub(crate) fn parse_trade(value: &Value) -> Option<Trade> {
    let timestamp_ms = value["date"]
        .as_i64()
        .or_else(|| value["date"].as_str()?.parse().ok())?;
    let price = number(&value["price"])?;
    let size = number(&value["size"])?;
    if price <= 0.0 || size < 0.0 {
        return None;
    }
    Some(Trade {
        timestamp_ms,
        price,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tradier_historical_tick_uses_millisecond_timestamp_price_and_size() {
        let row = json!({"time":"2026-11-12T20:55:00","timestamp":1794516900006i64,"price":354.925,"volume":40});
        let tick = parse_history_tick(&row).unwrap();
        assert_eq!(
            (tick.timestamp_ms, tick.price, tick.size),
            (1794516900006, 354.925, 40.0)
        );
        assert!(parse_history_tick(&json!({"price":1,"volume":1})).is_none());
        assert!(
            parse_history_tick(&json!({"timestamp":1794516900006i64,"price":-1,"volume":1}))
                .is_none()
        );
    }

    #[test]
    fn extended_tick_window_uses_latest_observed_minute_without_session_assumptions() {
        let before = Utc.with_ymd_and_hms(2026, 11, 12, 8, 4, 0).unwrap();
        let oldest = before - Duration::days(5);
        let midnight = Utc.with_ymd_and_hms(2026, 11, 12, 8, 0, 0).unwrap();
        let bars = vec![Bar::new(midnight, 1.0, 1.0, 1.0, 1.0, 1.0)];
        assert_eq!(
            extended_tick_window(&bars, before, oldest),
            Some((midnight - Duration::minutes(4), midnight + Duration::minutes(1)))
        );
        assert_eq!(extended_tick_window(&[], before, oldest), None);
    }
    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "requires saved Tradier credential and market-data network access"]
    fn tradier_tick_window_and_history_live() {
        let token = crate::credentials::vibestrader_token().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (start, end) = tradier_tick_window("AAPL", &token, Utc::now(), false)
                .await
                .unwrap()
                .expect("recent session");
            let ticks = tradier_ticks_range("AAPL", &token, start, end, false)
                .await
                .unwrap();
            assert!(!ticks.is_empty());
            assert!(
                ticks
                    .windows(2)
                    .all(|w| w[0].timestamp_ms <= w[1].timestamp_ms)
            );
            assert!(
                ticks
                    .iter()
                    .all(|t| t.timestamp_ms >= start.timestamp_millis()
                        && t.timestamp_ms < end.timestamp_millis())
            );
        });
    }
    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "requires saved Tradier credential and market-data network access"]
    fn tradier_weekly_and_monthly_history_live() {
        let token = crate::credentials::vibestrader_token().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for interval in [Interval::OneWeek, Interval::OneMonth] {
            let bars = rt
                .block_on(tradier_history("AAPL", &token, true, interval))
                .unwrap();
            assert!(!bars.is_empty(), "missing {interval:?} history");
        }
    }
    #[tokio::test]
    #[ignore = "requires network access"]
    async fn tradier_websocket_tls_connects_without_an_account_subscription() {
        let (mut socket, _) =
            tokio_tungstenite::connect_async("wss://ws.tradier.com/v1/markets/events")
                .await
                .expect("TLS and WebSocket handshake");
        socket.close(None).await.expect("close transport");
    }
    #[test]
    fn oversized_response_is_rejected_without_appending() {
        let mut bytes = vec![0; MAX_RESPONSE_BYTES];
        assert!(append_bounded(&mut bytes, &[1]).is_err());
        assert_eq!(bytes.len(), MAX_RESPONSE_BYTES);
    }
    #[test]
    fn omits_isolated_yahoo_after_hours_outliers_without_rewriting_prices() {
        let start = Utc.with_ymd_and_hms(2026, 11, 11, 21, 30, 0).unwrap();
        let mk = |i: i64, open, high, low, close, volume| {
            Bar::new(
                start + Duration::minutes(i * 5),
                open,
                high,
                low,
                close,
                volume,
            )
        };
        let mut bars = vec![
            mk(0, 760.0, 760.2, 759.9, 760.1, 0.0),
            mk(1, 759.89, 760.16, 711.46, 760.15, 0.0),
            mk(2, 711.46, 760.22, 711.46, 759.67, 0.0),
            mk(3, 759.7, 760.1, 759.5, 759.9, 0.0),
        ];
        assert_eq!(omit_suspect_yahoo_bars(&mut bars, Interval::FiveMinutes), 2);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].close, 759.9);
    }
    #[test]
    fn keeps_large_moves_with_volume_or_a_sustained_price_shift() {
        let start = Utc.with_ymd_and_hms(2026, 11, 11, 21, 30, 0).unwrap();
        let mk = |i: i64, price, low, volume| {
            Bar::new(
                start + Duration::minutes(i * 5),
                price,
                price,
                low,
                price,
                volume,
            )
        };
        let mut with_volume = vec![
            mk(0, 100.0, 100.0, 0.0),
            mk(1, 100.0, 90.0, 10.0),
            mk(2, 100.0, 100.0, 0.0),
        ];
        assert_eq!(
            omit_suspect_yahoo_bars(&mut with_volume, Interval::FiveMinutes),
            0
        );
        let mut shift = vec![
            mk(0, 100.0, 100.0, 0.0),
            mk(1, 100.0, 90.0, 0.0),
            mk(2, 90.0, 90.0, 0.0),
        ];
        assert_eq!(
            omit_suspect_yahoo_bars(&mut shift, Interval::FiveMinutes),
            0
        );
    }
    #[test]
    fn parses_tradier_daily_history() {
        let row =
            json!({"date":"2026-11-10","open":100,"high":105,"low":99,"close":103,"volume":500});
        let bar = parse_daily_bar(&row).unwrap();
        assert_eq!(
            bar.time,
            day_timestamp(chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap())
        );
        assert_eq!((bar.open, bar.close, bar.volume), (100.0, 103.0, 500.0));
    }
    #[test]
    fn parses_history_and_live_trade_in_documented_formats() {
        let bar = parse_bar(&json!({"timestamp":1751376600,"open":206.72,"high":208.38,"low":206.14,"close":207.53,"volume":9876387})).unwrap();
        assert_eq!(bar.time.timestamp(), 1751376600);
        assert_eq!(bar.close, 207.53);
        let trade =
            parse_trade(&json!({"date":"1751376600123","price":"207.54","size":"100"})).unwrap();
        assert_eq!(trade.timestamp_ms, 1751376600123);
        assert_eq!(trade.price, 207.54);
        assert_eq!(trade.size, 100.0);
    }
    #[test]
    fn rejects_invalid_trade_values() {
        assert!(parse_trade(&json!({"date":"bad","price":"1","size":"1"})).is_none());
        assert!(parse_trade(&json!({"date":"1751376600123","price":"NaN","size":"1"})).is_none());
        assert!(parse_trade(&json!({"date":"1751376600123","price":"0","size":"1"})).is_none());
    }
    #[test]
    fn regular_session_respects_calendar_and_early_close() {
        let calendar = json!({"calendar":{"days":{"day":[{"date":"2026-11-27","open":{"start":"09:30","end":"13:00"}},{"date":"2026-11-28","status":"closed"}]}}});
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let time = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert!(!inside_regular_session(&calendar, date, time(9, 29)));
        assert!(inside_regular_session(&calendar, date, time(9, 30)));
        assert!(inside_regular_session(&calendar, date, time(12, 59)));
        assert!(!inside_regular_session(&calendar, date, time(13, 0)));
        assert!(!inside_regular_session(
            &calendar,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 28).unwrap(),
            time(10, 0)
        ));
    }
    #[test]
    fn yahoo_excludes_incomplete_or_too_recent_bars() {
        let data = json!({"chart":{"result":[{"timestamp":[1000,1300,1600],"indicators":{"quote":[{"open":[1.0,2.0,3.0],"high":[1.2,2.2,3.2],"low":[0.8,1.8,2.8],"close":[1.1,2.1,3.1],"volume":[100,200,300]}]}}]}});
        let bars = parse_yahoo_bars(&data, 1600, Interval::FiveMinutes);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].time.timestamp(), 1300);
        assert_eq!(bars[1].close, 2.1);
    }
}
