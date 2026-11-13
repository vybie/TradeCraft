use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tradecraft_core::Trade;

#[derive(Debug)]
pub enum StreamEvent {
    Trade { symbol: String, trade: Trade },
    Status { connected: bool, message: String },
    RejectedSymbol(String),
}

fn subscription(session: &str, symbols: &[String]) -> Message {
    Message::Text(
        json!({"symbols":symbols,"filter":["trade"],"sessionid":session,"linebreak":true})
            .to_string()
            .into(),
    )
}

fn decode(text: &str) -> Vec<StreamEvent> {
    text.lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            if let Some(symbol) = value["error"]
                .as_str()
                .and_then(|error| error.strip_suffix(" is not a valid symbol"))
                && !symbol.is_empty()
                && symbol.len() <= 64
                && symbol
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '/'))
            {
                return Some(StreamEvent::RejectedSymbol(symbol.to_owned()));
            }
            if value.get("error").is_some() {
                return Some(StreamEvent::Status {
                    connected: false,
                    message: "Tradier rejected the stream subscription".into(),
                });
            }
            if value["type"] != "trade" {
                return None;
            }
            let symbol = value["symbol"].as_str()?.to_owned();
            let trade = crate::market::parse_trade(&value)?;
            Some(StreamEvent::Trade { symbol, trade })
        })
        .collect()
}

async fn connect(
    token: &str,
    symbols: &mut watch::Receiver<Vec<String>>,
    events: &mpsc::Sender<StreamEvent>,
) -> Result<(), String> {
    let client = crate::reference::client()?;
    let response = client
        .post("https://api.tradier.com/v1/markets/events/session")
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Content-Length", "0")
        .body("")
        .send()
        .await
        .map_err(|_| "Unable to create Tradier streaming session".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "Streaming session failed (HTTP {})",
            response.status()
        ));
    }
    let data = crate::market::read_json(response).await?;
    let session = data["stream"]["sessionid"]
        .as_str()
        .ok_or("Missing streaming session")?;
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(
            "wss://ws.tradier.com/v1/markets/events",
            Some(config),
            false,
        ),
    )
    .await
    .map_err(|_| "Tradier stream connection timed out".to_owned())?
    .map_err(|_| "Unable to connect to Tradier stream".to_owned())?;
    socket_loop(socket, session, symbols, events).await
}

async fn socket_loop<S>(
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    session: &str,
    symbols: &mut watch::Receiver<Vec<String>>,
    events: &mpsc::Sender<StreamEvent>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let current = symbols.borrow_and_update().clone();
    if current.is_empty() {
        return Ok(());
    }
    socket
        .send(subscription(session, &current))
        .await
        .map_err(|_| "Unable to subscribe to Tradier stream".to_owned())?;
    events
        .send(StreamEvent::Status {
            connected: true,
            message: "Stream connected · waiting for price updates".into(),
        })
        .await
        .map_err(|_| "Stream closed")?;
    loop {
        tokio::select! {
            changed = symbols.changed() => {
                if changed.is_err() { let _ = socket.close(None).await; return Ok(()); }
                let updated = symbols.borrow_and_update().clone();
                if updated.is_empty() { let _ = socket.close(None).await; return Ok(()); }
                socket.send(subscription(session, &updated)).await.map_err(|_| "Unable to update Tradier stream".to_owned())?;
            }
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => for event in decode(&text) {
                        let rejected = matches!(&event, StreamEvent::Status { connected: false, .. });
                        if events.send(event).await.is_err() { return Ok(()); }
                        if rejected { return Err("Tradier rejected the stream subscription".into()); }
                    },
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Err("Tradier stream disconnected".into()),
                    _ => {}
                }
            }
        }
    }
}

/// Runs at most one socket, updates its symbol list in place, and retries disconnects.
pub async fn run(
    token: String,
    mut symbols: watch::Receiver<Vec<String>>,
    events: mpsc::Sender<StreamEvent>,
) {
    let mut delay = 1;
    loop {
        if symbols.borrow().is_empty() && symbols.changed().await.is_err() {
            return;
        }
        if symbols.borrow().is_empty() {
            continue;
        }
        match connect(&token, &mut symbols, &events).await {
            Ok(()) => {
                delay = 1;
            }
            Err(message) => {
                if events
                    .send(StreamEvent::Status {
                        connected: false,
                        message,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(delay)) => {},
                    changed = symbols.changed() => if changed.is_err() { return; },
                }
                delay = (delay * 2).min(30);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscription_updates_symbol_list_without_changing_session_or_filter() {
        let Message::Text(payload) = subscription("session", &["SPY".into(), "TSLA".into()]) else {
            panic!("expected text")
        };
        let json: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(json["symbols"], json!(["SPY", "TSLA"]));
        assert_eq!(json["sessionid"], "session");
        assert_eq!(json["filter"], json!(["trade"]));
    }
    #[test]
    fn decodes_batched_trades_and_rejects_bad_values() {
        let result = decode(
            "{\"type\":\"trade\",\"symbol\":\"SPY\",\"price\":\"100\",\"size\":\"2\",\"date\":\"1751376600000\"}\n{\"type\":\"trade\",\"symbol\":\"TSLA\",\"price\":\"-1\",\"size\":\"2\",\"date\":\"1751376600000\"}",
        );
        assert_eq!(result.len(), 1);
        assert!(
            matches!(&result[0], StreamEvent::Trade { symbol, trade } if symbol == "SPY" && trade.price == 100.0)
        );
        assert!(
            matches!(&decode("{\"error\":\"BAD is not a valid symbol\"}")[0], StreamEvent::RejectedSymbol(symbol) if symbol == "BAD")
        );
    }

    #[tokio::test]
    async fn updates_symbols_and_receives_multiple_tickers_on_the_same_socket() {
        use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};
        let (client, server) = tokio::io::duplex(65536);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let (symbols_tx, mut symbols_rx) = watch::channel(vec!["SPY".to_owned()]);
        let (events_tx, mut events_rx) = mpsc::channel(10);
        let task = tokio::spawn(async move {
            socket_loop(client, "same-session", &mut symbols_rx, &events_tx).await
        });
        let Message::Text(first) = server.next().await.unwrap().unwrap() else {
            panic!("first subscription")
        };
        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap()["symbols"],
            json!(["SPY"])
        );
        assert!(matches!(
            events_rx.recv().await,
            Some(StreamEvent::Status {
                connected: true,
                ..
            })
        ));
        symbols_tx.send(vec!["SPY".into(), "TSLA".into()]).unwrap();
        let Message::Text(second) = server.next().await.unwrap().unwrap() else {
            panic!("updated subscription")
        };
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["symbols"], json!(["SPY", "TSLA"]));
        assert_eq!(second["sessionid"], "same-session");
        for symbol in ["SPY", "TSLA"] {
            server.send(Message::Text(json!({"type":"trade","symbol":symbol,"price":100,"size":1,"date":"1751376600000"}).to_string().into())).await.unwrap();
            assert!(
                matches!(events_rx.recv().await, Some(StreamEvent::Trade { symbol: received, .. }) if received == symbol)
            );
        }
        task.abort();
    }

    #[cfg(feature = "vibestrader-credential")]
    #[tokio::test]
    #[ignore = "opens a real Tradier market stream using the saved personal credential"]
    async fn live_stream_accepts_a_changed_symbol_list() {
        let token = crate::credentials::vibestrader_token().expect("saved Tradier credential");
        let (symbols_tx, symbols_rx) = watch::channel(vec!["SPY".to_owned()]);
        let (events_tx, mut events_rx) = mpsc::channel(100);
        let task = tokio::spawn(run(token, symbols_rx, events_tx));
        let connected = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match events_rx.recv().await {
                    Some(StreamEvent::Status {
                        connected: true, ..
                    }) => break true,
                    Some(StreamEvent::Status {
                        connected: false,
                        message,
                    }) => panic!("Stream connection: {message}"),
                    None => break false,
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(connected, "Tradier stream failed to connect");
        symbols_tx.send(vec!["SPY".into(), "TSLA".into()]).unwrap();
        let update_rejected = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    events_rx.recv().await,
                    Some(StreamEvent::Status {
                        connected: false,
                        ..
                    }) | None
                ) {
                    return true;
                }
            }
        })
        .await
        .unwrap_or(false);
        task.abort();
        assert!(!update_rejected, "Tradier rejected the updated symbol list");
    }
}
