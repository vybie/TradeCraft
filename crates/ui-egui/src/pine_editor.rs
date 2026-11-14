use std::{
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::pine_engine::PineStudy;
use egui_charts::studies::Indicator;
use egui_charts::theme::ThemeExt;
use egui_charts::ui_kit::buttons::Button;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PineEntry { pub id: String, pub name: String, pub source: String }

const MAX_SOURCE_BYTES: usize = 32_000;
const EXAMPLE: &str = "//@version=6\nindicator(\"Moving average\", overlay=true)\nlength = input.int(20, \"Length\")\nplot(ta.sma(close, length))\n";

pub struct PineEditor {
    pub open: bool,
    source: String,
    saved_draft: Option<String>,
    draft_dirty: bool,
    status: Arc<Mutex<Vec<String>>>,
    message: String,
    pub(crate) library: Vec<PineEntry>,
    pending_library: Option<(Vec<PineEntry>, Option<String>)>,
    selected: Option<String>,
    library_error: Option<String>,
    delete_confirm: bool,
    pub(crate) revision: u64,
}

impl PineEditor {
    pub(crate) fn new_script(&mut self) {
        if self.draft_dirty && let Err(error) = self.flush_draft() {
            self.message = format!("Could not save the current draft: {error}");
            self.open = true;
            return;
        }
        self.selected = None;
        self.source = EXAMPLE.into();
        self.delete_confirm = false;
        self.message.clear();
        self.open = true;
    }

    pub(crate) fn edit_script(&mut self, id: &str) {
        if self.draft_dirty && let Err(error) = self.flush_draft() {
            self.message = format!("Could not save the current draft: {error}");
            self.open = true;
            return;
        }
        if let Some(entry) = self.library.iter().find(|entry| entry.id == id) {
            self.selected = Some(entry.id.clone());
            self.source = entry.source.clone();
            self.delete_confirm = false;
            self.message.clear();
            self.open = true;
        }
    }

    fn entry_label(&self, entry: &PineEntry) -> String {
        let mut matches: Vec<_> = self.library.iter().filter(|other| other.name == entry.name).collect();
        if matches.len() < 2 { return entry.name.clone(); }
        matches.sort_by(|a, b| a.id.cmp(&b.id));
        let index = matches.iter().position(|other| other.id == entry.id).unwrap_or(0) + 1;
        format!("{} ({} of {})", entry.name, index, matches.len())
    }

    pub(crate) fn confirm_delete(&mut self, id: &str) {
        if self.draft_dirty && let Err(error) = self.flush_draft() {
            self.message = format!("Could not save the current draft: {error}");
            self.open = true;
            return;
        }
        if let Some(entry) = self.library.iter().find(|entry| entry.id == id) {
            self.selected = Some(entry.id.clone());
            self.source = entry.source.clone();
            self.delete_confirm = true;
            self.open = true;
        }
    }

    pub fn new() -> Self {
        let saved_draft = script_path()
            .and_then(|path| std::fs::File::open(path).ok())
            .and_then(|file| {
                let mut text = String::new();
                file.take((MAX_SOURCE_BYTES + 1) as u64)
                    .read_to_string(&mut text)
                    .ok()?;
                (text.len() <= MAX_SOURCE_BYTES).then_some(text)
            });
        let source = saved_draft.clone().unwrap_or_else(|| EXAMPLE.into());
        let (library, library_error) = match load_library() { Ok(library) => (library, None), Err(error) => (Vec::new(), Some(error)) };
        Self {
            open: false,
            source,
            saved_draft,
            draft_dirty: false,
            status: Arc::new(Mutex::new(Vec::new())),
            message: String::new(),
            library,
            pending_library: None,
            selected: None,
            library_error,
            delete_confirm: false,
            revision: 0,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, chart: &mut egui_charts::Chart) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        let available = ctx.content_rect().size();
        let window_size = egui::vec2((available.x * 0.9).clamp(1.0, 1200.0), (available.y * 0.9).clamp(1.0, 900.0));
        let editor_height = (window_size.y - 220.0).max(60.0);
        let theme = ctx.active_theme();
        egui::Window::new("Pine editor (experimental)")
            .open(&mut open)
            .fixed_size(window_size)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .frame(egui::Frame::window(&ctx.global_style()).fill(theme.ui.panel_bg_floating).stroke(egui::Stroke::new(1.0, theme.ui.border)))
            .vscroll(true)
            .show(ctx, |ui| {
                ui.style_mut().override_font_id = Some(egui::FontId::proportional(theme.text.body));
                ui.spacing_mut().button_padding = egui::vec2(theme.spacing.md, theme.spacing.sm);
                ui.colored_label(theme.ui.text_secondary, "Experimental Pine indicators · Live-bar behavior is approximate; strategies are not supported.");
                let selected_name = self.selected.as_ref().and_then(|id| self.library.iter().find(|entry| &entry.id == id)).map_or_else(|| "Draft".to_owned(), |entry| self.entry_label(entry));
                ui.label(egui::RichText::new(selected_name).strong());
                if self.delete_confirm {
                    ui.horizontal(|ui| {
                        ui.label("Delete this saved script?");
                        if Button::new("Delete").danger().small().show(ui).clicked() { self.delete_library_entry(); }
                        if Button::new("Cancel").ghost().small().show(ui).clicked() { self.delete_confirm = false; }
                    });
                }
                if let Some(error) = &self.library_error { ui.colored_label(egui::Color32::LIGHT_RED, error); }
                ui.horizontal_wrapped(|ui| {
                if Button::new(if self.selected.is_some() { "Update script" } else { "Save to library" }).secondary().small().show(ui).clicked() { self.save_library_entry(); }
                if Button::new("Apply to chart").primary().small().show(ui).clicked() {
                    self.message.clear();
                    if let Err(error) = validate_source(&self.source) {
                        self.message = error;
                    } else {
                        match PineStudy::new(&self.source) {
                            Ok(study) if !study.diagnostics().is_empty() => {
                                self.message = study.diagnostics().iter().map(|d| format!("Line {}: {}", d.line, d.message)).collect::<Vec<_>>().join("\n");
                            }
                            Ok(study) => {
                                self.status = Arc::new(Mutex::new(Vec::new()));
                                match chart.try_add_indicator(Box::new(study.with_status(self.status.clone()))) {
                                    Ok(_) => self.message = "Added to chart. Runtime errors appear below.".into(),
                                    Err(error) => self.message = format!("Not added: {error}"),
                                }
                            }
                            Err(error) => self.message = error,
                        }
                    }
                }
                ui.colored_label(theme.ui.text_secondary, "Draft saves automatically");
                });
                if !self.message.is_empty() { ui.label(&self.message); }
                if let Ok(status) = self.status.lock() {
                    for message in status.iter() { ui.label(message); }
                }
                let source_label = ui.label("Pine script");
                let changed = egui::ScrollArea::vertical()
                    .id_salt("pine-source-scroll")
                    .max_height(editor_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| ui.add(egui::TextEdit::multiline(&mut self.source).font(egui::FontId::monospace(theme.text.body)).desired_rows(1).desired_width(f32::INFINITY)).labelled_by(source_label.id).changed())
                    .inner;
                if changed {
                    self.draft_dirty = true;
                    match self.flush_draft() {
                        Ok(()) => {},
                        Err(error) => self.message = format!("Could not save script: {error}"),
                    }
                }
            });
        self.open = open;
    }

    pub(crate) fn flush_draft(&mut self) -> Result<(), String> {
        if !self.draft_dirty { return Ok(()); }
        save_script(&self.source, self.saved_draft.as_deref())?;
        self.saved_draft = Some(self.source.clone());
        self.draft_dirty = false;
        Ok(())
    }

    pub(crate) fn flush_library(&mut self) -> Result<(), String> {
        let Some((next, selected)) = self.pending_library.as_ref() else { return Ok(()); };
        save_library(next, &self.library)?;
        self.library = next.clone();
        self.selected = selected.clone();
        self.pending_library = None;
        self.revision += 1;
        Ok(())
    }

    fn save_library_entry(&mut self) {
        if self.library_error.is_some() { return; }
        let study = match validate_source(&self.source).and_then(|()| PineStudy::new(&self.source)) {
            Ok(study) if study.diagnostics().is_empty() => study,
            Ok(study) => { self.message = study.diagnostics().iter().map(|d| format!("Line {}: {}", d.line, d.message)).collect::<Vec<_>>().join("\n"); return; }
            Err(error) => { self.message = error; return; }
        };
        let mut next = self.library.clone();
        if study.name().len() > 256 { self.message = "Script title exceeds 256 bytes.".into(); return; }
        if self.selected.is_none() && next.len() >= 128 { self.message = "The Pine library is full (128 scripts).".into(); return; }
        let id = self.selected.clone().unwrap_or_else(|| format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
        let entry = PineEntry { id: id.clone(), name: study.name().to_owned(), source: self.source.clone() };
        if let Some(existing) = next.iter_mut().find(|e| e.id == id) { *existing = entry; } else { next.push(entry); }
        self.pending_library = Some((next, Some(id)));
        match self.flush_library() {
            Ok(()) => { self.message = "Saved to Custom Pine.".into(); }
            Err(error) => self.message = format!("Could not save script: {error}"),
        }
    }

    fn delete_library_entry(&mut self) {
        if self.library_error.is_some() { return; }
        let mut next = self.library.clone();
        next.retain(|entry| Some(&entry.id) != self.selected.as_ref());
        self.pending_library = Some((next, None));
        match self.flush_library() {
            Ok(()) => { self.delete_confirm = false; self.message = "Removed from Custom Pine; charts keep their applied copies.".into(); }
            Err(error) => self.message = format!("Could not delete script: {error}"),
        }
    }
}

fn library_path() -> Result<PathBuf, String> {
    tradecraft_providers::settings::data_dir().map(|path| path.join("pine-library.json")).ok_or_else(|| "Pine script storage is unavailable".into())
}
fn load_library() -> Result<Vec<PineEntry>, String> {
    load_library_file(&library_path()?)
}
fn load_library_file(path: &std::path::Path) -> Result<Vec<PineEntry>, String> {
    let bytes = match std::fs::read(path) { Ok(bytes) => bytes, Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()), Err(e) => return Err(format!("Could not read Pine library: {e}")) };
    if bytes.len() > 4_000_000 { return Err("Pine library is too large".into()); }
    let doc: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("Could not read Pine library: {e}"))?;
    if doc["version"].as_u64() != Some(1) { return Err("Unsupported Pine library version".into()); }
    let rows = doc["scripts"].as_array().ok_or("Invalid Pine library")?;
    if rows.len() > 128 { return Err("Pine library has too many scripts".into()); }
    let mut ids = std::collections::HashSet::new();
    rows.iter().map(|v| {
        let entry = PineEntry { id: v["id"].as_str().ok_or("Invalid script id")?.to_owned(), name: v["name"].as_str().ok_or("Invalid script name")?.to_owned(), source: v["source"].as_str().ok_or("Invalid script source")?.to_owned() };
        if entry.id.is_empty() || entry.id.len() > 128 || entry.name.len() > 256 || entry.source.len() > MAX_SOURCE_BYTES || !ids.insert(entry.id.clone()) { return Err("Invalid Pine script".into()); }
        Ok(entry)
    }).collect()
}
fn save_library(entries: &[PineEntry], expected: &[PineEntry]) -> Result<(), String> {
    save_library_file(&library_path()?, entries, expected)
}
fn save_library_file(path: &std::path::Path, entries: &[PineEntry], expected: &[PineEntry]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid Pine library directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_extension("lock")).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| format!("Pine library is busy: {e}"))?;
    if load_library_file(path)? != expected { return Err("Pine library changed in another TradeCraft window; reload before saving".into()); }
    let scripts: Vec<_> = entries.iter().map(|entry| serde_json::json!({"id": entry.id, "name": entry.name, "source": entry.source})).collect();
    let document = serde_json::json!({"version": 1, "scripts": scripts}).to_string();
    if document.len() > 4_000_000 { return Err("Pine library exceeds its 4 MB limit".into()); }
    crate::terminal_storage::write_atomic(path, &document).map_err(|e| e.to_string())
}

pub(crate) fn validate_source(source: &str) -> Result<(), String> {
    use crate::pine_engine::lexer::{lex, Token};
    if source.len() > MAX_SOURCE_BYTES { return Err("Script exceeds the 32 KB limit.".into()); }
    let tokens = lex(source)?;
    if tokens.len() > 8_000 { return Err("Script exceeds the token limit.".into()); }
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.token {
            Token::Indent | Token::Op("(") | Token::Op("[") => { depth += 1; if depth > 64 { return Err("Script nesting exceeds the limit.".into()); } }
            Token::Dedent | Token::Op(")") | Token::Op("]") => depth = depth.saturating_sub(1),
            _ => {}
        }
        if let [a, dot, b] = &tokens[index..tokens.len().min(index + 3)] {
            if dot.is_op(".") && ((a.is_ident("request") && b.is_ident("security_lower_tf")) || (a.is_ident("barstate") && (b.is_ident("isrealtime") || b.is_ident("ishistory") || b.is_ident("isconfirmed")))) {
                return Err(format!("Line {}: this Pine feature is not supported yet.", a.line));
            }
        }
    }
    Ok(())
}

fn script_path() -> Option<PathBuf> {
    Some(
        tradecraft_providers::settings::data_dir()?
            .join("pine-script.pine"),
    )
}

fn save_script(source: &str, expected: Option<&str>) -> Result<(), String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err("Script exceeds the 32 KB limit.".into());
    }
    let path = script_path().ok_or("Local application data is unavailable")?;
    let directory = path.parent().ok_or("Script directory is unavailable")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_extension("lock")).map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|e| format!("Pine draft is busy: {e}"))?;
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if current.as_deref() != expected { return Err("Pine draft changed in another TradeCraft window".into()); }
    crate::terminal_storage::write_atomic(&path, source).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_charts::studies::Indicator;

    #[test]
    fn long_script_keeps_actions_on_screen() {
        for size in [egui::vec2(1280.0, 720.0), egui::vec2(500.0, 400.0)] {
            let editor = PineEditor {
                open: true,
                source: "plot(close)\n".repeat(500),
                saved_draft: None,
                draft_dirty: false,
                status: Arc::new(Mutex::new(Vec::new())),
                message: String::new(),
                library: Vec::new(),
                pending_library: None,
                selected: None,
                library_error: None,
                delete_confirm: false,
                revision: 0,
            };
            let chart = egui_charts::Chart::new(egui_charts::model::BarData::default());
            let mut harness = egui_kittest::Harness::builder()
                .with_size(size)
                .build_ui_state(|ui, state: &mut (PineEditor, egui_charts::Chart)| {
                    egui_charts::theme::apply_to_egui(ui.ctx(), &egui_charts::theme::Theme::dark());
                    state.0.show(ui.ctx(), &mut state.1);
                }, (editor, chart));
            harness.run_steps(4);
            let popup = harness.get_by_label("Pine editor (experimental)").rect();
            assert!((popup.center().x - size.x / 2.0).abs() < 2.0 && (popup.center().y - size.y / 2.0).abs() < 2.0, "popup: {popup:?} on {size:?}");
            for label in ["Save to library", "Apply to chart"] {
                let rect = harness.get_by_label(label).rect();
                assert!(rect.min.x >= 0.0 && rect.min.y >= 0.0 && rect.max.x <= size.x && rect.max.y <= size.y, "{label}: {rect:?} on {size:?}");
            }
            let before = harness.get_by_role(egui::accesskit::Role::MultilineTextInput).rect();
            harness.event(egui::Event::PointerMoved(egui::pos2(popup.center().x, before.top() + 30.0)));
            harness.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -200.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: egui::Modifiers::NONE });
            harness.run_steps(4);
            let after = harness.get_by_role(egui::accesskit::Role::MultilineTextInput).rect();
            assert!(after.top() < before.top(), "editor should scroll: {before:?} -> {after:?}");
        }
    }

    #[test]
    fn library_round_trips_multiple_scripts_and_rejects_unknown_versions() {
        let path = std::env::temp_dir().join(format!("tradecraft-pine-library-{}.json", std::process::id()));
        let entries = vec![PineEntry { id: "one".into(), name: "One".into(), source: "indicator(\"One\")\nplot(close)".into() }, PineEntry { id: "two".into(), name: "Two".into(), source: "indicator(\"Two\")\nplot(open)".into() }];
        save_library_file(&path, &entries, &[]).unwrap();
        let restored = load_library_file(&path).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[1].source, entries[1].source);
        let remaining = vec![restored[1].clone()];
        save_library_file(&path, &remaining, &restored).unwrap();
        assert_eq!(load_library_file(&path).unwrap(), remaining);
        std::fs::write(&path, "{\"version\":99,\"scripts\":[]}").unwrap();
        assert!(load_library_file(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn duplicate_scripts_open_the_requested_entry_for_confirmation() {
        let entries = vec![PineEntry { id: "1".into(), name: "Levels".into(), source: "old".into() }, PineEntry { id: "2".into(), name: "Levels".into(), source: "new".into() }];
        let mut editor = PineEditor { open: false, source: String::new(), saved_draft: None, draft_dirty: false,
            status: Arc::new(Mutex::new(Vec::new())), message: String::new(), library: entries,
            pending_library: None, selected: None, library_error: None, delete_confirm: false, revision: 0 };
        assert_eq!(editor.entry_label(&editor.library[0]), "Levels (1 of 2)");
        assert_eq!(editor.entry_label(&editor.library[1]), "Levels (2 of 2)");
        editor.confirm_delete("1");
        assert!(editor.open && editor.delete_confirm);
        assert_eq!(editor.selected.as_deref(), Some("1"));
        assert_eq!(editor.source, "old");
        assert_eq!(editor.library.len(), 2, "opening confirmation does not delete anything");
    }

    #[test]
    fn applying_the_same_script_twice_keeps_one_chart_study() {
        let editor = PineEditor { open: true, source: EXAMPLE.into(), saved_draft: None, draft_dirty: false,
            status: Arc::new(Mutex::new(Vec::new())), message: String::new(), library: Vec::new(),
            pending_library: None, selected: None, library_error: None, delete_confirm: false, revision: 0 };
        let chart = egui_charts::Chart::new(egui_charts::model::BarData::default());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(|ui, state: &mut (PineEditor, egui_charts::Chart)| { state.0.show(ui.ctx(), &mut state.1); }, (editor, chart));
        h.run_steps(3);
        h.get_by_label("Apply to chart").click();
        h.run_steps(3);
        h.get_by_label("Apply to chart").click();
        h.run_steps(3);
        assert_eq!(h.state().1.indicators().count(), 1);
        assert!(h.state().0.message.contains("already on this chart"));
    }

    #[test]
    fn duplicate_policy_distinguishes_scripts_and_charts() {
        let mut chart = egui_charts::Chart::new(egui_charts::model::BarData::default());
        let first = "indicator(\"Levels\")\nplot(close)";
        let second = "indicator(\"Levels\")\nplot(open)";
        assert!(chart.try_add_indicator(Box::new(PineStudy::new(first).unwrap())).is_ok());
        assert!(matches!(chart.try_add_indicator(Box::new(PineStudy::new(first).unwrap())), Err(egui_charts::studies::StudyError::DuplicateStudy(_))));
        assert!(chart.try_add_indicator(Box::new(PineStudy::new(second).unwrap())).is_ok());
        assert!(chart.try_add_indicator(Box::new(egui_charts::studies::SMA::new(5))).is_ok());
        assert!(chart.try_add_indicator(Box::new(egui_charts::studies::SMA::new(5))).is_ok());
        let mut other = egui_charts::Chart::new(egui_charts::model::BarData::default());
        assert!(other.try_add_indicator(Box::new(PineStudy::new(first).unwrap())).is_ok());
    }

    #[test]
    fn chart_legend_shows_values_for_offscreen_tracked_pine_plots() {
        let bar = egui_charts::model::Bar::new(chrono::DateTime::UNIX_EPOCH, 42.0, 42.0, 42.0, 42.0, 1.0);
        let mut chart = egui_charts::Chart::new(egui_charts::model::BarData::from_bars(vec![bar]));
        chart.set_timeframe(egui_charts::model::Timeframe::Min5);
        let source = "indicator(\"Legend fixture\", overlay=true)\nplot(close, color=#FFFFFF, trackprice=true, offset=-9999)\nplot(close+1, color=#8300FF, trackprice=true, offset=-9999)\nplot(close+2, color=#FF6800, trackprice=true, offset=-9999)\nplot(close+3, color=#00ECFF, trackprice=true, offset=-9999)\nplot(close+4, color=#FFFF00, trackprice=true, offset=-9999)\nplot(close+5, color=#16F000, trackprice=true, offset=-9999)\nplot(close+6, color=#FF0000, trackprice=true, offset=-9999)\nplot(na)\n";
        chart.add_indicator(Box::new(PineStudy::new(source).unwrap()));
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 760.0))), ..Default::default() };
        for _ in 0..2 {
            let output = ctx.run_ui(input.clone(), |ui| { chart.show(ui); });
            let texts: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text.galley.text().to_owned(), text.pos, text.galley.job.sections[0].format.color)), _ => None,
            }).collect();
            let (_, title, _) = texts.iter().find(|(text, _, _)| text == "Legend fixture").expect("study legend");
            for (index, rgb) in [[255,255,255],[131,0,255],[255,104,0],[0,236,255],[255,255,0],[22,240,0],[255,0,0]].into_iter().enumerate() {
                let expected = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                assert!(texts.iter().any(|(text, pos, color)| text.parse::<f64>() == Ok(42.0 + index as f64) && (pos.y - title.y).abs() < 3.0 && pos.x > title.x && *color == expected), "missing value/color {index}: {texts:?}");
            }
            assert!(texts.iter().any(|(text, pos, _)| text == "-" && (pos.y - title.y).abs() < 3.0), "{texts:?}");
            assert!(!texts.iter().any(|(text, _, _)| text.contains('∅')));
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn strategy_is_rejected_and_runtime_errors_hide_output() {
        assert!(PineStudy::new("strategy(\"example\")\nplot(close)").is_err());
        let status = Arc::new(Mutex::new(Vec::new()));
        let mut study = PineStudy::new("indicator(\"example\")\nplot(unknown_function(close))")
            .expect("parse")
            .with_status(status.clone());
        let bar = egui_charts::model::Bar::new(chrono::DateTime::UNIX_EPOCH, 1.0, 1.0, 1.0, 1.0, 1.0);
        study.calculate(&[bar]);
        assert!(study.values().columns().is_empty());
        assert!(status.lock().expect("status").iter().any(|line| line.contains("unknown_function")));
    }

    #[test]
    fn unsupported_realtime_and_lower_timeframe_are_rejected() {
        assert!(validate_source("plot(barstate.isrealtime ? 1 : 0)").is_err());
        assert!(validate_source("plot(barstate.isnew ? 1 : 0)").is_ok());
        assert!(validate_source("plot(request.security_lower_tf('SPY', '1', close))").is_err());
        assert!(validate_source("// barstate.isrealtime\nplot(close)").is_ok());
    }

    #[test]
    fn example_study_is_calculated_when_added_to_a_chart() {
        let bars: Vec<_> = (1..=25).map(|n| {
            let close = n as f64;
            egui_charts::model::Bar::new(chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(n), close, close, close, close, 1.0)
        }).collect();
        let mut chart = egui_charts::Chart::new(egui_charts::model::BarData::from_bars(bars));
        chart.add_indicator(Box::new(PineStudy::new(EXAMPLE).expect("example parses")));
        let (_, study) = chart.indicators().next().expect("chart study");
        assert_eq!(study.values().value(0, 24), Some(15.5));
    }
}
