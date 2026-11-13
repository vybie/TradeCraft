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

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};

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
}
impl Interval {
    pub fn label(self) -> &'static str {
        match self {
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
            Self::FifteenMinutes => "15m",
            Self::OneDay => "1D",
        }
    }
    pub fn seconds(self) -> i64 {
        match self {
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
            Self::FifteenMinutes => 900,
            Self::OneDay => 86_400,
        }
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
