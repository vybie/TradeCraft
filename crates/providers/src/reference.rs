use crate::settings::Provider;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct SymbolMatch {
    pub symbol: String,
    pub description: String,
    pub exchange: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarketQuote {
    pub symbol: String,
    pub last: f64,
    pub previous_close: f64,
    pub time: DateTime<Utc>,
}

pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Mozilla/5.0 TradeCraft/0.2")
        .build()
        .map_err(|_| "Unable to initialize market data client".into())
}

async fn json(response: reqwest::Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!(
            "Market data unavailable (HTTP {})",
            response.status()
        ));
    }
    super::market::read_json(response).await
}

fn items(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    }
}

fn text(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap_or_default().to_owned()
}

fn parse_symbols(provider: Provider, data: &Value) -> Vec<SymbolMatch> {
    let entries = match provider {
        Provider::Tradier => items(&data["securities"]["security"]),
        Provider::Yahoo => items(&data["quotes"]),
    };
    entries
        .into_iter()
        .filter_map(|v| {
            let symbol = text(v, "symbol");
            if symbol.is_empty() || symbol.len() > 64 {
                return None;
            }
            let (description, exchange, kind) = match provider {
                Provider::Tradier => (text(v, "description"), text(v, "exchange"), text(v, "type")),
                Provider::Yahoo => (
                    v["shortname"]
                        .as_str()
                        .or_else(|| v["longname"].as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    text(v, "exchange"),
                    text(v, "quoteType"),
                ),
            };
            Some(SymbolMatch {
                symbol,
                description,
                exchange,
                kind,
            })
        })
        .collect()
}

pub async fn search(
    client: &reqwest::Client,
    provider: Provider,
    token: &str,
    query: &str,
) -> Result<Vec<SymbolMatch>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if query.len() > 64 {
        return Err("Search is too long".into());
    }
    let request = match provider {
        Provider::Tradier => client
            .get("https://api.tradier.com/v1/markets/search")
            .bearer_auth(token)
            .query(&[("q", query), ("indexes", "true")]),
        Provider::Yahoo => client
            .get("https://query1.finance.yahoo.com/v1/finance/search")
            .query(&[("q", query), ("quotesCount", "40"), ("newsCount", "0")]),
    };
    let response = request
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| "Symbol search unavailable".to_owned())?;
    let mut matches = parse_symbols(provider, &json(response).await?);
    if provider == Provider::Tradier && needs_ticker_lookup(query, &matches) {
        let lookup = client
            .get("https://api.tradier.com/v1/markets/lookup")
            .bearer_auth(token)
            .header("Accept", "application/json")
            .query(&[("q", query)])
            .send()
            .await;
        if let Ok(response) = lookup
            && let Ok(data) = json(response).await
        {
            merge_symbols(&mut matches, parse_symbols(provider, &data));
        }
    }
    Ok(matches)
}

fn needs_ticker_lookup(query: &str, matches: &[SymbolMatch]) -> bool {
    !query.is_empty()
        && query.len() <= 16
        && query
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '-' | '^'))
        && !matches
            .iter()
            .any(|item| item.symbol.eq_ignore_ascii_case(query))
}

fn merge_symbols(matches: &mut Vec<SymbolMatch>, lookup: Vec<SymbolMatch>) {
    for item in lookup {
        if !matches.iter().any(|existing| {
            existing.symbol.eq_ignore_ascii_case(&item.symbol)
                && existing.exchange.eq_ignore_ascii_case(&item.exchange)
        }) {
            matches.push(item);
        }
    }
}

fn parse_tradier_quotes(data: &Value) -> Vec<MarketQuote> {
    items(&data["quotes"]["quote"])
        .into_iter()
        .filter_map(|v| {
            let symbol = text(v, "symbol");
            let last = v["last"].as_f64()?;
            let previous_close = v["prevclose"].as_f64()?;
            let time = DateTime::<Utc>::from_timestamp_millis(v["trade_date"].as_i64()?)?;
            if symbol.is_empty()
                || !last.is_finite()
                || last <= 0.0
                || !previous_close.is_finite()
                || previous_close <= 0.0
            {
                return None;
            }
            Some(MarketQuote {
                symbol,
                last,
                previous_close,
                time,
            })
        })
        .collect()
}

pub async fn tradier_quotes(
    client: &reqwest::Client,
    token: &str,
    symbols: &[String],
) -> Result<Vec<MarketQuote>, String> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    let response = client
        .get("https://api.tradier.com/v1/markets/quotes")
        .bearer_auth(token)
        .header("Accept", "application/json")
        .query(&[("symbols", symbols.join(","))])
        .send()
        .await
        .map_err(|_| "Tradier quotes unavailable".to_owned())?;
    Ok(parse_tradier_quotes(&json(response).await?))
}

pub async fn tradier_calendar(
    client: &reqwest::Client,
    token: &str,
    year: i32,
    month: u32,
) -> Result<Value, String> {
    let response = client
        .get("https://api.tradier.com/v1/markets/calendar")
        .bearer_auth(token)
        .header("Accept", "application/json")
        .query(&[("year", year.to_string()), ("month", month.to_string())])
        .send()
        .await
        .map_err(|_| "Market calendar unavailable".to_owned())?;
    let data = json(response).await?;
    if data["calendar"]["days"]["day"].is_null() {
        return Err("Market calendar unavailable".into());
    }
    Ok(data)
}

fn parse_yahoo_quote(symbol: &str, data: &Value, now: i64) -> Option<MarketQuote> {
    let result = &data["chart"]["result"][0];
    let times = result["timestamp"].as_array()?;
    let closes = result["indicators"]["quote"][0]["close"].as_array()?;
    let values: Vec<_> = times
        .iter()
        .zip(closes)
        .filter_map(|(t, p)| {
            let end = t.as_i64()?.checked_add(300)?;
            let last = p.as_f64()?;
            (end <= now - 900 && last.is_finite() && last > 0.0).then_some((end, last))
        })
        .collect();
    let &(time, last) = values.last()?;
    let day = DateTime::<Utc>::from_timestamp(time - 300, 0)?
        .with_timezone(&chrono_tz::America::New_York)
        .date_naive();
    let previous_close = values.iter().rev().find_map(|(end, close)| {
        let date = DateTime::<Utc>::from_timestamp(end - 300, 0)?
            .with_timezone(&chrono_tz::America::New_York)
            .date_naive();
        (date < day).then_some(*close)
    })?;
    Some(MarketQuote {
        symbol: symbol.to_owned(),
        last,
        previous_close,
        time: DateTime::<Utc>::from_timestamp(time, 0)?,
    })
}

pub async fn yahoo_quote(
    client: &reqwest::Client,
    symbol: &str,
) -> Result<Option<MarketQuote>, String> {
    let mut url = reqwest::Url::parse("https://query1.finance.yahoo.com/v8/finance/chart/")
        .map_err(|_| "Invalid market data URL")?;
    let ticker = if symbol == "SPX" { "^GSPC" } else { symbol };
    url.path_segments_mut()
        .map_err(|_| "Invalid market data URL")?
        .pop_if_empty()
        .push(ticker);
    let response = client
        .get(url)
        .query(&[
            ("range", "5d"),
            ("interval", "5m"),
            ("includePrePost", "false"),
        ])
        .send()
        .await
        .map_err(|_| "Yahoo quotes unavailable".to_owned())?;
    Ok(parse_yahoo_quote(
        symbol,
        &json(response).await?,
        Utc::now().timestamp(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn tradier_parses_single_and_multiple_quotes_without_fabricating_values() {
        let quote =
            json!({"symbol":"TSLA","last":101.0,"prevclose":100.0,"trade_date":1751376600000i64});
        let result = parse_tradier_quotes(&json!({"quotes":{"quote":quote.clone()}}));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].last - result[0].previous_close, 1.0);
        assert_eq!(
            parse_tradier_quotes(&json!({"quotes":{"quote":[quote,{"symbol":"BAD","last":3}]}}))
                .len(),
            1
        );
    }
    #[test]
    fn yahoo_uses_only_completed_delayed_candles() {
        let day = 1_751_376_600i64;
        let data = json!({"chart":{"result":[{"timestamp":[day-86400,day,day+300],"indicators":{"quote":[{"close":[99.0,101.0,102.0]}]}}]}});
        let quote = parse_yahoo_quote("AAPL", &data, day + 1200).unwrap();
        assert_eq!(quote.last, 101.0);
        assert_eq!(quote.previous_close, 99.0);
        assert_eq!(quote.time.timestamp(), day + 300);
    }
    #[test]
    fn search_parses_provider_results() {
        let tradier = json!({"securities":{"security":{"symbol":"AAPL","description":"Apple Inc","exchange":"Q","type":"stock"}}});
        let yahoo = json!({"quotes":[{"symbol":"AAPL","shortname":"Apple Inc","exchange":"NMS","quoteType":"EQUITY"}]});
        assert_eq!(
            parse_symbols(Provider::Tradier, &tradier)[0].description,
            "Apple Inc"
        );
        assert_eq!(parse_symbols(Provider::Yahoo, &yahoo)[0].symbol, "AAPL");
    }

    #[test]
    fn ticker_lookup_fills_missing_match_without_duplicates() {
        let mut found = parse_symbols(
            Provider::Tradier,
            &json!({"securities":{"security":[
                {"symbol":"DOX","description":"Amdocs Ltd","exchange":"Q","type":"stock"}
            ]}}),
        );
        assert!(needs_ticker_lookup("amd", &found));
        let lookup = parse_symbols(
            Provider::Tradier,
            &json!({"securities":{"security":[
                {"symbol":"AMD","description":"Advanced Micro Devices Inc","exchange":"Q","type":"stock"},
                {"symbol":"DOX","description":"Amdocs Ltd","exchange":"Q","type":"stock"}
            ]}}),
        );
        merge_symbols(&mut found, lookup);
        assert_eq!(found.iter().filter(|s| s.symbol == "AMD").count(), 1);
        assert_eq!(found.iter().filter(|s| s.symbol == "DOX").count(), 1);
        assert!(!needs_ticker_lookup("amd", &found));
        assert!(!needs_ticker_lookup("Advanced Micro Devices", &found));
    }

    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "requires the saved personal Tradier credential and network access"]
    fn tradier_amd_search_case() {
        let token = crate::credentials::vibestrader_token().expect("saved Tradier credential");
        let client = client().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for query in ["amd", "AMD"] {
            let found = rt
                .block_on(search(&client, Provider::Tradier, &token, query))
                .unwrap();
            assert!(
                found.iter().any(|item| item.symbol == "AMD"),
                "missing AMD for {query}"
            );
        }
    }

    #[cfg(feature = "vibestrader-credential")]
    #[test]
    #[ignore = "requires the saved personal Tradier credential and network access"]
    fn tradier_search_and_batched_quotes_live() {
        let token = crate::credentials::vibestrader_token().expect("saved Tradier credential");
        let client = client().expect("market data client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let found = runtime
            .block_on(search(&client, Provider::Tradier, &token, "Apple"))
            .unwrap();
        assert!(found.iter().any(|item| item.symbol == "AAPL"));
        let quotes = runtime
            .block_on(tradier_quotes(
                &client,
                &token,
                &["AAPL".into(), "TSLA".into()],
            ))
            .unwrap();
        assert!(quotes.iter().any(|quote| quote.symbol == "AAPL"));
        assert!(quotes.iter().any(|quote| quote.symbol == "TSLA"));
    }

    #[test]
    #[ignore = "requires network access to Yahoo's unofficial API"]
    fn yahoo_search_and_delayed_quote_live() {
        let client = client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let found = runtime
            .block_on(search(&client, Provider::Yahoo, "", "Apple"))
            .unwrap();
        assert!(found.iter().any(|item| item.symbol == "AAPL"));
        let quote = runtime
            .block_on(yahoo_quote(&client, "AAPL"))
            .unwrap()
            .expect("delayed quote");
        assert!(quote.time.timestamp() <= Utc::now().timestamp() - 900);
    }
}
