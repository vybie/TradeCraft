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

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc};

#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub time: DateTime<Utc>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}
impl Bar {
    pub fn new(
        time: DateTime<Utc>,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> Self {
        Self {
            time,
            open,
            high,
            low,
            close,
            volume,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Trade {
    pub timestamp_ms: i64,
    pub price: f64,
    pub size: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    OneDay,
    OneWeek,
    OneMonth,
}
impl Interval {
    pub fn label(self) -> &'static str {
        match self {
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
            Self::FifteenMinutes => "15m",
            Self::OneDay => "1D",
            Self::OneWeek => "1W",
            Self::OneMonth => "1M",
        }
    }
    pub fn seconds(self) -> i64 {
        match self {
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
            Self::FifteenMinutes => 900,
            Self::OneDay => 86_400,
            Self::OneWeek => 7 * 86_400,
            Self::OneMonth => 32 * 86_400,
        }
    }
}

/// Start of the trading day, week, or calendar month containing a New York date.
pub fn market_period_date(date: NaiveDate, interval: Interval) -> NaiveDate {
    match interval {
        Interval::OneWeek => date
            .checked_sub_signed(chrono::Duration::days(i64::from(
                date.weekday().num_days_from_monday(),
            )))
            .unwrap_or(date),
        Interval::OneMonth => date.with_day(1).unwrap_or(date),
        _ => date,
    }
}

pub fn market_day_timestamp(date: NaiveDate) -> DateTime<Utc> {
    let morning = NaiveTime::from_hms_opt(9, 30, 0).unwrap_or(NaiveTime::MIN);
    let local = date.and_time(morning);
    chrono_tz::America::New_York
        .from_local_datetime(&local)
        .single()
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or(local.and_utc())
}
