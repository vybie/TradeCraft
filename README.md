# TradeCraft prototype

A native Rust charting experiment using the egui/eframe stack and the development version of egui-charts. It loads one-, five-, fifteen-minute or daily chart history from Yahoo or Tradier; Tradier also streams market price updates for one symbol. It does not request account information, positions, or order history. Chart and price timestamps use the computer's local time in 12-hour format.

Extended hours are enabled by default. Changing the symbol, interval or extended-hours setting reloads history and reconnects the stream. Regular-hours mode uses Tradier's market calendar to filter live prices, including early closes. Tradier's available extended-hours history is limited.

The chart supports candles, bars and lines. The Indicators menu lets you add multiple SMA, EMA, RSI, MACD and Bollinger Bands with configurable periods (and band deviation); click an active indicator label to remove it. Indicator settings are not saved between launches.

The right-hand watchlist lets you add, remove and select symbols. Prices are shown only for the active chart; other watchlist entries do not have live quotes yet. Watchlist changes are not saved between launches.

## Run

Install Rust, then run `cargo run --release`. The default Yahoo Finance chart uses an unofficial endpoint and is delayed by at least 15 minutes. Under Settings → Market data, choose Tradier and enter your own production token. TradeCraft stores its own Tradier token in Windows Credential Locker, never in settings.json. The Yahoo endpoint is unofficial; check Yahoo's terms and data rights before distributing an app that uses it.

Community builds use `cargo build --release` and exclude the VibesTrader credential shortcut. For a personal build that includes it, use `cargo run --release --features vibestrader-credential`.

This prototype uses real market data, but it is not ready to make trading decisions. History is fetched before the stream starts, leaving a possible gap at connection. It does not automatically reconnect, backfill gaps, or handle corrections and late trades. The chart can show the last downloaded history while disconnected. Tradier may not send trades outside market hours. The prototype is read-only and does not place orders.

Tradier permits one market-data stream at a time. Avoid opening a second market stream with the same account while this prototype is connected. REST calls count against Tradier's per-token market-data rate limit.
