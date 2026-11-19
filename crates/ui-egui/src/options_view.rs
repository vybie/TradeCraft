use egui_charts::scales::{CurrencyFormatter, PriceFormatter};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tradecraft_providers::{
    options::Contract,
    reference::MarketQuote,
    settings::{Provider, Settings},
};

const ROW_HEIGHT: f32 = 44.0;

enum ResultMessage {
    Expirations(Result<Vec<String>, String>),
    Chain(Result<(Vec<Contract>, Option<MarketQuote>), String>),
}

pub struct OptionsView {
    pub open: bool,
    symbol: String,
    expirations: Vec<String>,
    expiration: String,
    contracts: Vec<Contract>,
    underlying: Option<MarketQuote>,
    show_puts: bool,
    scroll_to_price: bool,
    receiver: Option<Receiver<ResultMessage>>,
    loading: bool,
    error: String,
    fetched: Option<chrono::DateTime<chrono::Local>>,
    columns: Vec<String>,
    columns_error: String,
    live: Option<Arc<Mutex<crate::options_stream::State>>>,
    live_revision: Option<u64>,
    live_reconnects: u64,
    stream_status: String,
    request_settings: Option<Settings>,
    retry_at: Option<Instant>,
}

#[derive(Clone, Copy)]
enum ColumnFormat {
    Text,
    Price,
    Integer,
    Greek,
    Percent,
    Volatility,
    Timestamp,
}

struct Column {
    key: &'static str,
    title: &'static str,
    format: ColumnFormat,
    required: bool,
    default: bool,
}

const COLUMNS: &[Column] = &[
    Column {
        key: "strike",
        title: "Strike",
        format: ColumnFormat::Price,
        required: true,
        default: true,
    },
    Column {
        key: "volume",
        title: "Volume",
        format: ColumnFormat::Integer,
        required: false,
        default: true,
    },
    Column {
        key: "greeks.delta",
        title: "Delta",
        format: ColumnFormat::Greek,
        required: false,
        default: true,
    },
    Column {
        key: "change_percentage",
        title: "% change",
        format: ColumnFormat::Percent,
        required: true,
        default: true,
    },
    Column {
        key: "change",
        title: "Change",
        format: ColumnFormat::Price,
        required: true,
        default: true,
    },
    Column {
        key: "bid",
        title: "Bid",
        format: ColumnFormat::Price,
        required: true,
        default: true,
    },
    Column {
        key: "ask",
        title: "Ask",
        format: ColumnFormat::Price,
        required: true,
        default: true,
    },
    Column {
        key: "greeks.mid_iv",
        title: "IV %",
        format: ColumnFormat::Volatility,
        required: false,
        default: true,
    },
    Column {
        key: "last",
        title: "Last · chart",
        format: ColumnFormat::Price,
        required: false,
        default: true,
    },
    Column {
        key: "open_interest",
        title: "Open interest",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.gamma",
        title: "Gamma",
        format: ColumnFormat::Greek,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.theta",
        title: "Theta",
        format: ColumnFormat::Greek,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.vega",
        title: "Vega",
        format: ColumnFormat::Greek,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.rho",
        title: "Rho",
        format: ColumnFormat::Greek,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.phi",
        title: "Phi",
        format: ColumnFormat::Greek,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.bid_iv",
        title: "Bid IV %",
        format: ColumnFormat::Volatility,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.ask_iv",
        title: "Ask IV %",
        format: ColumnFormat::Volatility,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.smv_vol",
        title: "SMV volatility %",
        format: ColumnFormat::Volatility,
        required: false,
        default: false,
    },
    Column {
        key: "bidsize",
        title: "Bid size",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "asksize",
        title: "Ask size",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "open",
        title: "Open",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "high",
        title: "High",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "low",
        title: "Low",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "close",
        title: "Close",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "prevclose",
        title: "Previous close",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "average_volume",
        title: "Average volume",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "last_volume",
        title: "Last trade size",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "week_52_high",
        title: "52-week high",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "week_52_low",
        title: "52-week low",
        format: ColumnFormat::Price,
        required: false,
        default: false,
    },
    Column {
        key: "contract_size",
        title: "Contract size",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "lot_size",
        title: "Lot size",
        format: ColumnFormat::Integer,
        required: false,
        default: false,
    },
    Column {
        key: "symbol",
        title: "Contract symbol",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "description",
        title: "Description",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "root_symbol",
        title: "Option root",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "underlying",
        title: "Underlying",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "option_type",
        title: "Type",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "expiration_date",
        title: "Expiration",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "expiration_type",
        title: "Expiration type",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "exch",
        title: "Exchange",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "bidexch",
        title: "Bid exchange",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "askexch",
        title: "Ask exchange",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
    Column {
        key: "trade_date",
        title: "Last trade time",
        format: ColumnFormat::Timestamp,
        required: false,
        default: false,
    },
    Column {
        key: "bid_date",
        title: "Bid time",
        format: ColumnFormat::Timestamp,
        required: false,
        default: false,
    },
    Column {
        key: "ask_date",
        title: "Ask time",
        format: ColumnFormat::Timestamp,
        required: false,
        default: false,
    },
    Column {
        key: "greeks.updated_at",
        title: "Greeks updated",
        format: ColumnFormat::Text,
        required: false,
        default: false,
    },
];

fn default_columns() -> Vec<String> {
    COLUMNS
        .iter()
        .filter(|c| !c.required && c.default)
        .map(|c| c.key.to_owned())
        .collect()
}

fn columns_path() -> Option<std::path::PathBuf> {
    let name = if cfg!(feature = "vibestrader-credential") {
        "options-columns-personal.json"
    } else {
        "options-columns-community.json"
    };
    Some(tradecraft_providers::settings::data_dir()?.join(name))
}

fn load_columns() -> Vec<String> {
    columns_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|data| serde_json::from_slice::<Vec<String>>(&data).ok())
        .unwrap_or_else(default_columns)
}

fn save_columns(columns: &[String]) -> Result<(), String> {
    let path = columns_path().ok_or("Options settings directory unavailable")?;
    std::fs::create_dir_all(path.parent().ok_or("Invalid options settings path")?)
        .map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|e| format!("Options settings are busy: {e}"))?;
    let json = serde_json::to_string(columns).map_err(|e| e.to_string())?;
    crate::terminal_storage::write_atomic(&path, &json).map_err(|e| e.to_string())
}

impl Default for OptionsView {
    fn default() -> Self {
        Self {
            open: false,
            symbol: String::new(),
            expirations: Vec::new(),
            expiration: String::new(),
            contracts: Vec::new(),
            underlying: None,
            show_puts: false,
            scroll_to_price: false,
            receiver: None,
            loading: false,
            error: String::new(),
            fetched: None,
            columns: load_columns(),
            columns_error: String::new(),
            live: None,
            live_revision: None,
            live_reconnects: 0,
            stream_status: String::new(),
            request_settings: None,
            retry_at: None,
        }
    }
}

fn value(value: Option<f64>, decimals: usize) -> String {
    value
        .map(|v| format!("{v:.decimals$}"))
        .unwrap_or_else(|| "—".into())
}

fn percent(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.2}%"))
        .unwrap_or_else(|| "—".into())
}

fn dollars(number: f64) -> String {
    CurrencyFormatter::usd().format(number)
}

fn contract_price(number: Option<f64>) -> String {
    number
        .filter(|n| n.is_finite())
        .map(|n| format!("{} ({})", dollars(n), dollars(n * 100.0)))
        .unwrap_or_else(|| "—".into())
}

fn column_value(contract: &Contract, column: &Column) -> String {
    let raw = column
        .key
        .split('.')
        .fold(&contract.fields, |v, key| &v[key]);
    let number = match column.key {
        "strike" => Some(contract.strike),
        "volume" => contract.volume,
        "greeks.delta" => contract.delta,
        "change_percentage" => contract.change_percentage,
        "change" => contract.change,
        "bid" => contract.bid,
        "ask" => contract.ask,
        "greeks.mid_iv" => contract.iv,
        "last" => contract.last,
        "open_interest" => contract.open_interest,
        "greeks.gamma" => contract.gamma,
        "greeks.theta" => contract.theta,
        "greeks.vega" => contract.vega,
        _ => raw.as_f64().filter(|v| v.is_finite()),
    };
    match column.format {
        ColumnFormat::Text => raw
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("—")
            .to_owned(),
        ColumnFormat::Price if matches!(column.key, "bid" | "ask" | "last") => {
            contract_price(number)
        }
        ColumnFormat::Price if column.key == "change" => {
            number.map(dollars).unwrap_or_else(|| "—".into())
        }
        ColumnFormat::Price => value(number, 2),
        ColumnFormat::Integer => number
            .map(|n| CurrencyFormatter::new("", 0).format(n))
            .unwrap_or_else(|| "—".into()),
        ColumnFormat::Greek => value(number, 4),
        ColumnFormat::Percent => percent(number),
        ColumnFormat::Volatility => percent(number.map(|n| n * 100.0)),
        ColumnFormat::Timestamp => raw
            .as_i64()
            .and_then(|n| {
                if n > 10_000_000_000 {
                    chrono::DateTime::from_timestamp_millis(n)
                } else {
                    chrono::DateTime::from_timestamp(n, 0)
                }
            })
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%b %-d %I:%M %p")
                    .to_string()
            })
            .unwrap_or_else(|| "—".into()),
    }
}

fn column_min_width(column: &Column) -> f32 {
    match column.key {
        "symbol" | "description" => 210.0,
        "bid" | "ask" | "last" => 160.0,
        "trade_date" | "bid_date" | "ask_date" | "greeks.updated_at" => 170.0,
        _ => 110.0,
    }
}

fn marker_index(rows: &[&Contract], price: f64) -> usize {
    rows.iter()
        .position(|contract| contract.strike <= price)
        .unwrap_or(rows.len())
}

fn underlying_marker(ui: &mut egui::Ui, price: f64, width: f32) {
    let text = format!("Underlying: ${price:.2}");
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), &text));
    let painter = ui.painter();
    let accent = ui.visuals().selection.bg_fill;
    let accent_text = ui.visuals().selection.stroke.color;
    painter.hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, accent),
    );
    let font = egui::FontId::proportional(14.0);
    let text_size = painter
        .layout_no_wrap(text.clone(), font.clone(), accent_text)
        .size();
    // Anchor the badge near Strike so it remains visible before horizontally scrolling.
    let badge = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 12.0, rect.center().y - 13.0),
        egui::vec2(text_size.x + 24.0, 26.0),
    );
    painter.rect_filled(badge, egui::CornerRadius::same(13), accent);
    painter.text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        text,
        font,
        accent_text,
    );
}

fn row_separator(ui: &mut egui::Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    let color = ui
        .visuals()
        .window_fill
        .lerp_to_gamma(ui.visuals().text_color(), 0.18);
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, color),
    );
}

impl OptionsView {
    pub fn open_for(&mut self, symbol: &str, settings: Settings) {
        self.open = true;
        self.symbol = symbol.to_owned();
        self.expirations.clear();
        self.expiration.clear();
        self.contracts.clear();
        self.underlying = None;
        self.show_puts = false;
        self.scroll_to_price = false;
        self.fetched = None;
        self.error.clear();
        self.receiver = None;
        self.live = None;
        self.live_revision = None;
        self.live_reconnects = 0;
        self.stream_status.clear();
        self.retry_at = None;
        self.request_settings = Some(settings);
        if settings.provider != Provider::Tradier {
            self.loading = false;
            self.error = "Select Tradier in Market data settings to view options.".into();
        } else {
            self.request(settings, false);
        }
    }

    fn request(&mut self, settings: Settings, chain: bool) {
        let symbol = self.symbol.clone();
        let expiration = self.expiration.clone();
        self.loading = true;
        self.error.clear();
        self.retry_at = None;
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let token = tradecraft_engine::tradier_token(settings.use_vibestrader)
                    .map_err(str::to_owned)?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "Could not start options request".to_owned())?;
                let client = tradecraft_providers::reference::client()?;
                Ok(if chain {
                    ResultMessage::Chain(runtime.block_on(async {
                        let contracts = tradecraft_providers::options::chain(
                            &client,
                            &token,
                            &symbol,
                            &expiration,
                        )
                        .await?;
                        let quote = tradecraft_providers::reference::tradier_quotes(
                            &client,
                            &token,
                            std::slice::from_ref(&symbol),
                        )
                        .await
                        .ok()
                        .and_then(|quotes| {
                            quotes
                                .into_iter()
                                .find(|q| q.symbol.eq_ignore_ascii_case(&symbol))
                        });
                        Ok((contracts, quote))
                    }))
                } else {
                    ResultMessage::Expirations(runtime.block_on(
                        tradecraft_providers::options::expirations(&client, &token, &symbol),
                    ))
                })
            })();
            let _ = tx.send(result.unwrap_or_else(|e| {
                if chain {
                    ResultMessage::Chain(Err(e))
                } else {
                    ResultMessage::Expirations(Err(e))
                }
            }));
        });
    }

    #[cfg(test)]
    pub fn show(&mut self, ctx: &egui::Context, settings: Settings) -> Option<String> {
        self.show_with_feed(ctx, settings, None)
    }

    pub fn show_with_feed(&mut self, ctx: &egui::Context, settings: Settings, feed: Option<&crate::terminal_feed::Feed>) -> Option<String> {
        if !self.open {
            self.live = None;
            self.receiver = None;
            self.loading = false;
            return None;
        }
        if self.request_settings.is_some_and(|previous| previous != settings) {
            self.open_for(&self.symbol.clone(), settings);
        }
        if let Some(rx) = &self.receiver {
            match rx.try_recv() {
                Ok(message) => {
                    self.receiver = None;
                    self.loading = false;
                    match message {
                        ResultMessage::Expirations(Ok(dates)) => {
                            self.expirations = dates;
                            if let Some(date) = self.expirations.first() {
                                self.expiration = date.clone();
                                self.request(settings, true);
                            } else {
                                self.error =
                                    "No listed expirations found for this instrument.".into();
                            }
                        }
                        ResultMessage::Chain(Ok((contracts, quote))) => {
                            self.scroll_to_price = self.fetched.is_none();
                            let old_symbols: std::collections::BTreeSet<_> = self.contracts.iter().map(|contract| &contract.symbol).collect();
                            let new_symbols: std::collections::BTreeSet<_> = contracts.iter().map(|contract| &contract.symbol).collect();
                            if old_symbols != new_symbols {
                                self.live = None;
                                self.live_reconnects = 0;
                            }
                            self.contracts = contracts;
                            self.underlying = quote;
                            self.fetched = Some(chrono::Local::now());
                            self.live_revision = None;
                        }
                        ResultMessage::Expirations(Err(e)) | ResultMessage::Chain(Err(e)) => {
                            self.error = e;
                            self.retry_at = Some(Instant::now() + Duration::from_secs(30));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.receiver = None;
                    self.loading = false;
                    self.error = "Options request stopped unexpectedly.".into();
                    self.retry_at = Some(Instant::now() + Duration::from_secs(30));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.open {
            return None;
        }
        if self.live.is_none() && !self.contracts.is_empty() && settings.provider == Provider::Tradier
            && let Some(feed) = feed {
            let mut symbols: Vec<_> = self.contracts.iter().map(|contract| contract.symbol.clone()).collect();
            symbols.push(self.symbol.clone());
            self.live = feed.options_stream(symbols, ctx.clone());
        }
        if let Some(live) = self.live.clone() && let Ok(state) = live.lock() {
            self.stream_status = state.message.clone();
            if self.live_revision != Some(state.revision) {
                for contract in &mut self.contracts {
                    if let Some(prices) = state.prices.get(&contract.symbol) { prices.apply(contract); }
                }
                if let Some(quote) = &mut self.underlying
                    && let Some(trade) = state.prices.get(&self.symbol).and_then(|prices| prices.last_trade())
                    && let Some(time) = chrono::DateTime::from_timestamp_millis(trade.timestamp_ms)
                    && time >= quote.time {
                    quote.last = trade.price;
                    quote.time = time;
                }
                self.live_revision = Some(state.revision);
            }
            if state.connected && state.reconnects > self.live_reconnects && !self.loading {
                let reconnect = self.live_reconnects > 0;
                self.live_reconnects = state.reconnects;
                if reconnect { self.request(settings, true); }
            }
        }
        if !self.loading && settings.provider == Provider::Tradier
            && self.retry_at.is_some_and(|deadline| Instant::now() >= deadline) {
            self.request(settings, !self.expiration.is_empty());
        }
        if self.loading { ctx.request_repaint_after(Duration::from_millis(100)); }
        if let Some(deadline) = self.retry_at { ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now())); }
        let mut open = self.open;
        let mut picked = None;
        let screen = ctx.content_rect().size();
        let width = (screen.x * 0.92).min(1500.0);
        let height = (screen.y * 0.88).min(1000.0);
        egui::Window::new(format!("Options · {}", self.symbol))
            .open(&mut open).default_size(egui::vec2(width, height)).resizable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let surface = ui.visuals().window_fill.lerp_to_gamma(ui.visuals().text_color(), 0.055);
                egui::Frame::new().fill(surface).corner_radius(10).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(egui::RichText::new("UNDERLYING").small().weak().strong());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(&self.symbol).size(22.0).strong());
                        if let Some(quote) = &self.underlying {
                            let change = quote.last - quote.previous_close;
                            let percent = (quote.previous_close != 0.0).then(|| change / quote.previous_close * 100.0);
                            ui.label(egui::RichText::new(format!("${:.2}", quote.last)).size(22.0).strong());
                            if let Some(percent) = percent {
                                ui.label(egui::RichText::new(format!("{change:+.2}  ({percent:+.2}%)")).strong());
                            }
                        }
                    });
                    if let Some(quote) = &self.underlying {
                        ui.label(egui::RichText::new(format!("Last trade · {}", quote.time.with_timezone(&chrono::Local).format("%b %-d, %I:%M:%S %p"))).small().weak());
                    } else {
                        ui.label(egui::RichText::new("Underlying quote unavailable").small().weak());
                    }
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.heading("Option chain");
                    let count = self.contracts.iter().filter(|c| c.kind == if self.show_puts { "put" } else { "call" }).count();
                    if self.fetched.is_some() { ui.label(egui::RichText::new(format!("{count} contracts")).small().weak()); }
                    ui.label(egui::RichText::new("Select a strike or last price to open its chart").small().weak());
                });
                ui.horizontal_wrapped(|ui| {
                    for (label, puts) in [("Calls", false), ("Puts", true)] {
                        let selected = self.show_puts == puts;
                        let button = egui::Button::selectable(selected, egui::RichText::new(label).strong()).min_size(egui::vec2(74.0, 34.0));
                        if ui.add(button).clicked() && !selected { self.show_puts = puts; self.scroll_to_price = true; }
                    }
                    ui.add_space(6.0);
                    let previous = self.expiration.clone();
                    let expiration_label = ui.label("Expiration");
                    egui::ComboBox::from_id_salt("options_expiration").width(128.0).selected_text(if self.expiration.is_empty() { "Unavailable".into() } else { self.expiration.clone() }).show_ui(ui, |ui| {
                        for date in &self.expirations { ui.selectable_value(&mut self.expiration, date.clone(), date); }
                    }).response.labelled_by(expiration_label.id);
                    if previous != self.expiration {
                        self.live = None;
                        self.live_revision = None;
                        self.live_reconnects = 0;
                        self.stream_status.clear();
                        self.contracts.clear();
                        self.underlying = None;
                        self.fetched = None;
                        self.request(settings, true);
                    }
                    if ui.add_enabled(!self.loading && settings.provider == Provider::Tradier, egui::Button::new("Refresh").min_size(egui::vec2(68.0, 34.0))).on_hover_text("Refresh the options snapshot and Greeks").clicked() { self.request(settings, !self.expiration.is_empty()); }
                    let mut columns_changed = false;
                    ui.menu_button("Columns", |ui| {
                        ui.strong("Options columns");
                        ui.label("Required columns are always shown.");
                        egui::ScrollArea::vertical().id_salt("column_picker").max_height(420.0).show(ui, |ui| {
                            for column in COLUMNS {
                                let mut visible = column.required || self.columns.iter().any(|key| key == column.key);
                                if ui.add_enabled(!column.required, egui::Checkbox::new(&mut visible, column.title)).changed() {
                                    if visible { self.columns.push(column.key.to_owned()); } else { self.columns.retain(|key| key != column.key); }
                                    columns_changed = true;
                                }
                            }
                        });
                    });
                    if columns_changed {
                        self.columns_error = save_columns(&self.columns).err().map(|e| format!("Could not save columns: {e}")).unwrap_or_default();
                    }
                });
                if !self.columns_error.is_empty() { ui.colored_label(ui.visuals().error_fg_color, &self.columns_error); }
                if self.loading { ui.label(if self.fetched.is_some() { "Refreshing options…" } else { "Loading options…" }); }
                if !self.stream_status.is_empty() { ui.label(egui::RichText::new(&self.stream_status).small()); }
                if !self.error.is_empty() {
                    egui::Frame::new().fill(ui.visuals().error_fg_color.gamma_multiply(0.12)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                        ui.colored_label(ui.visuals().error_fg_color, &self.error);
                    });
                }
                if let Some(time) = self.fetched {
                    ui.label(egui::RichText::new(format!("Snapshot {} · Greeks are snapshot values · Prices may be delayed", time.format("%b %-d, %I:%M:%S %p"))).small().weak());
                    if self.contracts.is_empty() { ui.label("No contracts returned for this expiration."); }
                }
                ui.collapsing("About prices and charts", |ui| {
                    ui.label("Bid, ask and last prices update from the market stream when available. The snapshot time is not the time of each quote.");
                    ui.label("Prices in parentheses are multiplied by 100 before fees; adjusted contracts may differ. Select a strike or last price to open its chart.");
                });
                let kind = if self.show_puts { "put" } else { "call" };
                let mut rows: Vec<_> = self.contracts.iter().filter(|c| c.kind == kind).collect();
                rows.sort_by(|a, b| b.strike.total_cmp(&a.strike).then_with(|| a.symbol.cmp(&b.symbol)));
                if rows.is_empty() && !self.contracts.is_empty() && self.fetched.is_some() {
                    ui.label(format!("No {} available for this expiration.", if self.show_puts { "puts" } else { "calls" }));
                }
                let nearest = self.underlying.as_ref().and_then(|quote| rows.iter().enumerate().min_by(|(_, a), (_, b)| (a.strike - quote.last).abs().total_cmp(&(b.strike - quote.last).abs())).map(|(i, _)| i));
                let marker = self.underlying.as_ref().filter(|_| !rows.is_empty()).map(|quote| marker_index(&rows, quote.last));
                let scroll = self.scroll_to_price;
                self.scroll_to_price = false;
                let compact = ui.available_width() < 900.0;
                let mut visible: Vec<_> = COLUMNS.iter().filter(|column| column.required || self.columns.iter().any(|key| key == column.key)).collect();
                if compact {
                    // Keep the trading-critical columns in view first; all other selected
                    // columns remain available by scrolling horizontally.
                    visible.sort_by_key(|column| match column.key { "strike" => 0, "bid" => 1, "ask" => 2, "last" => 3, _ => 4 });
                    if visible.len() > 4 { ui.label(egui::RichText::new("Scroll horizontally for more columns").small().weak()); }
                }
                egui::ScrollArea::horizontal().id_salt("options_horizontal").show(ui, |ui| {
                    let spacing = ui.spacing().item_spacing.x;
                    let count = visible.len();
                    let minimums: Vec<f32> = visible.iter().map(|column| {
                        let minimum = if compact && matches!(column.key, "bid" | "ask" | "last") { 110.0 } else { column_min_width(column) };
                        if matches!(column.key, "bid" | "ask" | "last") {
                            let widest = rows.iter().filter_map(|c| match column.key { "bid" => c.bid, "ask" => c.ask, _ => c.last }).filter(|v| v.is_finite()).max_by(|a, b| a.abs().total_cmp(&b.abs()));
                            let text = contract_price(widest);
                            let size = if column.key == "last" { egui::FontId::proportional(14.0) } else { egui::FontId::proportional(15.0) };
                            minimum.max(ui.painter().layout_no_wrap(text, size, ui.visuals().text_color()).size().x + 24.0)
                        } else { minimum }
                    }).collect();
                    let extra = ((ui.available_width() - spacing * (count.saturating_sub(1) as f32) - 24.0 - minimums.iter().sum::<f32>()) / count as f32).max(0.0);
                    let mut widths: Vec<f32> = minimums.iter().map(|minimum| minimum + extra).collect();
                    if compact && visible.len() > 4 && visible.get(3).is_some_and(|c| c.key == "last") {
                        let first_columns = widths.iter().take(4).sum::<f32>() + spacing * 4.0;
                        widths[3] += (ui.available_width() - first_columns + 1.0).max(0.0);
                    }
                    let table_width = widths.iter().sum::<f32>() + spacing * count.saturating_sub(1) as f32;
                    let header_color = ui.visuals().window_fill.lerp_to_gamma(ui.visuals().text_color(), 0.055);
                    egui::Frame::new().fill(header_color).corner_radius(6).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (column, width) in visible.iter().zip(&widths) {
                                ui.add_sized([*width, 36.0], egui::Label::new(egui::RichText::new(column.title).size(14.0).strong()));
                            }
                        });
                    });
                    row_separator(ui, table_width);
                    egui::ScrollArea::vertical().id_salt("options_rows").auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (index, c) in rows.iter().enumerate() {
                            if marker == Some(index) {
                                if let Some(quote) = &self.underlying { underlying_marker(ui, quote.last, table_width); }
                            }
                            let row = ui.horizontal(|ui| {
                                for (column, width) in visible.iter().zip(&widths) {
                                    if column.key == "strike" || column.key == "last" {
                                        let text = if column.key == "strike" { dollars(c.strike) } else if c.last.is_some() { contract_price(c.last) } else { "View chart".into() };
                                        let color = if column.key == "last" { ui.visuals().hyperlink_color } else { ui.visuals().text_color() };
                                        let mut rich = egui::RichText::new(text).size(14.0).color(color).strong();
                                        if column.key == "last" { rich = rich.underline(); }
                                        let button = egui::Button::new(rich).frame(false);
                                        if ui.add_sized([*width, ROW_HEIGHT], button).on_hover_text(format!("Open chart for {}", c.symbol)).clicked() { picked = Some(c.symbol.clone()); }
                                    } else {
                                        let text = column_value(c, column);
                                        ui.add_sized([*width, ROW_HEIGHT], egui::Label::new(egui::RichText::new(text).size(15.0)));
                                    }
                                }
                            });
                            if scroll && nearest == Some(index) { row.response.scroll_to_me(Some(egui::Align::Center)); }
                            row_separator(ui, table_width);
                        }
                        if marker == Some(rows.len()) {
                            if let Some(quote) = &self.underlying { underlying_marker(ui, quote.last, table_width); }
                        }
                    });
                });
            });
        self.open = open;
        if !open { self.live = None; }
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn contract(symbol: &str, kind: &str, strike: f64, last: f64) -> Contract {
        Contract {
            fields: serde_json::Value::Null,
            symbol: symbol.into(),
            kind: kind.into(),
            strike,
            last: Some(last),
            bid: None,
            ask: None,
            change: None,
            change_percentage: None,
            volume: None,
            open_interest: None,
            iv: None,
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
        }
    }

    fn view() -> OptionsView {
        OptionsView {
            open: true,
            symbol: "SPY".into(),
            expiration: "2027-01-15".into(),
            expirations: vec!["2027-01-15".into()],
            contracts: vec![
                contract("SPY_CALL", "call", 500.0, 3.06),
                contract("SPY_PUT", "put", 500.0, 4.28),
            ],
            fetched: Some(chrono::Local::now()),
            columns: default_columns(),
            ..Default::default()
        }
    }

    #[test]
    fn live_quote_and_trade_events_update_the_displayed_chain() {
        let (watcher, live) = crate::options_stream::Watcher::new(vec!["SPY_CALL".into()], egui::Context::default());
        let mut view = view();
        view.live = Some(live);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0))
            .build_ui_state(|ui, view: &mut OptionsView| { view.show(ui.ctx(), Settings::default()); }, view);
        h.run_steps(3);
        watcher.push(&tradecraft_providers::shared_stream::StreamEvent::Trade {
            symbol: "SPY_CALL".into(),
            trade: tradecraft_core::Trade { timestamp_ms: 1751376600000, price: 3.5, size: 2.0 }, volume: Some(100.0),
        });
        watcher.push(&tradecraft_providers::shared_stream::StreamEvent::Quote {
            symbol: "SPY_CALL".into(),
            bid: Some(tradecraft_providers::shared_stream::QuoteSide { price: 3.4, size: Some(5.0), timestamp_ms: 1751376600000, exchange: None }),
            ask: Some(tradecraft_providers::shared_stream::QuoteSide { price: 3.6, size: Some(6.0), timestamp_ms: 1751376600000, exchange: None }),
        });
        h.run_steps(3);
        for label in ["$3.50 ($350.00)", "$3.40 ($340.00)", "$3.60 ($360.00)"] {
            assert!(h.query_by_label(label).is_some(), "missing live price {label}");
        }
        h.state_mut().open = false;
        h.run_steps(2);
        assert!(watcher.state.upgrade().is_none(), "closing the chain must end its subscription");
    }

    #[test]
    fn chain_switches_contract_type_and_opens_selected_contract() {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_ui_state(
                |ui, state: &mut (OptionsView, Option<String>)| {
                    state.1 = state
                        .0
                        .show(ui.ctx(), Settings::default())
                        .or_else(|| state.1.take());
                },
                (view(), None),
            );
        h.run_steps(3);
        assert!(h.query_by_label("$3.06 ($306.00)").is_some());
        assert!(h.query_by_label("$4.28 ($428.00)").is_none());
        h.get_by_label("Puts").click();
        h.run_steps(3);
        h.get_by_label("$4.28 ($428.00)").click();
        h.run_steps(2);
        assert_eq!(h.state().1.as_deref(), Some("SPY_PUT"));
    }

    #[test]
    fn percentages_and_underlying_marker_use_displayed_units_and_strike_order() {
        assert_eq!(percent(Some(-58.65)), "-58.65%");
        assert_eq!(percent(None), "—");
        let high = contract("HIGH", "call", 750.0, 1.0);
        let low = contract("LOW", "call", 749.0, 1.0);
        let rows = vec![&high, &low];
        assert_eq!(marker_index(&rows, 749.60), 1);
        assert_eq!(marker_index(&rows, 760.0), 0);
        assert_eq!(marker_index(&rows, 740.0), 2);
    }

    #[test]
    fn required_columns_remain_visible_and_optional_fields_are_formatted() {
        let selected: Vec<String> = vec!["open_interest".into(), "greeks.bid_iv".into()];
        let shown: Vec<_> = COLUMNS
            .iter()
            .filter(|c| c.required || selected.iter().any(|key| key == c.key))
            .map(|c| c.key)
            .collect();
        for key in ["strike", "change_percentage", "change", "bid", "ask"] {
            assert!(shown.contains(&key));
        }
        assert!(!shown.contains(&"volume"));
        let mut c = contract("SPY_CALL", "call", 500.0, 3.06);
        c.open_interest = Some(8249.0);
        c.fields = serde_json::json!({"greeks": {"bid_iv": 0.125}});
        assert_eq!(
            column_value(
                &c,
                COLUMNS.iter().find(|c| c.key == "open_interest").unwrap()
            ),
            "8,249"
        );
        assert_eq!(
            column_value(
                &c,
                COLUMNS.iter().find(|c| c.key == "greeks.bid_iv").unwrap()
            ),
            "12.50%"
        );
    }

    #[test]
    fn formats_volume_change_and_contract_prices() {
        let mut c = contract("SPY_CALL", "call", 500.0, 3.06);
        c.volume = Some(42112.0);
        c.bid = Some(1.23);
        c.ask = Some(1.25);
        c.change = Some(-2.75);
        let field =
            |key| column_value(&c, COLUMNS.iter().find(|column| column.key == key).unwrap());
        assert_eq!(field("volume"), "42,112");
        assert_eq!(field("bid"), "$1.23 ($123.00)");
        assert_eq!(field("ask"), "$1.25 ($125.00)");
        assert_eq!(field("last"), "$3.06 ($306.00)");
        assert_eq!(field("change"), "-$2.75");
        assert_eq!(contract_price(None), "—");
    }

    #[test]
    fn column_picker_exposes_optional_parameters() {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_ui_state(
                |ui, view: &mut OptionsView| {
                    view.show(ui.ctx(), Settings::default());
                },
                view(),
            );
        h.run_steps(3);
        h.get_by_label("Columns").click();
        h.run_steps(3);
        assert!(
            h.query_by_label("Required columns are always shown.")
                .is_some()
        );
        assert!(h.query_by_label("Open interest").is_some());
        assert!(h.query_by_label("Gamma").is_some());
    }

    #[test]
    fn narrow_and_wide_views_keep_controls_and_errors_accessible() {
        let settings = Settings { provider: Provider::Tradier, use_vibestrader: false };
        for width in [640.0, 1440.0] {
            let mut view = view();
            view.stream_status = "Stream disconnected".into();
            view.error = "Could not refresh options".into();
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(width, 900.0))
                .build_ui_state(|ui, view: &mut OptionsView| { view.show(ui.ctx(), settings); }, view);
            h.run_steps(3);
            for label in ["Calls", "Puts", "Expiration", "Refresh", "Columns", "Stream disconnected", "Could not refresh options", "About prices and charts"] {
                let rect = h.get_by_label(label).rect();
                assert!(rect.left() >= 0.0 && rect.right() <= width, "{label} clipped at width {width}: {rect:?}");
            }
            if width == 640.0 {
                for label in ["Strike", "Bid", "Ask", "Last · chart"] {
                    let rect = h.get_by_label(label).rect();
                    assert!(rect.right() <= width, "important column {label} is off-screen: {rect:?}");
                }
            }
            h.get_by_label("About prices and charts").click();
            h.run_steps(2);
            assert!(h.query_by_label_contains("adjusted contracts may differ").is_some());
            assert!(h.query_by_label("$3.06 ($306.00)").is_some(), "last snapshot should remain visible during an error");
        }
    }

    #[test]
    #[ignore = "headless visual inspection"]
    fn render_options_view() {
        let mut preview = view();
        preview.underlying = Some(MarketQuote {
            symbol: "SPY".into(),
            last: 499.60,
            previous_close: 500.0,
            time: chrono::Utc::now(),
        });
        preview.scroll_to_price = false;
        preview.contracts[0].change_percentage = Some(-58.65);
        preview.contracts[0].iv = Some(0.24);
        preview.contracts[0].bid = Some(3.04);
        preview.contracts[0].ask = Some(3.08);
        preview.contracts[0].change = Some(-2.75);
        preview.contracts[0].volume = Some(42112.0);
        for strike in 496..=504 {
            preview.contracts.push(contract(
                &format!("SPY{strike}C"),
                "call",
                strike as f64,
                3.06,
            ));
        }
        preview.stream_status = "Live option quotes · receiving market events".into();
        for (name, width, dark) in [("dark", 1440.0, true), ("light", 1440.0, false), ("narrow", 640.0, true)] {
            let mut sample = view();
            sample.underlying = preview.underlying.clone();
            sample.contracts = preview.contracts.clone();
            sample.stream_status = preview.stream_status.clone();
            sample.scroll_to_price = false;
            let mut h = egui_kittest::Harness::builder()
                .allow_missing_glyphs().with_size(egui::vec2(width, 900.0)).wgpu()
                .build_ui_state(move |ui, view: &mut OptionsView| {
                    let theme = if dark { egui_charts::theme::Theme::dark() } else { egui_charts::theme::Theme::light() };
                    egui_charts::theme::apply_to_egui(ui.ctx(), &theme);
                    view.show(ui.ctx(), Settings { provider: Provider::Tradier, use_vibestrader: false });
                }, sample);
            h.run_steps(3);
            h.render().unwrap().save(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../target/options-preview-{name}.png"))).unwrap();
        }
    }
}
