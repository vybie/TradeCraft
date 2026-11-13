# Changelog

## Unreleased

- Updated the project description and documentation to describe TradeCraft as an alpha application.
- Updated the vendored egui-charts terminal to upstream revision `c9a7f0b`, retaining TradeCraft's integration, and added a scheduled upstream update check.

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
