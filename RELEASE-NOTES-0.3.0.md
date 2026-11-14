# TradeCraft 0.3.0 alpha

TradeCraft 0.3.0 expands the charting workspace with custom Pine indicators, multiple chart tabs, crypto market data and more ways to preserve your setup. It also adds new chart intervals and improves everyday chart and watchlist behavior.

This release builds on TradeCraft 0.2.0 with a more capable and personal charting workspace.

## Custom Pine indicators

TradeCraft now has a Pine editor and a library for custom indicators. Open **Indicators → Custom Pine** to create, edit, save, apply or delete scripts. Keep multiple scripts in your library and use them alongside built-in indicators.

- The interpreter supports a subset of Pine, including some v4 scripts, technical-analysis functions, maps, matrices, named arguments and candle plots.
- Applied scripts can display plots and tracked price levels on charts, with matching values and colors in the legend. The same script cannot be added twice to one chart.
- Applied studies retain their source in saved workspaces, so later edits to a library script do not silently change a chart's saved copy.
- Same-symbol daily requests can use provider daily history. An example US-session levels script is included.

This is the first release of TradeCraft's Pine indicator support, built around an expanding subset of the language.

## Charts and saved workspaces

- **Multiple chart tabs:** Open separate charts, switch between them and restore open tabs and their chart setups after restarting or changing providers.
- **More of your workspace saved:** Chart settings, indicators, drawings, watchlists, window and panel settings, and alert definitions are stored locally. Named layouts remain available separately.
- **Safer quitting:** TradeCraft waits for pending saves. If saving fails or takes too long, you can retry, return to the app or explicitly confirm quitting without saving. Settings and drawing writes also detect conflicts between running copies.
- **Smoother navigation:** The chart preserves your selected position and zoom as new candles arrive. The clock displays the selected chart timezone, and chart settings offer local, named and exchange-time choices where available.
- **Windows notification area:** Closing the window minimizes it to the notification area; reopen it from the icon or quit from its menu.

Alerts are saved between sessions and evaluated while TradeCraft is running.

## Market data and watchlists

- **Kraken crypto:** Search supported spot pairs by name or symbol, chart live ticker and candle data, and follow rolling 24-hour changes in the watchlist. Search and watchlists show recognizable asset names while retaining their underlying markets.
- **More Tradier intervals:** Weekly and monthly charts and recent individual trade ticks join the existing candle intervals for supported symbols.
- **More useful stock changes:** The Tradier watchlist preserves the last completed session's change during the overnight reset until regular trading resumes.

## Platform and what's next

The Windows community and personal builds retain separate provider preferences. The personal build continues to include the VibesTrader credential shortcut.

macOS support is underway, with native credential storage and packaging prepared for Apple Silicon and Intel.

TradeCraft remains alpha and read-only. For current feature limitations, provider details, release checks and the remaining macOS build and license work, see the [README](README.md), [architecture report](ARCHITECTURE.md), and [release checklist](RELEASE-CHECKLIST.md).
