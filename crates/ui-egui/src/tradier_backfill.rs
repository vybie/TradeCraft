//! Reconcile historical bars with live trades at a completed one-minute boundary.
use chrono::{DateTime, Duration, TimeZone, Utc};
use egui_charts::model::{BarClock, Timeframe};
use tradecraft_core::{Bar, Interval};

pub fn interval(timeframe: Timeframe) -> Option<Interval> {
    match timeframe {
        Timeframe::Min1 => Some(Interval::OneMinute),
        Timeframe::Min5 => Some(Interval::FiveMinutes),
        Timeframe::Min15 => Some(Interval::FifteenMinutes),
        Timeframe::Day1 => Some(Interval::OneDay),
        Timeframe::Week1 => Some(Interval::OneWeek),
        Timeframe::Month1 => Some(Interval::OneMonth),
        _ => None,
    }
}

pub fn next_minute(now: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp((now.timestamp().div_euclid(60) + 1) * 60, 0).unwrap_or(now)
}

/// Detect a materially delayed stream from an actual event timestamp, without assuming index entitlements.
pub fn delayed_cutoff(
    now: DateTime<Utc>,
    event: DateTime<Utc>,
) -> Option<(DateTime<Utc>, Duration)> {
    let lag = now.signed_duration_since(event);
    (lag > Duration::seconds(90)).then(|| (next_minute(event), lag))
}

/// One stale event can be a replayed last trade. Require an advancing event with a similar lag.
pub fn confirmed_delay(
    previous: (DateTime<Utc>, DateTime<Utc>),
    now: DateTime<Utc>,
    event: DateTime<Utc>,
) -> Option<(DateTime<Utc>, Duration)> {
    let (previous_event, previous_seen) = previous;
    let old_lag = previous_seen.signed_duration_since(previous_event);
    let (cutoff, lag) = delayed_cutoff(now, event)?;
    (event > previous_event && (lag - old_lag).num_seconds().abs() <= 10).then_some((cutoff, lag))
}

fn market_date(time: DateTime<Utc>) -> chrono::NaiveDate {
    time.with_timezone(&chrono_tz::America::New_York)
        .date_naive()
}

pub fn slot(clock: &BarClock, interval: Interval, time: DateTime<Utc>) -> DateTime<Utc> {
    if matches!(
        interval,
        Interval::OneDay | Interval::OneWeek | Interval::OneMonth
    ) {
        tradecraft_core::market_day_timestamp(tradecraft_core::market_period_date(
            market_date(time),
            interval,
        ))
    } else {
        clock.bar_open(time)
    }
}

pub fn partial_start(clock: &BarClock, interval: Interval, cutoff: DateTime<Utc>) -> DateTime<Utc> {
    let point = cutoff - Duration::milliseconds(1);
    if matches!(
        interval,
        Interval::OneDay | Interval::OneWeek | Interval::OneMonth
    ) {
        market_date(point)
            .and_hms_opt(0, 0, 0)
            .and_then(|time| {
                chrono_tz::America::New_York
                    .from_local_datetime(&time)
                    .single()
            })
            .map(|time| time.to_utc())
            .unwrap_or(point - Duration::days(1))
    } else {
        clock.bar_open(point)
    }
}

/// Rebuild a forming week/month from finished daily candles and the current day's complete minutes.
pub fn reconcile_calendar(
    history: Vec<Bar>,
    daily: Vec<Bar>,
    minutes: Vec<Bar>,
    clock: &BarClock,
    interval: Interval,
    cutoff: DateTime<Utc>,
) -> Vec<Bar> {
    let target_time = cutoff - Duration::milliseconds(1);
    let target = slot(clock, interval, target_time);
    let current_date = market_date(target_time);
    let mut parts: Vec<Bar> = daily
        .into_iter()
        .filter(|b| market_date(b.time) < current_date && slot(clock, interval, b.time) == target)
        .collect();
    parts.extend(minutes.into_iter().filter(|b| {
        market_date(b.time) == current_date
            && b.time + Duration::minutes(1) <= cutoff
            && slot(clock, interval, b.time) == target
    }));
    parts.sort_by_key(|b| b.time);
    let mut output = completed(history, clock, interval, target_time);
    if let Some(first) = parts.first() {
        let mut aggregate = Bar::new(target, first.open, first.high, first.low, first.close, 0.0);
        for part in parts {
            aggregate.high = aggregate.high.max(part.high);
            aggregate.low = aggregate.low.min(part.low);
            aggregate.close = part.close;
            aggregate.volume += part.volume;
        }
        output.push(aggregate);
    }
    output
}

/// Complete bars before the current slot are safe to replace without touching newer stream ticks.
pub fn completed(
    bars: Vec<Bar>,
    clock: &BarClock,
    interval: Interval,
    cutoff: DateTime<Utc>,
) -> Vec<Bar> {
    let boundary = slot(clock, interval, cutoff);
    bars.into_iter()
        .filter(|bar| slot(clock, interval, bar.time) < boundary)
        .collect()
}

/// Aggregate only complete minutes belonging to the candle just before `cutoff`.
pub fn partial(
    minutes: Vec<Bar>,
    clock: &BarClock,
    interval: Interval,
    cutoff: DateTime<Utc>,
) -> Option<Bar> {
    let target = slot(clock, interval, cutoff - Duration::milliseconds(1));
    let mut bars = minutes.into_iter().filter(|bar| {
        bar.time + Duration::minutes(1) <= cutoff && slot(clock, interval, bar.time) == target
    });
    let first = bars.next()?;
    let mut result = Bar::new(
        target,
        first.open,
        first.high,
        first.low,
        first.close,
        first.volume,
    );
    for bar in bars {
        result.high = result.high.max(bar.high);
        result.low = result.low.min(bar.low);
        result.close = bar.close;
        result.volume += bar.volume;
    }
    Some(result)
}

/// Prefer the provider's finished candle at an interval boundary; otherwise rebuild its partial candle from complete minutes.
pub fn reconcile(
    history: Vec<Bar>,
    minutes: Vec<Bar>,
    clock: &BarClock,
    interval: Interval,
    cutoff: DateTime<Utc>,
) -> Vec<Bar> {
    let target = slot(clock, interval, cutoff - Duration::milliseconds(1));
    let boundary = slot(clock, interval, cutoff);
    let finished = (boundary > target)
        .then(|| {
            history
                .iter()
                .find(|bar| slot(clock, interval, bar.time) == target)
                .cloned()
        })
        .flatten();
    let mut bars = completed(history, clock, interval, cutoff - Duration::milliseconds(1));
    if let Some(bar) = finished.or_else(|| partial(minutes, clock, interval, cutoff)) {
        bars.push(bar);
    }
    bars
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weekly_and_monthly_recovery_joins_completed_days_and_current_minutes() {
        let time = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        for interval in [Interval::OneWeek, Interval::OneMonth] {
            let clock = BarClock::new(if interval == Interval::OneWeek {
                Timeframe::Week1
            } else {
                Timeframe::Month1
            });
            let daily = vec![
                Bar::new(
                    time("2026-11-09T14:30:00Z"),
                    100.0,
                    105.0,
                    99.0,
                    103.0,
                    50.0,
                ),
                Bar::new(
                    time("2026-11-10T14:30:00Z"),
                    103.0,
                    107.0,
                    102.0,
                    106.0,
                    70.0,
                ),
            ];
            let minutes = vec![
                Bar::new(
                    time("2026-11-11T14:30:00Z"),
                    106.0,
                    109.0,
                    105.0,
                    108.0,
                    10.0,
                ),
                Bar::new(
                    time("2026-11-11T14:31:00Z"),
                    108.0,
                    110.0,
                    107.0,
                    109.0,
                    20.0,
                ),
            ];
            let bars = reconcile_calendar(
                vec![],
                daily,
                minutes,
                &clock,
                interval,
                time("2026-11-11T14:32:00Z"),
            );
            assert_eq!(bars.len(), 1);
            assert_eq!(
                (
                    bars[0].open,
                    bars[0].high,
                    bars[0].low,
                    bars[0].close,
                    bars[0].volume
                ),
                (100.0, 110.0, 99.0, 109.0, 150.0)
            );
        }
    }
    #[test]
    fn backfill_reconciles_only_completed_minutes_and_preserves_volume() {
        let clock = BarClock::new(Timeframe::Min5);
        let start = DateTime::<Utc>::from_timestamp(1_751_376_600, 0).unwrap();
        let bars: Vec<_> = (0..5)
            .map(|i| {
                Bar::new(
                    start + Duration::minutes(i),
                    100.0 + i as f64,
                    102.0 + i as f64,
                    99.0 + i as f64,
                    101.0 + i as f64,
                    10.0,
                )
            })
            .collect();
        let cutoff = start + Duration::minutes(3);
        let bar = partial(bars.clone(), &clock, Interval::FiveMinutes, cutoff).unwrap();
        assert_eq!(
            (bar.open, bar.high, bar.low, bar.close, bar.volume),
            (100.0, 104.0, 99.0, 103.0, 30.0)
        );
        assert!(completed(bars, &clock, Interval::FiveMinutes, cutoff).is_empty());
    }
    #[test]
    fn daily_slots_follow_new_york_calendar_not_utc_midnight() {
        let clock = BarClock::new(Timeframe::Day1);
        let a = DateTime::parse_from_rfc3339("2026-11-12T23:55:00Z")
            .unwrap()
            .to_utc();
        let b = DateTime::parse_from_rfc3339("2026-11-13T00:01:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            slot(&clock, Interval::OneDay, a),
            slot(&clock, Interval::OneDay, b)
        );
    }

    #[test]
    fn recovered_candle_replaces_overlapping_ticks_before_buffered_ticks_replay() {
        use egui_charts::data::{Datafeed, HistoryPage, HistoryRequest, Reply, Sink, Subscription};
        use egui_charts::model::Tick;
        use egui_charts::widget::SeriesId;
        use std::sync::{Arc, Mutex};
        struct Feed {
            start: DateTime<Utc>,
            sink: Mutex<Option<Sink>>,
        }
        impl Datafeed for Feed {
            fn history(&self, _: HistoryRequest, reply: Reply<HistoryPage>) {
                reply.send(Ok(HistoryPage::new(
                    vec![egui_charts::model::Bar::new(
                        self.start, 100.0, 101.0, 99.0, 100.0, 10.0,
                    )],
                    true,
                )));
            }
            fn subscribe(&self, sub: Subscription) {
                *self.sink.lock().unwrap() = Some(sub.sink);
            }
        }
        let start =
            DateTime::<Utc>::from_timestamp(Utc::now().timestamp().div_euclid(300) * 300 - 300, 0)
                .unwrap();
        let feed = Arc::new(Feed {
            start,
            sink: Mutex::new(None),
        });
        let chart = egui_charts::ChartBuilder::new()
            .with_feed(feed.clone())
            .with_symbol("TSLA")
            .with_timeframe(Timeframe::Min5)
            .build();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                |ui, chart: &mut egui_charts::Chart| {
                    chart.show(ui);
                },
                chart,
            );
        h.run_steps(4);
        let sink = feed
            .sink
            .lock()
            .unwrap()
            .clone()
            .expect("chart subscription");
        sink.push_tick(Tick::new(start + Duration::seconds(1), 102.0, 2.0, None));
        h.run_steps(2);
        sink.push_bar(egui_charts::model::Bar::new(
            start, 100.0, 103.0, 99.0, 102.0, 15.0,
        ));
        sink.push_tick(Tick::new(start + Duration::seconds(2), 104.0, 3.0, None));
        h.run_steps(2);
        let bar = h.state().bars(SeriesId::MAIN).last().unwrap();
        assert_eq!(
            (bar.open, bar.high, bar.low, bar.close, bar.volume),
            (100.0, 104.0, 99.0, 104.0, 18.0)
        );
    }

    #[test]
    fn delayed_stream_uses_event_time_for_reconciliation() {
        let now = DateTime::<Utc>::from_timestamp(1_751_377_000, 0).unwrap();
        let delayed = now - Duration::minutes(15);
        let (cutoff, lag) = delayed_cutoff(now, delayed).unwrap();
        assert!(cutoff > delayed && cutoff <= delayed + Duration::minutes(1));
        assert_eq!(lag, Duration::minutes(15));
        assert!(delayed_cutoff(now, now - Duration::seconds(3)).is_none());
        assert!(
            confirmed_delay(
                (delayed, now),
                now + Duration::seconds(2),
                delayed + Duration::seconds(2)
            )
            .is_some()
        );
        assert!(confirmed_delay((delayed, now), now + Duration::seconds(2), delayed).is_none());
    }

    #[cfg(feature = "vibestrader-credential")]
    #[tokio::test]
    #[ignore = "requires the saved personal Tradier credential and network access"]
    async fn tradier_minutes_reconstruct_a_completed_five_minute_candle() {
        let token = tradecraft_engine::tradier_token(true).expect("saved Tradier credential");
        let history = tradecraft_providers::market::tradier_history(
            "TSLA",
            &token,
            false,
            Interval::FiveMinutes,
        )
        .await
        .unwrap();
        let bar = history
            .iter()
            .rev()
            .find(|bar| bar.time < Utc::now() - Duration::minutes(10))
            .expect("completed candle");
        let cutoff = bar.time + Duration::minutes(5);
        let minutes = tradecraft_providers::market::tradier_history_range(
            "TSLA",
            &token,
            false,
            Interval::OneMinute,
            bar.time,
            Some(cutoff),
        )
        .await
        .unwrap();
        let clock = BarClock::new(Timeframe::Min5);
        let rebuilt = reconcile(
            vec![bar.clone()],
            minutes,
            &clock,
            Interval::FiveMinutes,
            cutoff,
        )
        .pop()
        .expect("reconciled candle");
        assert!((rebuilt.open - bar.open).abs() < 0.02);
        assert!((rebuilt.high - bar.high).abs() < 0.02);
        assert!((rebuilt.low - bar.low).abs() < 0.02);
        assert!((rebuilt.close - bar.close).abs() < 0.02);
        assert_eq!(rebuilt.volume, bar.volume);
    }
}
