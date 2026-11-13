# TradeCraft 0.2.0 alpha

TradeCraft 0.2.0 replaces the initial prototype window with the egui-charts terminal and expands the market-data integration. This is an early, read-only community preview.

## Charting workspace

- The egui-charts terminal interface, with chart styles, drawing tools, indicator catalog and study settings, and multiple chart layouts.
- Local terminal storage for settings, drawings, templates and named layouts. Restoration and recovery still need broader verification.
- Company-name and ticker search and a watchlist with price, change and percentage change from the selected provider.
- One-, five- and fifteen-minute and daily chart history, local-time labels, and extended-hours data enabled by default where available.

## Market data

- Tradier chart and watchlist symbols share a single dynamically updated market-data stream, with REST snapshots for quote initialization. After interruptions, the stream reconnects and recovers available chart history, reconciling the recovered bars with buffered live ticks.
- Status text shows when a price update was received; a connected stream does not guarantee a particular symbol is updating. Provider entitlements and trading sessions determine availability.
- Yahoo provides an alternative delayed data source. Its unofficial endpoint is subject to access and distribution restrictions, which remain under review.
- Tradier Brokerage API does not supply S&P futures. SPX may stop updating outside its available session; this release does not provide an overnight futures-derived SPX feed.

## Windows and project structure

- Windows builds use the TradeCraft crosshair icon and open without a separate console window.
- The community build uses its own provider preferences and does not include the personal VibesTrader credential shortcut. The personal build has a stable executable path for taskbar shortcuts.
- The source is organized into core, provider, engine and UI crates. The [architecture and compliance audit](https://github.com/vybie/TradeCraft/blob/v0.2.0/ARCHITECTURE.md) records remaining Craft standards gaps and dependency-license findings; this is an independent project, not an official Craft release.

## Getting started

Run the Windows community build, open the TradeCraft icon menu, and select **Market data settings** to configure Tradier with your own production token. Without Tradier configuration, Yahoo is the default delayed source. See the [README](https://github.com/vybie/TradeCraft/blob/v0.2.0/README.md) for build instructions, credential storage and source limitations.

## Known limitations

- This is alpha software and is not ready for trading decisions. It does not place trades or request account balances, positions, or order history.
- Some egui-charts terminal features need services that TradeCraft does not provide. Pine Script execution is not implemented.
- Alerts are in memory and evaluated only while the app is open. Workspace restoration, crash recovery and multi-process storage need further verification.
- Market data can be delayed, unavailable or stale. Reconnect recovery depends on available history and has not yet been verified through a real interruption during active market hours; initial-load overlap, corrections and late trades need further work. Avoid a second Tradier market stream with the same account.
- Craft license-allowlist checks report unresolved findings, and Yahoo data distribution rights remain unresolved. See the [audit](https://github.com/vybie/TradeCraft/blob/v0.2.0/ARCHITECTURE.md) and [attribution](https://github.com/vybie/TradeCraft/blob/v0.2.0/ATTRIBUTION.md).
