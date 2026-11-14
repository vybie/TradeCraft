# TradeCraft roadmap

TradeCraft is an independent native Rust charting app. The near-term goal is a useful, dependable charting workspace for US stocks and indices, with configurable indicators and a full-window interface.

This is a working plan. Check items only after implementation and verification. The current workspace contains local changes beyond the published v0.1.0 preview; see [ARCHITECTURE.md](ARCHITECTURE.md) for the architecture audit and open issues.

## Current baseline

- [x] Native egui/wgpu window using the egui-charts terminal, including its chart, toolbars, indicator catalog, drawing tools, settings and watchlist UI.
- [x] 1m, 5m, 15m, and 1D candles; candles, bars, and line chart styles.
- [x] Extended-hours option on by default; chart timestamps use local time and AM/PM.
- [x] Use the upstream indicator catalog and settings UI. Saved layouts and drawings use local storage; restoration and watchlist persistence still need verification.
- [x] Tradier market-data connection using the user's own credentials; provisional delayed Yahoo data without credentials. Coverage, timeliness, and availability depend on the provider.
- [x] Local source restructured into core, providers, engine, and UI crates; a community build excludes the personal VibesTrader shortcut.
- [x] GitHub repository and an early Windows preview are public. The published preview predates the local architecture work.

## M1 — Indicators and chart setup

**Goal:** Make the indicators the user relies on easy to configure and reuse.

- [ ] **M1.1** Add settings for each existing indicator: edit its parameters after adding it, change supported colors and line widths, show/hide it, and remove it without accidentally deleting it when opening settings.
- [ ] **M1.2** Make it straightforward to use multiple SMA and EMA instances, including periods 13, 21, 50, 100, and 200, with independent settings. These are useful presets, not fixed limits.
- [ ] **M1.3** Improve the Indicators picker: find an indicator by name and distinguish available indicators from those already on the chart.
- [ ] **M1.4** Verify RSI and MACD pane placement, labels, scaling, and interaction against their intended behavior; fix gaps found. Keep overlays such as moving averages and Bollinger Bands on the price chart.
- [ ] **M1.5** Validate indicator values and warm-up behavior against independently calculated fixtures, including switching symbol and interval and receiving a new candle.

**Done when:** A user can add, edit, hide, show, and remove independent indicator instances, including the moving-average periods above, and see correct results on the appropriate chart or pane.

## M2 — Save the workspace

**Goal:** Return to the same useful chart setup after restarting the app.

- [ ] **M2.1** Save and restore symbol, interval, chart style, extended-hours choice, and active indicators with their settings and visibility.
- [ ] **M2.2** Save and restore the watchlist and its order. Keep credentials in the OS credential store, outside workspace files.
- [ ] **M2.3** Support saved indicator templates so a moving-average setup can be applied again. Define whether a template includes the symbol or interval before implementing it.
- [ ] **M2.4** Handle missing, old, or corrupt workspace files without losing the last usable configuration; version the saved format and test restoration and migrations.

**Done when:** Close and reopen TradeCraft and the chart and watchlist return as configured, including independent indicator settings. A saved indicator template can be reapplied.

## M3 — Market-data reliability

**Goal:** Make data freshness and gaps visible and improve chart continuity.

- [ ] **M3.1** Show the selected provider, live/delayed/disconnected state, and the actual last-update time clearly; do not label stale or delayed data as live.
- [ ] **M3.2** Reconnect after interruptions, backfill missed history, and reconcile overlapping candles without losing or duplicating updates. Surface failures and provider limits.

  Tradier reconnect backfill and a controlled reconnect test are implemented; market-hour interruption testing, initial-load overlap, late trades and corrections remain open.
- [ ] **M3.3** Verify intraday and daily candles, market-session boundaries, daylight-saving changes, holidays, early closes, and extended-hours behavior with provider data and known fixtures. Investigate corrections and late reports rather than silently inventing prices.
- [ ] **M3.4** Keep Yahoo's suspicious-candle filtering visible and review its false-positive behavior. Resolve Yahoo automated-access and distribution rights, or replace it with a suitable source; do not promise complete US stock coverage until verified.
- [ ] **M3.5** Verify Tradier's entitlement-dependent stock and index behavior during market hours, including stream limits and VibesTrader's occasional quote requests. Use only market-data endpoints; do not read positions, orders, or account history.

**Done when:** Reconnect and gap recovery are tested, freshness is accurately shown, and the documented provider limitations are visible rather than hidden.

## M4 — Window and chart interaction

**Goal:** Make the whole window and everyday chart controls feel familiar and coherent.

- [ ] **M4.1** Review the full window: toolbar, chart header, right sidebar, status bar, spacing, icons, and menus. Refine the native UI for a coherent TradeCraft experience.
- [ ] **M4.2** Improve symbol search, switching intervals and chart styles, and active-state feedback. Add common intervals as supported and tested by the selected data provider.
- [ ] **M4.3** Improve crosshair, OHLC/volume readouts, zoom, pan, time and price scales, and reset-to-view behavior; verify mouse and keyboard interaction.
- [ ] **M4.4** Improve the watchlist workflow and decide how to show quotes for multiple symbols within each provider's connection and rate limits.
- [ ] **M4.5** Review accessible labels, focus, window resizing, and usability with the existing dark theme.

Provider-backed name/ticker search and watchlist quotes are implemented. Tradier shares one stream between charts and watchlists, with initial batched snapshots; Yahoo uses delayed candles. Broader search, watchlist workflow and provider-rate-limit validation remain open.

**Done when:** Common charting actions are discoverable and work consistently across supported window sizes, with the full window visually reviewed.

## M5 — Next capabilities, to prioritize together

- [ ] **M5.1** Add additional indicators based on user demand and verified formulas.
- [ ] **M5.2** Design multiple charts and named layouts, including how symbols and intervals sync and how one provider stream is shared.
- [ ] **M5.3** Evaluate options symbols, chains, and chart data with actual provider entitlements before promising coverage.
- [ ] **M5.4** Evaluate price and indicator alerts, including persistence and what happens when the app is closed.
- [ ] **M5.5** Research custom indicators and Pine Script compatibility; do not imply Pine scripts already work or promise full compatibility without a feasibility decision.
- [ ] **M5.6** Add more live-data providers based on user needs, each with its own credentials, data rights, entitlements, and limits.
- [ ] **M5.7** Revisit drawing tools after the indicator and core charting workflow is solid; extensive left-toolbar drawing tools are not the current priority.

## Cross-cutting work

- [ ] **C1 — Craft architecture:** Keep UI toolkit dependencies in the UI crate and reusable behavior in the engine. Continue the shared command interface and safety audit; do not claim full Craft compliance while documented gaps remain.
- [ ] **C2 — Dependency licenses:** Keep the existing Windows dependency-expression findings and the three added by the terminal integration open in `ARCHITECTURE.md`. Clarify the Craft allowlist interpretation and resolve them with verified compatible dependencies or an upstream decision; `cargo xtask licenses` must not be weakened to hide findings.
- [ ] **C3 — Data rights and trust:** Resolve Yahoo distribution rights and document data source limitations. Maintain the read-only charting scope; broker integration is for market data, not placing trades or accessing account data.
- [ ] **C4 — Releases:** Review the local workspace changes, use a branch and pull request for publication, and create a new community build only after the selected milestone is tested. Preserve the opt-in personal credential feature separately.

## Suggested order

Reuse upstream terminal features first. Verify the integrated indicator and persistence workflows, then address M3 data reliability and the remaining provider services. Choose M5 features based on user needs. Cross-cutting issues remain visible throughout.
