//! Search Kraken assets by name while keeping their exact market identifiers.
use futures_util::{StreamExt, stream};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CryptoMarket {
    pub pair: String,
    pub name: String,
}

// A small offline fallback also lets common asset names be searched before the first lookup.
fn known_name(base: &str) -> Option<&'static str> {
    match base {
        "BTC" => Some("Bitcoin"),
        "ETH" => Some("Ethereum"),
        "SOL" => Some("Solana"),
        "DOGE" => Some("Dogecoin"),
        "XRP" => Some("XRP"),
        "ADA" => Some("Cardano"),
        "AVAX" => Some("Avalanche"),
        "DOT" => Some("Polkadot"),
        "LINK" => Some("Chainlink"),
        "LTC" => Some("Litecoin"),
        "USDT" => Some("Tether"),
        "USDC" => Some("USD Coin"),
        _ => None,
    }
}

fn quote_rank(quote: &str) -> u8 {
    match quote {
        "USD" => 0,
        "USDC" => 1,
        "USDT" => 2,
        "EUR" => 3,
        _ => 4,
    }
}

fn select_markets(pairs: &[String], text: &str, names: &HashMap<String, String>) -> Vec<String> {
    let text = text.trim().to_ascii_uppercase();
    let query = text.strip_prefix("KRAKEN:").unwrap_or(&text);
    let explicit_pair = query.contains('/');
    let mut candidates = BTreeMap::<String, Vec<String>>::new();
    for pair in pairs {
        let Some((base, quote)) = pair.split_once('/') else {
            continue;
        };
        let name = names
            .get(base)
            .map(String::as_str)
            .or_else(|| known_name(base))
            .unwrap_or("")
            .to_ascii_uppercase();
        if !(if explicit_pair {
            pair.contains(query)
        } else {
            base.contains(query) || name.contains(query)
        }) {
            continue;
        }
        candidates
            .entry(base.to_owned())
            .or_default()
            .push(format!("{base}/{quote}"));
    }
    let mut chosen = Vec::new();
    for (base, mut markets) in candidates {
        markets.sort_by_key(|pair| {
            (
                quote_rank(pair.split_once('/').map_or("", |(_, q)| q)),
                pair.clone(),
            )
        });
        if !explicit_pair {
            markets.truncate(1);
        }
        let rank = if base == query {
            0
        } else if base.starts_with(query) {
            1
        } else {
            2
        };
        chosen.extend(markets.into_iter().map(|pair| (rank, pair)));
    }
    chosen.sort_by_key(|(rank, pair)| (*rank, pair.clone()));
    chosen.into_iter().map(|(_, pair)| pair).collect()
}

static NAMES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Returns a known or previously resolved asset name without making a network request.
pub fn cached_name(base: &str) -> Option<String> {
    let cache = NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    let saved = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    saved
        .get(base)
        .cloned()
        .or_else(|| known_name(base).map(str::to_owned))
}

/// Resolves names for saved watchlist markets in the background.
pub async fn warm_watchlist_names(client: &reqwest::Client, pairs: &[String]) {
    for pair in pairs.iter().take(64) {
        if let Some((base, _)) = pair.split_once('/')
            && cached_name(base).is_none()
        {
            let _ = search_catalog(client, std::slice::from_ref(pair), pair).await;
        }
    }
}

/// Prefer a single USD market for ordinary asset searches; explicit pair searches remain available.
pub async fn search_catalog(
    client: &reqwest::Client,
    pairs: &[String],
    text: &str,
) -> Vec<CryptoMarket> {
    let cache = NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    let selected = {
        let saved = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        select_markets(pairs, text, &saved)
    };
    let missing: Vec<_> = {
        let saved = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        selected
            .iter()
            .filter_map(|pair| {
                let (base, _) = pair.split_once('/')?;
                (text.trim().len() >= 2 && !saved.contains_key(base) && known_name(base).is_none())
                    .then(|| (base.to_owned(), pair.clone()))
            })
            .take(8)
            .collect()
    };
    let lookups = stream::iter(missing.into_iter().map(|(base, pair)| async move {
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let response = client
                .get("https://api.kraken.com/0/public/PreTrade")
                .query(&[("symbol", &pair)])
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            let value = crate::market::read_json(response).await.ok()?;
            if value["result"]["base_asset"].as_str()? != base {
                return None;
            }
            let description = value["result"]["description"].as_str()?;
            let name = description.split_once(" / ")?.0.trim();
            (!name.is_empty() && name.len() <= 100).then(|| name.to_owned())
        })
        .await
        .ok()
        .flatten();
        (base, result)
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    let mut saved = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (base, name) in lookups {
        if let Some(name) = name {
            saved.insert(base, name);
        }
    }
    selected
        .into_iter()
        .map(|pair| {
            let base = pair.split_once('/').map_or(pair.as_str(), |(base, _)| base);
            let name = saved
                .get(base)
                .map(String::as_str)
                .or_else(|| known_name(base))
                .unwrap_or(base)
                .to_owned();
            CryptoMarket { pair, name }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asset_search_prefers_one_usd_market_and_supports_explicit_pairs() {
        let pairs =
            ["SOL/USDT", "SOL/USD", "SOL/USD1", "SOLV/USD", "JITOSOL/USD"].map(str::to_owned);
        let names = HashMap::from([("SOLV".into(), "Solv Protocol".into())]);
        assert_eq!(
            select_markets(&pairs, "sol", &names),
            ["SOL/USD", "SOLV/USD", "JITOSOL/USD"]
        );
        assert_eq!(select_markets(&pairs, "Solana", &names), ["SOL/USD"]);
        assert_eq!(
            select_markets(&pairs, "solv protocol", &names),
            ["SOLV/USD"]
        );
        assert_eq!(select_markets(&pairs, "SOL/USDT", &names), ["SOL/USDT"]);
        assert_eq!(
            select_markets(&pairs, "KRAKEN:SOL/USD1", &names),
            ["SOL/USD1"]
        );
    }

    #[test]
    #[ignore = "requires Kraken public market metadata"]
    fn kraken_metadata_supplies_a_name_for_an_unseeded_asset() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = crate::reference::client().unwrap();
        let markets = runtime.block_on(search_catalog(&client, &["SOLV/USD".into()], "SOLV"));
        assert_eq!(
            markets,
            [CryptoMarket {
                pair: "SOLV/USD".into(),
                name: "Solv Protocol".into()
            }]
        );
    }
}
