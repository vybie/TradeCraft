//! A parsed Pine script as a chart study: its plots become lines, `fill` becomes a band and
//! `plotshape` becomes text labels.

use egui::Color32;
use egui_charts::drawings::{ChartPoint, Drawing, DrawingToolType};
use egui_charts::model::{Bar, Instrument, Timeframe};
use egui_charts::studies::{
    CrossDirection, Indicator, IndicatorMarker, IndicatorValues, InputKind, InputValue, PlotType,
    StudyData, StudyDataRequest, StudyError, StudyFill, StudyInput, StudyTable, StudyCandle,
};

use crate::pine_engine::Script;
use crate::pine_engine::ast::InputDecl;
use crate::pine_engine::runtime::{Context, DrawingKind, DrawingOutput, SecurityData, ShapeMark};
use crate::pine_engine::value::Value;

/// A Pine script that the chart calculates and draws.
#[derive(Clone)]
pub struct PineStudy {
    source: String,
    script: Script,
    context: Context,
    chart_timezone: egui_charts::model::TimezoneId,
    local_timezone: egui_charts::model::TimezoneId,
    name: String,
    overlay: bool,
    values: IndicatorValues,
    colors: Vec<Color32>,
    line_names: Vec<String>,
    plot_types: Vec<PlotType>,
    patterns: Vec<egui_charts::model::LineStyle>,
    plot_visible: Vec<bool>,
    plot_track_price: Vec<bool>,
    plot_track_price_solid: Vec<bool>,
    widths: Vec<f32>,
    offsets: Vec<isize>,
    per_bar_colors: Vec<Vec<Option<Color32>>>,
    candles: Vec<StudyCandle>,
    fills: Vec<StudyFill>,
    markers: Vec<IndicatorMarker>,
    drawings: Vec<Drawing>,
    tables: Vec<StudyTable>,
    /// Histories the last run read for `request.security` on other symbols.
    sources: Vec<SecurityData>,
    /// Other-symbol histories the script asks the chart for, as `(symbol, interval)`.
    requests: Vec<(String, Timeframe)>,
    inputs: Vec<StudyInput>,
    visible: bool,
    status: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
}

impl PineStudy {
    /// Parses `source` as a study, or describes the syntax it cannot read.
    pub fn new(source: &str) -> Result<Self, String> {
        let script = Script::parse(source)?;
        let name = script.program().title.clone();
        let overlay = script.program().overlay;
        let inputs = script.inputs().iter().map(study_input).collect();
        Ok(Self {
            source: source.to_owned(),
            script,
            context: Context::default(),
            chart_timezone: egui_charts::model::TimezoneId::utc(),
            local_timezone: iana_time_zone::get_timezone().ok()
                .and_then(|name| egui_charts::model::TimezoneId::named(&name))
                .unwrap_or_else(|| {
                    use chrono::Offset;
                    egui_charts::model::TimezoneId::fixed_offset(chrono::Local::now().offset().fix().local_minus_utc())
                        .unwrap_or_default()
                }),
            name,
            overlay,
            values: IndicatorValues::new(),
            colors: Vec::new(),
            line_names: Vec::new(),
            plot_types: Vec::new(),
            patterns: Vec::new(),
            plot_visible: Vec::new(),
            plot_track_price: Vec::new(),
            plot_track_price_solid: Vec::new(),
            widths: Vec::new(),
            offsets: Vec::new(),
            per_bar_colors: Vec::new(),
            candles: Vec::new(),
            fills: Vec::new(),
            markers: Vec::new(),
            drawings: Vec::new(),
            tables: Vec::new(),
            sources: Vec::new(),
            requests: Vec::new(),
            inputs,
            visible: true,
            status: None,
        })
    }

    /// What the last run could not do, with source lines, for the editor.
    pub fn diagnostics(&self) -> &[crate::pine_engine::ast::Diagnostic] {
        self.script.diagnostics()
    }

    pub fn with_status(mut self, status: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        self.status = Some(status);
        self
    }
}

impl Indicator for PineStudy {
    fn name(&self) -> &str {
        &self.name
    }

    fn persistence_name(&self) -> String {
        format!("tradecraft-pine-v1:{}", self.source)
    }

    fn unique_per_chart(&self) -> bool { true }

    fn calculate(&mut self, data: &[Bar]) {
        if data.len() > 10_000 {
            self.values = IndicatorValues::new();
            self.line_names.clear();
            self.fills.clear();
            self.candles.clear();
            self.drawings.clear();
            self.tables.clear();
            self.markers.clear();
            if let Some(status) = &self.status {
                if let Ok(mut status) = status.lock() {
                    *status = vec!["Pine study stopped: chart exceeds 10,000 bars.".into()];
                }
            }
            return;
        }
        let output = self.script.run(data, &self.context, &self.sources);
        self.values = IndicatorValues::new();
        if let Some(status) = &self.status {
            if let Ok(mut status) = status.lock() {
                *status = output
                    .diagnostics
                    .iter()
                    .map(|d| format!("Line {}: {}", d.line, d.message))
                    .collect();
            }
        }
        if !output.diagnostics.is_empty() {
            self.line_names.clear();
            self.fills.clear();
            self.candles.clear();
            self.drawings.clear();
            self.tables.clear();
            self.markers.clear();
            return;
        }
        let mut row = vec![f64::NAN; output.plots.len()];
        for bar in 0..data.len() {
            for (line, plot) in output.plots.iter().enumerate() {
                row[line] = plot.values.get(bar).copied().unwrap_or(f64::NAN);
            }
            self.values.push_lines(&row);
        }
        self.line_names = output.plots.iter().map(|plot| plot.title.clone()).collect();
        self.colors = output.plots.iter().map(|plot| plot.color).collect();
        self.plot_types = output.plots.iter().map(|plot| plot.plot_type).collect();
        self.patterns = output.plots.iter().map(|plot| plot.pattern).collect();
        self.plot_visible = output.plots.iter().map(|plot| plot.visible).collect();
        self.plot_track_price = output.plots.iter().map(|plot| plot.track_price).collect();
        self.plot_track_price_solid = output.plots.iter().map(|plot| plot.track_price_solid).collect();
        self.widths = output.plots.iter().map(|plot| plot.width).collect();
        self.offsets = output.plots.iter().map(|plot| plot.offset).collect();
        self.per_bar_colors = output
            .plots
            .iter()
            .map(|plot| plot.colors.clone())
            .collect();
        self.fills = output.fills;
        self.candles = output
            .candles
            .iter()
            .map(|candle| StudyCandle {
                bars: candle.bar..candle.bar + 1,
                bar: candle.candle.clone(),
                plot: candle.plot,
                color: candle.color,
                bar_style: candle.bar_style,
            })
            .collect();
        self.drawings = output.drawings.iter().map(study_drawing).collect();
        // The chart paints no study markers, so each `plotshape` mark becomes a text label with
        // the shape's glyph or the script's `text=`.
        self.drawings.extend(
            output
                .shapes
                .iter()
                .filter(|shape| !shape.text.is_empty())
                .map(study_shape),
        );
        self.tables = output.tables;
        self.requests = output.security_requests;
        self.markers = output
            .shapes
            .iter()
            .map(|shape| IndicatorMarker {
                bar: shape.bar,
                value: shape.value,
                direction: if shape.above {
                    CrossDirection::Down
                } else {
                    CrossDirection::Up
                },
            })
            .collect();
    }

    fn values(&self) -> &IndicatorValues {
        &self.values
    }

    fn data_requests(&self) -> Vec<StudyDataRequest> {
        self.requests
            .iter()
            .map(|(symbol, timeframe)| StudyDataRequest {
                symbol: symbol.clone(),
                timeframe: Some(*timeframe),
                metric: None,
            })
            .collect()
    }

    fn update_data(&mut self, data: &[Bar], _from: usize, sources: &[StudyData<'_>]) {
        self.sources = sources
            .iter()
            .map(|source| SecurityData {
                symbol: source.request.symbol.clone(),
                timeframe: source.timeframe,
                bars: source.bars.to_vec(),
            })
            .collect();
        self.calculate(data);
    }

    fn inputs(&self) -> Vec<StudyInput> {
        self.inputs.clone()
    }

    fn set_input(&mut self, id: &str, value: InputValue) -> Result<(), StudyError> {
        let Some(index) = self.inputs.iter().position(|input| input.id == id) else {
            return Err(StudyError::UnknownInput(id.into()));
        };
        if !self.inputs[index].accepts(&value) {
            return Err(StudyError::InvalidInput {
                id: id.to_owned(),
                reason: "is outside its accepted values".into(),
            });
        }
        let Some(decl) = self
            .script
            .inputs()
            .iter()
            .find(|decl| decl.id.to_string() == id)
            .cloned()
        else {
            return Err(StudyError::UnknownInput(id.into()));
        };
        self.script.set_input(decl.id, pine_value(&decl, &value));
        self.inputs[index].value = value;
        self.values.clear();
        Ok(())
    }

    fn colors(&self) -> Vec<Color32> {
        self.colors.clone()
    }

    fn set_colors(&mut self, colors: Vec<Color32>) {
        if !colors.is_empty() {
            self.colors = colors;
        }
    }

    fn plot_style(&self, line: usize) -> egui_charts::studies::PlotStyle {
        egui_charts::studies::PlotStyle {
            color: self.colors.get(line).copied().unwrap_or(Color32::WHITE),
            width: self.widths.get(line).copied().unwrap_or(1.0),
            pattern: self.patterns.get(line).copied().unwrap_or_default(),
            plot_type: self.plot_types.get(line).copied().unwrap_or_default(),
            visible: self.plot_visible.get(line).copied().unwrap_or(true),
        }
    }

    fn plot_offset(&self, line: usize) -> isize {
        self.offsets.get(line).copied().unwrap_or(0)
    }

    fn plot_price_line(&self, line: usize) -> bool {
        self.plot_track_price.get(line).copied().unwrap_or(false)
    }

    fn plot_price_line_solid(&self, line: usize) -> bool {
        self.plot_track_price_solid.get(line).copied().unwrap_or(false)
    }

    fn plot_color(&self, line: usize, bar: usize) -> Option<Color32> {
        self.per_bar_colors
            .get(line)
            .and_then(|colors| colors.get(bar))
            .copied()
            .flatten()
    }

    fn fills(&self) -> Vec<StudyFill> {
        self.fills.clone()
    }

    fn candles(&self) -> &[StudyCandle] {
        &self.candles
    }

    fn table(&self) -> Option<&StudyTable> {
        self.tables.first()
    }

    fn tables(&self) -> Vec<&StudyTable> {
        self.tables.iter().collect()
    }

    fn markers(&self) -> &[IndicatorMarker] {
        &self.markers
    }

    fn drawings(&self) -> &[Drawing] {
        &self.drawings
    }

    fn keeps_drawing_style(&self) -> bool {
        true
    }

    fn line_names(&self) -> Vec<String> {
        self.line_names.clone()
    }

    fn line_count(&self) -> usize {
        self.line_names.len()
    }

    fn is_overlay(&self) -> bool {
        self.overlay
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn set_timeframe(&mut self, timeframe: Timeframe) {
        self.context.timeframe = timeframe;
    }

    fn set_instrument(&mut self, instrument: Option<&Instrument>) -> bool {
        let symbol = instrument
            .map(|instrument| instrument.symbol.clone())
            .unwrap_or_default();
        let mintick = instrument
            .and_then(|instrument| instrument.tick_size)
            .filter(|tick| *tick > 0.0)
            .unwrap_or(0.01);
        let exchange = instrument
            .and_then(|instrument| instrument.exchange.clone())
            .unwrap_or_default();
        let root = instrument
            .and_then(|instrument| instrument.root.clone())
            .unwrap_or_else(|| symbol.clone());
        let symbol_type = instrument
            .and_then(|instrument| instrument.symbol_type)
            .map(|kind| kind.to_string())
            .unwrap_or_default();
        let changed = symbol != self.context.symbol
            || mintick != self.context.mintick
            || exchange != self.context.exchange
            || root != self.context.root
            || symbol_type != self.context.symbol_type;
        self.context.symbol = symbol;
        self.context.mintick = mintick;
        self.context.exchange = exchange;
        self.context.root = root;
        self.context.symbol_type = symbol_type;
        changed
    }

    fn set_chart_timezone(&mut self, timezone: &egui_charts::scales::TimezoneMode) -> bool {
        use egui_charts::scales::TimezoneMode;
        self.chart_timezone = match timezone {
            TimezoneMode::Utc => egui_charts::model::TimezoneId::utc(),
            TimezoneMode::Local => self.local_timezone.clone(),
            TimezoneMode::Timezone(zone) | TimezoneMode::Exchange(zone) => zone.clone(),
        };
        let effective = self.chart_timezone.clone();
        let changed = self.context.timezone != effective;
        self.context.timezone = effective;
        changed
    }

    fn clone_box(&self) -> Box<dyn Indicator> {
        Box::new(self.clone())
    }
}

/// One Pine drawing as a chart drawing: a trend line, a text label or a rectangle.
fn study_drawing(output: &DrawingOutput) -> Drawing {
    let (tool, points) = match output.kind {
        DrawingKind::Line => (
            DrawingToolType::TrendLine,
            vec![output.first, output.second],
        ),
        DrawingKind::Label => (DrawingToolType::TextLabel, vec![output.first]),
        DrawingKind::Box => (DrawingToolType::Rect, vec![output.first, output.second]),
        // A `linefill` is the polygon through both lines' ends.
        DrawingKind::Fill => {
            let mut points = vec![output.first, output.second];
            points.extend(output.quad.into_iter().flatten());
            (DrawingToolType::Polyline, points)
        }
    };
    let mut drawing = Drawing::new(0, tool);
    drawing.chart_points = points
        .into_iter()
        .map(|(bar, price)| ChartPoint::new(bar, price))
        .collect();
    drawing.completed = true;
    drawing.closed = output.kind == DrawingKind::Fill;
    drawing.color = output.color.to_srgba_unmultiplied();
    drawing.stroke_width = output.width;
    drawing.line_style = output.style;
    drawing.font_size = output.font_size;
    drawing.fill_color = output.fill_color.map(|color| color.to_srgba_unmultiplied());
    drawing.text_color = output.text_color.map(|color| color.to_srgba_unmultiplied());
    if !output.text.is_empty() {
        drawing.text = Some(output.text.clone());
    }
    drawing
}

/// A `plotshape` mark as a text label at its bar and price.
fn study_shape(mark: &ShapeMark) -> Drawing {
    let mut drawing = Drawing::new(0, DrawingToolType::TextLabel);
    drawing.chart_points = vec![ChartPoint::new(mark.bar as f32, mark.value)];
    drawing.completed = true;
    drawing.color = mark.color.to_srgba_unmultiplied();
    drawing.font_size = mark.font_size;
    drawing.text_color = Some(
        mark.text_color
            .unwrap_or(mark.color)
            .to_srgba_unmultiplied(),
    );
    drawing.text = Some(mark.text.clone());
    drawing
}

/// The library schema of one `input.*` call.
fn study_input(decl: &InputDecl) -> StudyInput {
    let id = decl.id.to_string();
    let (value, kind) = match (&decl.kind, &decl.default) {
        (crate::pine_engine::ast::InputKind::Integer { min, max }, default) => {
            let value = default.number().unwrap_or(0.0) as i32;
            let min = min.map(|min| min as i32).unwrap_or(i32::MIN);
            let max = max.map(|max| max as i32).unwrap_or(i32::MAX);
            (InputValue::Integer(value), InputKind::Integer(min, max))
        }
        (crate::pine_engine::ast::InputKind::Number { min, max }, default) => {
            let value = default.number().unwrap_or(0.0);
            let min = min.unwrap_or(-1e300);
            let max = max.unwrap_or(1e300);
            (InputValue::Number(value), InputKind::Number(min, max))
        }
        (crate::pine_engine::ast::InputKind::Toggle, default) => (
            InputValue::Toggle(default.flag().unwrap_or(false)),
            InputKind::Toggle,
        ),
        (crate::pine_engine::ast::InputKind::Color, default) => (
            InputValue::Color(default.color().unwrap_or(Color32::WHITE)),
            InputKind::Color,
        ),
        (crate::pine_engine::ast::InputKind::Timeframe, _) => {
            (InputValue::Timeframe(None), InputKind::Timeframe)
        }
        (crate::pine_engine::ast::InputKind::Symbol, _) => {
            (InputValue::Symbol(String::new()), InputKind::Symbol)
        }
        // The library has no session input, so a session is edited as its text.
        (crate::pine_engine::ast::InputKind::Session, default) => (
            InputValue::Text(default.text().unwrap_or_default().to_owned()),
            InputKind::Text,
        ),
        (crate::pine_engine::ast::InputKind::Choice(choices), default) => {
            let index = default
                .text()
                .and_then(|text| choices.iter().position(|choice| choice == text))
                .unwrap_or(0);
            (
                InputValue::Choice(index),
                InputKind::Choice(choices.clone()),
            )
        }
        (crate::pine_engine::ast::InputKind::Text, default) => (
            InputValue::Text(default.text().unwrap_or_default().to_owned()),
            InputKind::Text,
        ),
    };
    StudyInput::new(id, decl.title.clone(), value, kind)
}

/// The Pine value an edited input holds.
fn pine_value(decl: &InputDecl, value: &InputValue) -> Value {
    match value {
        InputValue::Length(length) => Value::Num(*length as f64),
        InputValue::Integer(integer) => Value::Num(f64::from(*integer)),
        InputValue::Number(number) => Value::Num(*number),
        InputValue::Toggle(flag) => Value::Bool(*flag),
        InputValue::Color(color) => Value::Color(*color),
        InputValue::Choice(index) => match &decl.kind {
            crate::pine_engine::ast::InputKind::Choice(choices) => choices
                .get(*index)
                .map(|choice| Value::Str(choice.clone()))
                .unwrap_or(Value::Na),
            _ => Value::Num(*index as f64),
        },
        InputValue::Timeframe(timeframe) => timeframe
            .map(|timeframe| Value::Str(timeframe.as_str().into_owned()))
            .unwrap_or(Value::Na),
        InputValue::Symbol(symbol) => Value::Str(symbol.clone()),
        InputValue::Text(text) => Value::Str(text.clone()),
        _ => Value::Na,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    #[test]
    fn session_levels_ignore_after_hours_and_show_prior_close() {
        use chrono::TimeZone;
        let script = crate::pine_engine::Script::parse(include_str!("../../../../scripts/pine/bb-levels.pine")).unwrap();
        let bars: Vec<_> = [(2,13,0,130.0,95.0),(2,14,30,140.0,90.0),(2,21,0,200.0,80.0),
            (3,5,0,120.0,110.0),(3,14,0,119.0,111.0),(3,14,30,150.0,100.0),
            (3,15,0,145.0,101.0),(3,21,0,180.0,50.0)]
            .into_iter().map(|(d,h,m,hi,lo)| Bar::new(chrono::Utc.with_ymd_and_hms(2024,1,d,h,m,0).unwrap(), lo, hi, lo, hi, 1.0)).collect();
        let daily = SecurityData { symbol: "TEST".into(), timeframe: Timeframe::Day1,
            bars: vec![Bar::new(chrono::Utc.with_ymd_and_hms(2024,1,2,14,30,0).unwrap(), 100.0,140.0,90.0,115.0,1.0)] };
        // The chart is set to Pacific time while the script's market sessions use New York.
        let context = Context { symbol: "TEST".into(), timezone: "America/Los_Angeles".parse().unwrap(), ..Context::default() };
        let output = script.run(&bars, &context, std::slice::from_ref(&daily));
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let latest: Vec<_> = output.plots.iter().map(|plot| *plot.values.last().unwrap()).collect();
        assert_eq!(latest, [150.0, 100.0, 140.0, 90.0, 115.0, 120.0, 110.0]);
        assert_eq!(output.security_requests, [("TEST".into(), Timeframe::Day1)]);
        assert!(output.plots[4].track_price && output.plots[4].track_price_solid);
        assert_eq!(output.plots[4].color, Color32::YELLOW);
        assert_eq!(output.drawings.len(), 7);
        let midnight = script.run(&bars[..4], &context, std::slice::from_ref(&daily));
        assert_eq!(midnight.plots[2].values.last().copied(), Some(140.0));
        assert_eq!(midnight.plots[4].values.last().copied(), Some(115.0));
        assert_eq!(midnight.plots[5].values.last().copied(), Some(120.0));
        let mut study = PineStudy::new(include_str!("../../../../scripts/pine/bb-levels.pine")).unwrap();
        study.set_instrument(Some(&Instrument::new("TEST")));
        study.set_chart_timezone(&egui_charts::scales::TimezoneMode::Timezone("America/Los_Angeles".parse().unwrap()));
        study.calculate(&bars);
        let requests = study.data_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].symbol, "TEST");
        assert_eq!(requests[0].timeframe, Some(Timeframe::Day1));
        assert!(study.values().value(4, bars.len() - 1).is_none());
        study.update_data(&bars, 0, &[StudyData { request: requests[0].clone(), bars: &daily.bars,
            clock: egui_charts::model::BarClock::new(Timeframe::Day1), instrument: None, timeframe: Timeframe::Day1 }]);
        assert_eq!(study.values().value(4, bars.len() - 1), Some(115.0));
        assert!(study.plot_price_line_solid(4));
    }

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(at, close)| {
                Bar::new(
                    DateTime::UNIX_EPOCH + chrono::Duration::minutes(at as i64),
                    *close,
                    close + 1.0,
                    close - 1.0,
                    *close,
                    1.0,
                )
            })
            .collect()
    }

    const AVERAGE_CHANNEL: &str = "//@version=6\nindicator(\"Average channel fixture\", overlay=true)\nshortLength = input.int(3, \"Short\")\nlongLength = input.int(5, \"Long\")\nshortLine = plot(ta.ema(close, shortLength), \"Short\")\nlongLine = plot(ta.ema(close, longLength), \"Long\")\nfill(shortLine, longLine, color=color.new(color.blue, 80))\n";

    #[test]
    fn averages_plot_lines_a_fill_and_editable_inputs() {
        let mut study = PineStudy::new(AVERAGE_CHANNEL).unwrap();
        assert!(study.is_overlay());
        assert_eq!(study.name(), "Average channel fixture");
        let data = bars(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        study.calculate(&data);
        assert_eq!(study.line_count(), 2);
        assert_eq!(study.line_names(), ["Short", "Long"]);
        assert!(study.values().value(0, 0).is_none());
        assert!(study.values().value(1, 4).is_some());
        assert_eq!(study.fills().len(), 1);
        assert_eq!(study.inputs().len(), 2);
        let fast = study.inputs()[0].id.clone();
        study
            .set_input(&fast, InputValue::Integer(4))
            .expect("the input accepts a length");
        study.calculate(&data);
        assert!(study.values().value(0, 3).is_some());
        assert_eq!(study.inputs()[0].default, InputValue::Integer(3));
    }

    #[test]
    fn a_wrong_input_type_is_rejected() {
        let mut study = PineStudy::new(AVERAGE_CHANNEL).unwrap();
        let fast = study.inputs()[0].id.clone();
        assert!(
            study
                .set_input(&fast, InputValue::Text("x".into()))
                .is_err()
        );
    }

    #[test]
    fn display_none_reaches_the_plot_style() {
        let mut study =
            PineStudy::new("indicator(\"T\")\nplot(close, display=display.none)\n").unwrap();
        study.calculate(&bars(&[1.0]));
        assert!(!study.plot_style(0).visible);
    }

    #[test]
    fn plotcandle_reaches_the_chart_candles() {
        let mut study = PineStudy::new(
            "indicator(\"T\", overlay=true)\nplotcandle(open, high, low, close, color=color.green)\n",
        )
        .unwrap();
        let chart = bars(&[1.0, 2.0]);
        study.calculate(&chart);
        let candles = study.candles();
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[1].bars, 1..2);
        assert_eq!(candles[1].bar.high, 3.0);
        assert_eq!(candles[1].color, Some(Color32::GREEN));
        assert_eq!(candles[1].plot, 0);
        assert!(study.plot_style(0).visible);
    }

    #[test]
    fn another_symbol_becomes_a_study_data_request() {
        let mut study =
            PineStudy::new("indicator(\"T\")\nplot(request.security(\"BTCUSDT\", \"5\", close))\n")
                .unwrap();
        assert!(study.data_requests().is_empty());
        study.calculate(&bars(&[1.0, 2.0]));
        let requests = study.data_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].symbol, "BTCUSDT");
        assert_eq!(requests[0].timeframe, Some(Timeframe::Min5));
    }

    #[test]
    fn another_symbol_history_reaches_the_plot() {
        let mut study =
            PineStudy::new("indicator(\"T\")\nplot(request.security(\"OTHER\", \"2\", close))\n")
                .unwrap();
        let chart = bars(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        study.calculate(&chart);
        let request = study.data_requests().pop().expect("another symbol");
        assert_eq!(
            request.timeframe,
            Some(Timeframe::Custom(120.try_into().unwrap()))
        );
        let source: Vec<Bar> = [100.0, 200.0, 300.0, 400.0, 500.0]
            .iter()
            .enumerate()
            .map(|(at, close)| {
                Bar::new(
                    DateTime::UNIX_EPOCH + chrono::Duration::minutes(at as i64 * 2),
                    *close,
                    *close,
                    *close,
                    *close,
                    1.0,
                )
            })
            .collect();
        let timeframe = Timeframe::Custom(120.try_into().unwrap());
        let data = StudyData {
            request,
            bars: &source,
            clock: egui_charts::model::BarClock::new(timeframe),
            instrument: None,
            timeframe,
        };
        study.update_data(&chart, 0, &[data]);
        assert!(study.values().value(0, 0).is_none());
        assert!(study.values().value(0, 1).is_none());
        assert_eq!(study.values().value(0, 2), Some(100.0));
        assert_eq!(study.values().value(0, 3), Some(100.0));
        assert_eq!(study.values().value(0, 4), Some(200.0));
    }

    #[test]
    fn plotchar_text_is_drawn_as_a_label() {
        let mut study = PineStudy::new(
            "indicator(\"T\", overlay=true)\nplotchar(close > 0, text=\"Mon\", location=location.abovebar)\n",
        )
        .unwrap();
        study.calculate(&bars(&[10.0]));
        let drawings = study.drawings();
        assert_eq!(drawings.len(), 1);
        assert_eq!(drawings[0].text.as_deref(), Some("Mon"));
        assert_eq!(drawings[0].chart_points[0].price, 11.0);
    }

    #[test]
    fn syminfo_reads_the_instrument() {
        let mut study = PineStudy::new(
            "indicator(\"T\")\nplot(syminfo.type == \"crypto\" ? 1 : 0)\nplot(str.length(syminfo.prefix))\nplot(str.length(syminfo.root))\n",
        )
        .unwrap();
        let mut instrument = Instrument::new("BTCUSDT");
        instrument.exchange = Some("BINANCE".into());
        instrument.root = Some("BTC".into());
        instrument.symbol_type = Some(egui_charts::model::SymbolType::Crypto);
        assert!(study.set_instrument(Some(&instrument)));
        study.calculate(&bars(&[1.0]));
        assert_eq!(study.values().value(0, 0), Some(1.0));
        assert_eq!(study.values().value(1, 0), Some(7.0));
        assert_eq!(study.values().value(2, 0), Some(3.0));
    }

    #[test]
    fn pine_uses_the_selected_chart_timezone_for_clock_and_daily_buckets() {
        use chrono::TimeZone;
        use egui_charts::scales::TimezoneMode;
        let mut study = PineStudy::new("//@version=4\nstudy(\"Timezone fixture\")\nplot(hour)\n").unwrap();
        let data: Vec<_> = [(2, 23, 10.0), (3, 0, 20.0), (3, 14, 30.0)]
            .into_iter().map(|(day, hour, high)| {
                Bar::new(chrono::Utc.with_ymd_and_hms(2024, 1, day, hour, 0, 0).unwrap(), high, high, high, high, 1.0)
            }).collect();
        let ny = TimezoneMode::Timezone("America/New_York".parse().unwrap());
        assert!(study.set_chart_timezone(&ny));
        study.calculate(&data);
        assert_eq!(study.values().value(0, 2), Some(9.0));
        // Instrument metadata does not override the user's selected chart zone.
        study.set_instrument(Some(&Instrument::new("TEST").with_time_zone("Europe/London".parse().unwrap())));
        assert!(!study.set_chart_timezone(&ny));
        assert!(study.set_chart_timezone(&TimezoneMode::Utc));
        study.calculate(&data);
        assert_eq!(study.values().value(0, 2), Some(14.0));
        study.set_chart_timezone(&TimezoneMode::Local);
        assert_eq!(study.context.timezone, study.local_timezone);
    }

    #[test]
    fn a_pine_line_becomes_a_chart_drawing() {
        let mut study = PineStudy::new(
            "//@version=6\nindicator(\"L\", overlay=true)\nif barstate.islast\n    line.new(0, low, bar_index, high, color=color.red, width=2)\n",
        )
        .unwrap();
        let data = bars(&[1.0, 2.0, 3.0]);
        study.calculate(&data);
        let drawings = study.drawings();
        assert_eq!(drawings.len(), 1);
        assert_eq!(drawings[0].chart_points.len(), 2);
        assert_eq!(drawings[0].chart_points[0].bar_idx, 0.0);
        assert_eq!(drawings[0].chart_points[1].bar_idx, 2.0);
    }

    #[test]
    fn plotshape_becomes_markers() {
        let mut study = PineStudy::new(
            "//@version=6\nindicator(\"Rising bar marker\", overlay=true)\nrising = high > high[1]\nplotshape(rising, location=location.abovebar, color=color.white)\n",
        )
        .unwrap();
        let data = bars(&[1.0, 2.0, 5.0, 2.0, 1.0]);
        study.calculate(&data);
        assert!(!study.markers().is_empty());
    }
}
