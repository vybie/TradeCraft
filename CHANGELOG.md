# Changelog

## Unreleased

## 0.3.0 — alpha

- Add multiple chart tabs and restore saved workspaces, including applied Pine studies, when restarting or changing providers.
- Persist watchlists, window and panel settings, and alert definitions; detect settings and drawing write conflicts. Alerts are evaluated only while the app is running.
- Wait for pending saves on quit and offer retry, return to the app, or a separately confirmed quit without saving; on Windows, closing the window minimizes it to the notification area.
- Add Kraken spot-pair search, live chart data and watchlist quotes, including rolling 24-hour changes.
- Add Tradier weekly and monthly charts and recent individual trade-tick history, subject to provider availability and history limits.
- Make the Custom Pine picker and editor support creating, editing, saving and deleting scripts from the library.
- Match chart-legend values to their plot colors and describe Pine support in TradeCraft's own terms.
- Show tracked Pine values in chart legends and prevent adding the same Pine script twice to one chart.
- Add per-script deletion from the Custom Pine picker and distinguish scripts with matching names.
- Keep unchanged drawing metadata stable when saving the workspace so normal quits can finish.
- Use provider daily history for Pine daily security requests, and add a US-session levels script with a solid previous-close line.
- Added an experimental Pine indicator editor using a partial interpreter; compatibility and realtime behavior remain under investigation.
- Extended experimental Pine indicators with `ta.macd` and `ta.cross` and corrected `ta.valuewhen` occurrence indexing. Broader compatibility remains unverified.
- Integrated newer Pine language support from DimmOFF's fork, including v4 bare builtins, maps, matrices, named arguments and candle plots. Live-bar state remains approximate.
- Show the chart's actual time-zone abbreviation next to the clock without a pop-up.
- Add local and instrument exchange time choices to chart settings, with UTC as the fallback when the exchange time zone is unknown.
- Preserve the chart's chosen position and zoom as new candles arrive.
- Updated the project description and documentation to describe TradeCraft as an alpha application.
- Updated the vendored egui-charts terminal to upstream revision `c9a7f0b`, retaining TradeCraft's integration, and added a scheduled upstream update check.
- Improved Kraken search results to show crypto names, favor a single USD market per asset, and preserve explicit pair searches.
- Show crypto names in the watchlist while retaining the underlying Kraken markets for quotes and selection.
- Preserve the last completed stock session's watchlist change when Tradier resets overnight, until regular trading resumes.

## 0.2.0 — alpha

Changes since the published 0.1.0 preview:

### Charting and workspace

- Integrated the egui-charts terminal, including its indicator catalog, drawing tools, chart types and layouts.
- Added local storage for terminal settings, drawings, templates and named layouts; restoration still needs verification.
- Added provider-backed symbol search and watchlist quotes.

### Market data

- Added Tradier chart history and live price updates, alongside delayed Yahoo data. Extended-hours history is enabled by default when available.
- Shared one Tradier stream across chart and watchlist symbols, with initial REST snapshots and local-time chart timestamps.
- Added a live-price update timestamp to the Tradier stream status when an update is received.
- Added automatic stream reconnection and recovery of available chart history, with reconciliation of recovered bars and buffered live ticks.

### Builds and architecture

- Reorganized the application into core, provider, engine and UI crates; documented outstanding architecture and dependency compliance findings in `ARCHITECTURE.md`.
- Added separate community and personal provider settings, with the VibesTrader credential shortcut limited to personal builds.
- Added a stable personal executable path and build script for the taskbar shortcut.
- Added the TradeCraft crosshair application icon and Windows GUI executable configuration.

### Known limitations

- Live updates depend on provider availability and market activity; overnight SPX futures data is not included.
- Reconnect recovery is limited by available provider history; corrections, some upstream terminal services, and full workspace restoration remain incomplete or need verification. See `README.md` and `ARCHITECTURE.md`.
