# TradeCraft alpha

TradeCraft remains in alpha until announced otherwise. Versions use `0.minor.patch`: the minor number advances for feature milestones and the patch number for fixes and small improvements. This release is `0.2.0`, following the `0.1.0` preview. The workspace version in `Cargo.toml` is the source of truth; release tags and packages use the same version, such as `v0.2.0`. A version number is not reused for a different release.

A native Rust charting experiment using the egui/eframe stack and the development version of egui-charts. It loads one-, five-, fifteen-minute or daily chart history from Yahoo or Tradier; Tradier streams market price updates for charts and the watchlist over one shared connection. It does not request account information, positions, or order history. Chart and price timestamps use the computer's local time in 12-hour format.

Extended hours are enabled by default. Changing the symbol, interval or extended-hours setting reloads chart history; the shared Tradier stream updates its symbol list without a separate connection. Regular-hours mode uses Tradier's market calendar to filter live prices, including early closes. Tradier's available extended-hours history is limited.

The main window uses the egui-charts terminal, including its indicator catalog, study settings, drawing tools, chart types and layouts. Terminal settings, drawings, templates and named layouts are stored under the local TradeCraft application-data folder. Some upstream terminal features are incomplete or require services TradeCraft does not provide; see the architecture report.

The watchlist shows provider quotes and changes, and symbol search supports company names and tickers. Tradier watchlist prices update from the shared stream; an initial batched quote request supplies the previous close. Yahoo quotes are based on completed candles delayed by at least 15 minutes and refresh approximately once a minute. Provider availability and entitlements apply.

## Run

Install Rust, then run `cargo run --release`. The default Yahoo Finance chart uses an unofficial endpoint and is delayed by at least 15 minutes. Open the TradeCraft icon menu and choose Market data settings to select Tradier and enter your own production token. TradeCraft stores its own Tradier token in Windows Credential Locker, never in settings.json. The Yahoo endpoint is unofficial; check Yahoo's terms and data rights before distributing an app that uses it.

Community builds use `cargo build --release` and exclude the VibesTrader credential shortcut. Community provider preferences are stored separately in `settings-community.json`, so they start with Yahoo even if the personal build already uses Tradier. The personal build keeps its existing `settings.json`. Both files are under the local TradeCraft application-data folder. If both builds use a manually entered Tradier token, they share its Windows Credential Locker entry. For a personal build that includes the shortcut and updates the taskbar-pinned executable, run `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-personal.ps1`. The pin launches `target/personal/tradecraft.exe`; close TradeCraft before updating it if Windows has the executable locked.

The workspace layout, Craft standards audit, verification commands, and remaining gaps are documented in [ARCHITECTURE.md](ARCHITECTURE.md). Informal community releases include and link that compliance log so the community can review open findings.

Planned features and progress are tracked in [ROADMAP.md](ROADMAP.md).

Release changes are recorded in [CHANGELOG.md](CHANGELOG.md). Pine Script execution is not implemented. Tradier's Brokerage API does not provide futures; the app does not supply an overnight futures-derived SPX chart.

List the supported state commands with `cargo run -- --commands`. An isolated, offline command script can be run with `cargo run -- --script path/to/script.json`; it accepts a JSON array and prints the resulting workspace. It does not connect to market data, save settings, or control a running window.

This prototype uses real market data, but it is not ready to make trading decisions. After a Tradier stream interruption, TradeCraft requests missed chart history, then reconciles the forming candle at a completed one-minute boundary while buffering newer ticks. Recovery is limited by the provider's available history and is retried if data is unavailable. Initial history/stream overlap, exchange corrections and late trades still need further verification. The chart can show the last downloaded history while disconnected. Tradier may not send trades outside market hours. The prototype is read-only and does not place orders.

Tradier permits one market-data stream at a time. TradeCraft shares its stream across chart and watchlist symbols and updates its subscriptions as symbols change. Avoid opening a second market stream with the same account in another app. REST calls, including initial quote snapshots and symbol search, count against Tradier's per-token market-data rate limit. Alerts are in memory and only evaluated while the app is open.

TradeCraft source is available under the [MIT License](LICENSE). Third-party notices are listed in [ATTRIBUTION.md](ATTRIBUTION.md).
