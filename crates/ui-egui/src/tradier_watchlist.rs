//! Keep the last completed stock session's change visible across an overnight quote reset.
use chrono::{DateTime, NaiveDate, Utc};
use egui_charts::model::Quote;
use tradecraft_core::{Bar, Interval};
use tradecraft_providers::{market, reference::MarketQuote};

fn market_date(time: DateTime<Utc>) -> NaiveDate {
    time.with_timezone(&chrono_tz::America::New_York)
        .date_naive()
}

fn needs_previous_session(quote: &MarketQuote, now: DateTime<Utc>) -> bool {
    market_date(quote.time) < market_date(now) && quote.last == quote.previous_close
}

fn previous_session_close(quote: &MarketQuote, bars: &[Bar]) -> Option<f64> {
    let day = market_date(quote.time);
    // A stale or unrelated history response must never silently change the quote.
    let completed = bars.iter().find(|bar| market_date(bar.time) == day)?;
    if (completed.close - quote.last).abs() > 0.011 {
        return None;
    }
    bars.iter()
        .rev()
        .find(|bar| market_date(bar.time) < day && bar.close > 0.0)
        .map(|bar| bar.close)
}

pub async fn restore_completed_changes(
    token: &str,
    quotes: Vec<MarketQuote>,
    now: DateTime<Utc>,
) -> Vec<MarketQuote> {
    let mut restored = Vec::with_capacity(quotes.len());
    for chunk in quotes.chunks(4) {
        let mut pending = tokio::task::JoinSet::new();
        for mut quote in chunk.iter().cloned() {
            let token = token.to_owned();
            pending.spawn(async move {
                if needs_previous_session(&quote, now) {
                    let history = market::tradier_history_range(
                        &quote.symbol,
                        &token,
                        false,
                        Interval::OneDay,
                        quote.time - chrono::Duration::days(14),
                        Some(quote.time),
                    )
                    .await;
                    if let Ok(bars) = history
                        && let Some(close) = previous_session_close(&quote, &bars)
                    {
                        quote.previous_close = close;
                    }
                }
                quote
            });
        }
        while let Some(result) = pending.join_next().await {
            if let Ok(quote) = result {
                restored.push(quote);
            }
        }
    }
    restored
}

/// Merge a snapshot without losing a completed session's baseline to an overnight reset.
pub fn merge_snapshot(quote: &mut Quote, item: &MarketQuote) {
    let incoming_day = market_date(item.time);
    let current_day = market_date(quote.time);
    if incoming_day >= current_day {
        if !(incoming_day == current_day
            && item.last == item.previous_close
            && quote.last != quote.prev_close)
        {
            quote.prev_close = item.previous_close;
        }
        if item.time > quote.time {
            quote.last = item.last;
            quote.time = item.time;
        }
    }
}

/// On the first regular trade of a later session, use the prior session's last price as baseline.
pub fn begin_regular_session(quote: &mut Quote, time: DateTime<Utc>) {
    if market_date(time) > market_date(quote.time) {
        quote.prev_close = quote.last;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap()
    }
    #[test]
    fn keeps_completed_change_until_next_regular_session() {
        let item = MarketQuote {
            symbol: "TSLA".into(),
            last: 100.0,
            previous_close: 100.0,
            time: at(12, 21),
        };
        let bars = vec![
            Bar::new(at(11, 15), 90.0, 90.0, 90.0, 90.0, 0.0),
            Bar::new(at(12, 15), 100.0, 100.0, 100.0, 100.0, 0.0),
        ];
        assert!(needs_previous_session(&item, at(13, 8)));
        assert_eq!(previous_session_close(&item, &bars), Some(90.0));
        let mut quote = Quote::new(100.0, 90.0, item.time);
        merge_snapshot(&mut quote, &item);
        assert_eq!(quote.prev_close, 90.0);
        begin_regular_session(&mut quote, at(13, 15));
        quote.last = 102.0;
        quote.time = at(13, 15);
        assert_eq!(quote.prev_close, 100.0);
        merge_snapshot(&mut quote, &item);
        assert_eq!(quote.prev_close, 100.0);
    }
    #[test]
    fn ignores_mismatched_history_and_current_session_flat_quotes() {
        let item = MarketQuote {
            symbol: "TSLA".into(),
            last: 100.0,
            previous_close: 100.0,
            time: at(12, 21),
        };
        let bars = vec![Bar::new(at(12, 15), 101.0, 101.0, 101.0, 101.0, 0.0)];
        assert_eq!(previous_session_close(&item, &bars), None);
        assert!(!needs_previous_session(&item, at(12, 22)));
    }
    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "requires saved Tradier credentials and network access"]
    fn live_overnight_snapshot_restores_last_completed_change() {
        let token = tradecraft_providers::credentials::vibestrader_token().unwrap();
        let client = tradecraft_providers::reference::client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let original =
                tradecraft_providers::reference::tradier_quotes(&client, &token, &["TSLA".into()])
                    .await
                    .unwrap();
            assert_eq!(original.len(), 1);
            assert!(
                needs_previous_session(&original[0], Utc::now()),
                "Tradier no longer reports the overnight reset"
            );
            let restored = restore_completed_changes(&token, original, Utc::now()).await;
            assert_ne!(
                restored[0].last, restored[0].previous_close,
                "Last completed session's change was not restored"
            );
            println!(
                "TSLA last={} previous_close={}",
                restored[0].last, restored[0].previous_close
            );
        });
    }
}
