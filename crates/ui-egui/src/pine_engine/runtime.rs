//! Evaluates a parsed script bar by bar: series and `var` state, user functions, `if` values,
//! `plot`/`fill`/`plotshape`, and the stateful builtins.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use chrono::{Datelike, TimeZone, Timelike};
use egui::Color32;
use egui_charts::model::Bar;
use egui_charts::studies::{
    FillBoundary, PlotType, StudyCell, StudyFill, StudyTable, StudyTablePosition,
};

use crate::pine_engine::ast::{
    Arg, Diagnostic, Expr, Function, NodeId, Program, Stmt, TypeDecl,
};
use crate::pine_engine::builtins::{self, TaState};
use crate::pine_engine::parser::literal;
use crate::pine_engine::value::{self, Value};

/// How many times one `for`/`while` may run before it is stopped, so a runaway script cannot
/// hang the chart.
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// How many loop steps one run may take in total, so a script that loops over everything cannot
/// hang the chart.
const MAX_RUN_STEPS: usize = 2_000_000;

/// What a running loop is doing: `break`/`continue` set this and the loop reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Control {
    #[default]
    None,
    Break,
    Continue,
}

/// The chart facts a script may read: `syminfo.*` and `timeframe.*`.
#[derive(Clone, Debug)]
pub struct Context {
    pub symbol: String,
    pub timeframe: egui_charts::model::Timeframe,
    /// The instrument's price step, which `syminfo.mintick` and `math.round_to_mintick` use.
    pub mintick: f64,
    /// The venue of the chart symbol, which `syminfo.prefix` reads.
    pub exchange: String,
    /// The contract root, which `syminfo.root` reads.
    pub root: String,
    /// The Pine name of the instrument type, which `syminfo.type` reads.
    pub symbol_type: String,
    /// Exchange time zone for Pine calendar fields and session filters.
    pub timezone: egui_charts::model::TimezoneId,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            timeframe: egui_charts::model::Timeframe::Min1,
            mintick: 0.01,
            exchange: String::new(),
            root: String::new(),
            symbol_type: String::new(),
            timezone: egui_charts::model::TimezoneId::utc(),
        }
    }
}

/// A history the host fetched for a `request.security` on another symbol.
#[derive(Clone, Debug)]
pub struct SecurityData {
    pub symbol: String,
    pub timeframe: egui_charts::model::Timeframe,
    pub bars: Vec<Bar>,
}

/// One `plot` output: a full column and its presentation.
#[derive(Clone, Debug)]
pub struct PlotSeries {
    pub title: String,
    pub color: Color32,
    pub width: f32,
    pub plot_type: PlotType,
    /// The dash pattern of `linestyle=`.
    pub pattern: egui_charts::model::LineStyle,
    /// Whether `display=` lets the chart draw the line.
    pub visible: bool,
    pub track_price: bool,
    pub track_price_solid: bool,
    pub offset: isize,
    pub values: Vec<f64>,
    pub colors: Vec<Option<Color32>>,
}

/// One `plotcandle`/`plotbar` candle on a bar.
#[derive(Clone, Debug)]
pub struct PlotCandle {
    /// The loaded bar it sits on.
    pub bar: usize,
    /// The candle's open, high, low, close and the source bar's time.
    pub candle: Bar,
    /// Output whose colour, width and visibility apply to this candle.
    pub plot: usize,
    /// A colour for just this bar, as `color=` sets.
    pub color: Option<Color32>,
    /// Whether this is a `plotbar` OHLC bar rather than a candle.
    pub bar_style: bool,
}

/// One `plotshape` mark on a bar.
#[derive(Clone, Debug)]
pub struct ShapeMark {
    pub bar: usize,
    pub value: f64,
    pub above: bool,
    pub color: Color32,
    /// The glyph or label the study paints, since the library marker carries no text.
    pub text: String,
    pub text_color: Option<Color32>,
    pub font_size: f32,
}

/// Everything one run of the script produced.
#[derive(Clone, Debug, Default)]
pub struct RunOutput {
    pub plots: Vec<PlotSeries>,
    pub candles: Vec<PlotCandle>,
    pub shapes: Vec<ShapeMark>,
    pub fills: Vec<StudyFill>,
    pub drawings: Vec<DrawingOutput>,
    pub tables: Vec<StudyTable>,
    /// Other-symbol histories the script read, as `(symbol, interval)`.
    pub security_requests: Vec<(String, egui_charts::model::Timeframe)>,
    pub diagnostics: Vec<Diagnostic>,
}

/// What a Pine `line`, `label`, `box` or `linefill` draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawingKind {
    Line,
    Label,
    Box,
    /// The area between two lines, held as a polygon of the two lines' ends.
    Fill,
}

/// One `line.new`, `label.new`, `box.new` or `linefill.new` that was not deleted by the end of
/// the run, in bar coordinates so the chart can place it.
#[derive(Clone, Debug)]
pub struct DrawingOutput {
    pub kind: DrawingKind,
    pub first: (f32, f64),
    pub second: (f32, f64),
    /// The other two corners of a `linefill`: the second line's ends.
    pub quad: Option<[(f32, f64); 2]>,
    pub text: String,
    pub color: Color32,
    pub text_color: Option<Color32>,
    pub fill_color: Option<Color32>,
    pub width: f32,
    pub style: egui_charts::model::LineStyle,
    pub font_size: f32,
}

/// A script variable: its value on the current bar and the finished bars behind it.
#[derive(Clone, Debug)]
struct Var {
    value: Value,
    history: Vec<Value>,
}

#[derive(Clone, Debug)]
struct PlotSlot {
    series: PlotSeries,
}

#[derive(Clone, Debug)]
struct FillSlot {
    first: usize,
    second: usize,
    color: Color32,
}

#[derive(Clone, Debug)]
struct ShapeSlot {
    above: bool,
    color: Color32,
    text: String,
    text_color: Option<Color32>,
    font_size: f32,
    offset: isize,
    marks: Vec<(usize, f64)>,
}

/// The higher-timeframe series one `request.security` call site builds from the chart bars, plus
/// the state its expression keeps between those bars.
#[derive(Debug)]
struct SecuritySlot {
    bars: Vec<Bar>,
    values: Vec<Value>,
    bucket: Bucket,
    /// The current bucket key on the chart, or the source history's first bar time when the
    /// series comes from another symbol, so a reload starts it over.
    key: Option<i64>,
    ta: HashMap<NodeId, TaState>,
}

/// Runs one script over a bar series.
pub struct Evaluator<'a> {
    bars: Cow<'a, [Bar]>,
    program: &'a Program,
    context: &'a Context,
    inputs: &'a HashMap<NodeId, Value>,
    vars: HashMap<String, Var>,
    locals: Vec<HashMap<String, Value>>,
    functions: HashMap<&'a str, &'a Function>,
    types: HashMap<&'a str, &'a TypeDecl>,
    ta: HashMap<NodeId, TaState>,
    plots: Vec<PlotSlot>,
    plot_at: HashMap<NodeId, usize>,
    candles: Vec<PlotCandle>,
    fills: Vec<FillSlot>,
    fill_at: HashMap<NodeId, usize>,
    shapes: Vec<ShapeSlot>,
    shape_at: HashMap<NodeId, usize>,
    security: HashMap<NodeId, SecuritySlot>,
    /// Histories for `request.security` calls on other symbols, supplied by the host.
    sources: &'a [SecurityData],
    /// Every other-symbol `request.security` this run asked for, with its interval.
    security_requests: Vec<(String, egui_charts::model::Timeframe)>,
    diagnostics: Vec<Diagnostic>,
    reported: HashSet<(usize, String)>,
    depth: usize,
    bar: usize,
    control: Control,
    steps: usize,
    out_of_budget: bool,
    /// Every `line`/`label`/`box` this run created, in creation order, including deleted ones.
    drawings: Vec<value::ObjectRef>,
    /// Every `table.new` object this run created.
    tables: Vec<value::ObjectRef>,
}

impl<'a> Evaluator<'a> {
    pub fn new(
        program: &'a Program,
        bars: &'a [Bar],
        context: &'a Context,
        inputs: &'a HashMap<NodeId, Value>,
        sources: &'a [SecurityData],
    ) -> Self {
        let mut functions = HashMap::new();
        let mut types = HashMap::new();
        for statement in &program.body {
            match statement {
                Stmt::Function(function) => {
                    functions.insert(function.name.as_str(), function);
                }
                Stmt::Type(decl) => {
                    types.insert(decl.name.as_str(), decl);
                }
                _ => {}
            }
        }
        Self {
            bars: Cow::Borrowed(bars),
            program,
            context,
            inputs,
            vars: HashMap::new(),
            locals: Vec::new(),
            functions,
            types,
            ta: HashMap::new(),
            plots: Vec::new(),
            plot_at: HashMap::new(),
            candles: Vec::new(),
            fills: Vec::new(),
            fill_at: HashMap::new(),
            shapes: Vec::new(),
            shape_at: HashMap::new(),
            security: HashMap::new(),
            sources,
            security_requests: Vec::new(),
            diagnostics: Vec::new(),
            reported: HashSet::new(),
            depth: 0,
            bar: 0,
            control: Control::None,
            steps: 0,
            out_of_budget: false,
            drawings: Vec::new(),
            tables: Vec::new(),
        }
    }

    /// Runs every bar, in order, as a chart's studies are calculated.
    pub fn run(mut self) -> RunOutput {
        let body = self.program.body.as_slice();
        for bar in 0..self.bars.len() {
            self.bar = bar;
            self.run_block(body);
            self.finish_bar();
            if self.out_of_budget { break; }
        }
        let fills = self
            .fills
            .iter()
            .map(|fill| StudyFill {
                first: FillBoundary::Plot(fill.first),
                second: FillBoundary::Plot(fill.second),
                color: fill.color,
                inverted_color: None,
            })
            .collect();
        let shapes = self
            .shapes
            .iter()
            .flat_map(|shape| {
                shape.marks.iter().map(move |&(bar, value)| ShapeMark {
                    bar,
                    value,
                    above: shape.above,
                    color: shape.color,
                    text: shape.text.clone(),
                    text_color: shape.text_color,
                    font_size: shape.font_size,
                })
            })
            .collect();
        let drawings = self
            .drawings
            .iter()
            .filter_map(drawing_output)
            .collect();
        let tables = self.build_tables();
        RunOutput {
            plots: self.plots.into_iter().map(|slot| slot.series).collect(),
            candles: self.candles,
            shapes,
            fills,
            drawings,
            tables,
            security_requests: self.security_requests,
            diagnostics: self.diagnostics,
        }
    }

    /// The Pine tables to show, the one with the most filled cells first, each with its first row
    /// read as the headings of the rest.
    fn build_tables(&self) -> Vec<StudyTable> {
        let mut built: Vec<(usize, StudyTable)> = Vec::new();
        for object in &self.tables {
            let fields = value::lock_object(object);
            if fields.get("__deleted").and_then(Value::flag) == Some(true) {
                continue;
            }
            let columns = fields
                .get("__columns")
                .and_then(Value::number)
                .unwrap_or(0.0)
                .max(0.0) as usize;
            let rows = fields
                .get("__rows")
                .and_then(Value::number)
                .unwrap_or(0.0)
                .max(0.0) as usize;
            if columns == 0 || rows == 0 {
                continue;
            }
            let text = |column: usize, row: usize| {
                fields
                    .get(&format!("c:{column}:{row}"))
                    .and_then(|value| value.text().map(str::to_owned))
                    .unwrap_or_default()
            };
            let color = |prefix: &str, column: usize, row: usize| {
                fields
                    .get(&format!("{prefix}:{column}:{row}"))
                    .and_then(Value::color)
            };
            let mut cells: Vec<Vec<StudyCell>> = (0..rows)
                .map(|row| {
                    (0..columns)
                        .map(|column| StudyCell::Text(text(column, row)))
                        .collect()
                })
                .collect();
            let mut cell_bg: Vec<Vec<Option<Color32>>> = (0..rows)
                .map(|row| (0..columns).map(|column| color("b", column, row)).collect())
                .collect();
            let mut cell_text: Vec<Vec<Option<Color32>>> = (0..rows)
                .map(|row| (0..columns).map(|column| color("t", column, row)).collect())
                .collect();
            let filled = cells
                .iter()
                .flatten()
                .filter(|cell| matches!(cell, StudyCell::Text(text) if !text.is_empty()))
                .count();
            if filled == 0 {
                continue;
            }
            let merges: Vec<(usize, usize, usize, usize)> = fields
                .keys()
                .filter_map(|key| {
                    let mut parts = key
                        .strip_prefix("m:")?
                        .split(':')
                        .map(|part| part.parse::<usize>().ok());
                    Some((
                        parts.next()??,
                        parts.next()??,
                        parts.next()??,
                        parts.next()??,
                    ))
                })
                .collect();
            let headings = if columns > 1 && cells.len() > 1 {
                let first = cells.remove(0);
                cell_bg.remove(0);
                cell_text.remove(0);
                first
                    .into_iter()
                    .map(|cell| match cell {
                        StudyCell::Text(text) => text,
                        StudyCell::Percent(_) => String::new(),
                    })
                    .collect()
            } else {
                vec![String::new(); columns]
            };
            // `table.merge_cells` spans rows of the script's table; the heading row taken above
            // shifts them, and the range then takes its start cell's colours.
            let shift = rows - cells.len();
            for (start_column, start_row, end_column, end_row) in merges {
                let (Some(start_row), Some(end_row)) = (
                    start_row.checked_sub(shift),
                    end_row.checked_sub(shift),
                ) else {
                    continue;
                };
                let Some(bg) = cell_bg
                    .get(start_row)
                    .and_then(|row| row.get(start_column))
                    .copied()
                else {
                    continue;
                };
                let text_colour = cell_text
                    .get(start_row)
                    .and_then(|row| row.get(start_column))
                    .copied()
                    .flatten();
                for row in start_row..=end_row.min(cells.len().saturating_sub(1)) {
                    for column in start_column..=end_column.min(columns.saturating_sub(1)) {
                        if let Some(slot) =
                            cell_bg.get_mut(row).and_then(|row| row.get_mut(column))
                        {
                            *slot = bg;
                        }
                        if let Some(slot) =
                            cell_text.get_mut(row).and_then(|row| row.get_mut(column))
                        {
                            *slot = text_colour;
                        }
                    }
                }
            }
            let table = StudyTable {
                title: String::new(),
                columns: headings,
                rows: cells,
                cell_bg,
                cell_text,
                position: table_position(fields.get("__position").and_then(Value::text)),
            };
            built.push((filled, table));
        }
        built.sort_by_key(|(filled, _)| std::cmp::Reverse(*filled));
        built.into_iter().map(|(_, table)| table).collect()
    }

    fn finish_bar(&mut self) {
        for var in self.vars.values_mut() {
            // A bar that does not run an assignment keeps the previous value, as Pine does.
            var.history.push(var.value.clone());
        }
    }

    fn run_block(&mut self, block: &[Stmt]) -> Value {
        let mut value = Value::Na;
        for statement in block {
            value = self.run_stmt(statement);
            if self.control != Control::None || self.out_of_budget {
                break;
            }
        }
        value
    }

    /// Writes a variable where Pine would: `:=` updates the scope that declared it, `=` declares
    /// in the current one, which is the script's own scope unless a loop or function is running.
    fn store(&mut self, name: &str, value: Value, reassign: bool) {
        if reassign {
            for scope in self.locals.iter_mut().rev() {
                if scope.contains_key(name) {
                    scope.insert(name.to_owned(), value);
                    return;
                }
            }
            if let Some(var) = self.vars.get_mut(name) {
                var.value = value;
                return;
            }
        }
        if self.locals.is_empty() {
            let var = self.vars.entry(name.to_owned()).or_insert_with(|| Var {
                value: Value::Na,
                history: Vec::new(),
            });
            var.value = value;
        } else if let Some(scope) = self.locals.last_mut() {
            scope.insert(name.to_owned(), value);
        }
    }

    /// Whether the script declared `name` as a variable in the current or an outer scope.
    fn is_declared(&self, name: &str) -> bool {
        self.vars.contains_key(name) || self.locals.iter().any(|scope| scope.contains_key(name))
    }

    fn run_stmt(&mut self, statement: &Stmt) -> Value {
        match statement {
            Stmt::Assign {
                name,
                value,
                sticky,
                reassign,
            } => {
                // `var x = …` runs on the first bar only; a missing one is `na` until assigned.
                if *sticky && self.bar > 0 {
                    return self.ident(name);
                }
                let value = self.eval(value);
                self.store(name, value.clone(), *reassign);
                value
            }
            Stmt::Tuple {
                names,
                value,
                sticky,
            } => {
                if *sticky && self.bar > 0 {
                    return Value::Na;
                }
                let items = self.eval(value).array();
                for (at, name) in names.iter().enumerate() {
                    let item = items
                        .as_ref()
                        .and_then(|items| value::lock(items).get(at).cloned())
                        .unwrap_or(Value::Na);
                    self.store(name, item, false);
                }
                Value::Na
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = self.eval(cond).flag().unwrap_or(false);
                if cond {
                    self.run_block(then)
                } else if let Some(otherwise) = otherwise {
                    self.run_block(otherwise)
                } else {
                    Value::Na
                }
            }
            Stmt::For {
                line,
                var,
                from,
                to,
                step,
                body,
            } => {
                let from = self.eval(from).number();
                let to = self.eval(to).number();
                let step = match step {
                    Some(step) => self.eval(step).number(),
                    None => Some(1.0),
                }
                // Pine counts down when `from` is above `to`; `for i = 5 to 0` walks 5..0 and the
                // step keeps its size.
                .map(|step| match (from, to) {
                    (Some(from), Some(to)) if from > to => -step.abs(),
                    _ => step.abs(),
                });
                let (Some(from), Some(to), Some(step)) = (from, to, step) else {
                    return Value::Na;
                };
                if step == 0.0 {
                    return Value::Na;
                }
                let mut at = from;
                let mut value = Value::Na;
                let mut iterations = 0;
                loop {
                    let running = if step > 0.0 { at <= to } else { at >= to };
                    if !running {
                        break;
                    }
                    if iterations >= MAX_LOOP_ITERATIONS {
                        self.loop_limit(*line);
                        break;
                    }
                    iterations += 1;
                    if self.charge(*line) {
                        break;
                    }
                    self.locals
                        .push(HashMap::from([(var.clone(), Value::Num(at))]));
                    value = self.run_block(body);
                    self.locals.pop();
                    if std::mem::replace(&mut self.control, Control::None) == Control::Break {
                        break;
                    }
                    at += step;
                }
                value
            }
            Stmt::ForIn {
                line,
                vars,
                iterable,
                body,
            } => {
                // A snapshot of the entries, so the body may push to the same array or map.
                let evaluated = self.eval(iterable);
                let from_map = evaluated.map().is_some();
                let rows: Vec<(Value, Value)> = if let Some(entries) = evaluated.map() {
                    // `for [key, value] in someMap` walks the entries in insertion order.
                    value::lock_map(&entries).entries.clone()
                } else if let Some(items) = evaluated.array() {
                    value::lock(&items)
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(index, item)| (Value::Num(index as f64), item))
                        .collect()
                } else {
                    return Value::Na;
                };
                let mut value = Value::Na;
                let mut iterations = 0;
                for (key, item) in rows {
                    if iterations >= MAX_LOOP_ITERATIONS {
                        self.loop_limit(*line);
                        break;
                    }
                    iterations += 1;
                    if self.charge(*line) {
                        break;
                    }
                    let mut scope = HashMap::new();
                    match vars.as_slice() {
                        [] => {}
                        [var] => {
                            // A map yields its keys when the loop names one variable.
                            scope.insert(
                                var.clone(),
                                if from_map { key } else { item },
                            );
                        }
                        // `for [index, value] in array`: the position and the element.
                        [key_var, item_var, ..] => {
                            scope.insert(key_var.clone(), key);
                            scope.insert(item_var.clone(), item);
                            for name in vars.iter().skip(2) {
                                scope.insert(name.clone(), Value::Na);
                            }
                        }
                    }
                    self.locals.push(scope);
                    value = self.run_block(body);
                    self.locals.pop();
                    if std::mem::replace(&mut self.control, Control::None) == Control::Break {
                        break;
                    }
                }
                value
            }
            Stmt::While { line, cond, body } => {
                let mut value = Value::Na;
                let mut iterations = 0;
                loop {
                    if !self.eval(cond).flag().unwrap_or(false) {
                        break;
                    }
                    if iterations >= MAX_LOOP_ITERATIONS {
                        self.loop_limit(*line);
                        break;
                    }
                    iterations += 1;
                    if self.charge(*line) {
                        break;
                    }
                    self.locals.push(HashMap::new());
                    value = self.run_block(body);
                    self.locals.pop();
                    if std::mem::replace(&mut self.control, Control::None) == Control::Break {
                        break;
                    }
                }
                value
            }
            Stmt::Break => {
                self.control = Control::Break;
                Value::Na
            }
            Stmt::Continue => {
                self.control = Control::Continue;
                Value::Na
            }
            Stmt::FieldAssign {
                target,
                name,
                value,
            } => {
                let object = self.eval(target).object();
                let value = self.eval(value);
                if let Some(object) = object {
                    value::lock_object(&object).insert(name.clone(), value);
                }
                Value::Na
            }
            Stmt::Expr(value) => self.eval(value),
            Stmt::Function(_) | Stmt::Type(_) | Stmt::Unsupported { .. } => Value::Na,
        }
    }

    fn loop_limit(&mut self, line: usize) {
        let message = "Loop stopped: iteration limit exceeded".to_owned();
        if self.reported.insert((line, message.clone())) {
            self.diagnostics.push(Diagnostic { line, message });
        }
    }

    /// Counts one step of work and stops the whole run when a script does too much of it, so a
    /// recursive or looping script cannot hang the chart.
    fn charge(&mut self, line: usize) -> bool {
        self.steps += 1;
        if !self.out_of_budget && self.steps > MAX_RUN_STEPS {
            self.out_of_budget = true;
            let message = "Script stopped: execution limit exceeded".to_owned();
            if self.reported.insert((line, message.clone())) {
                self.diagnostics.push(Diagnostic { line, message });
            }
        }
        self.out_of_budget
    }

    fn eval(&mut self, expr: &Expr) -> Value {
        if self.out_of_budget {
            return Value::Na;
        }
        match expr {
            Expr::Number(number) => Value::Num(*number),
            Expr::Bool(flag) => Value::Bool(*flag),
            Expr::Str(text) => Value::Str(text.clone()),
            Expr::Color(rgba) => Value::Color(crate::pine_engine::parser::rgba_of(*rgba)),
            Expr::Na => Value::Na,
            Expr::Array(items) => {
                let values: Vec<Value> = items.iter().map(|item| self.eval(item)).collect();
                Value::array_of(values)
            }
            Expr::Lambda { params, body } => Value::Lambda(std::sync::Arc::new(value::Lambda {
                params: params.clone(),
                body: (**body).clone(),
            })),
            Expr::Ident(name) => self.ident(name),
            Expr::Member { target, name } => {
                let value = self.eval(target);
                if let Some(object) = value.object() {
                    return value::lock_object(&object)
                        .get(name)
                        .cloned()
                        .unwrap_or(Value::Na);
                }
                let path = format!("{}.{}", dotted(target).unwrap_or_default(), name);
                self.member(&path)
            }
            Expr::Call {
                id,
                callee,
                receiver,
                args,
                line,
            } => self.call(*id, callee, receiver.as_deref(), args, *line),
            Expr::Index { target, index } => self.index(target, index),
            Expr::Unary { op, value } => {
                let value = self.eval(value);
                match *op {
                    "-" => match value.number() {
                        Some(number) => Value::Num(-number),
                        None => Value::Na,
                    },
                    "not" => Value::Bool(!value.flag().unwrap_or(false)),
                    _ => Value::Na,
                }
            }
            Expr::Binary { op, left, right } => match *op {
                "and" => {
                    if !self.eval(left).flag().unwrap_or(false) {
                        Value::Bool(false)
                    } else {
                        Value::Bool(self.eval(right).flag().unwrap_or(false))
                    }
                }
                "or" => {
                    if self.eval(left).flag().unwrap_or(false) {
                        Value::Bool(true)
                    } else {
                        Value::Bool(self.eval(right).flag().unwrap_or(false))
                    }
                }
                _ => {
                    let left = self.eval(left);
                    let right = self.eval(right);
                    binary(op, left, right)
                }
            },
            Expr::Ternary {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond).flag().unwrap_or(false) {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            Expr::If {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond).flag().unwrap_or(false) {
                    self.run_block(then)
                } else if let Some(otherwise) = otherwise {
                    self.run_block(otherwise)
                } else {
                    Value::Na
                }
            }
            Expr::Switch { subject, arms } => {
                let subject = subject.as_ref().map(|subject| self.eval(subject));
                for arm in arms {
                    // A bare `=>` arm answers when no earlier arm did; any listed value may match.
                    let matched = if arm.conds.is_empty() {
                        true
                    } else {
                        arm.conds.iter().any(|cond| match &subject {
                            Some(subject) => {
                                let cond = self.eval(cond);
                                binary("==", subject.clone(), cond).flag().unwrap_or(false)
                            }
                            None => self.eval(cond).flag().unwrap_or(false),
                        })
                    };
                    if matched {
                        return self.run_block(&arm.body);
                    }
                }
                Value::Na
            }
        }
    }

    fn ident(&self, name: &str) -> Value {
        for scope in self.locals.iter().rev() {
            if let Some(value) = scope.get(name) {
                return value.clone();
            }
        }
        if let Some(var) = self.vars.get(name) {
            return var.value.clone();
        }
        match name {
            "open" => self.bar_value(|bar| bar.open),
            "high" => self.bar_value(|bar| bar.high),
            "low" => self.bar_value(|bar| bar.low),
            "close" => self.bar_value(|bar| bar.close),
            "volume" => self.bar_value(|bar| bar.volume),
            "hl2" => self.bar_average([0.5, 0.0, 0.5, 0.0]),
            "hlc3" => self.bar_average([0.0, 1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]),
            "ohlc4" => self.bar_average([0.25; 4]),
            "bar_index" => Value::Num(self.bar as f64),
            // Pine's execution clock, not a bar time: every bar of a run reads the same instant.
            "timenow" => Value::Num(chrono::Utc::now().timestamp_millis() as f64),
            "last_bar_time" => self
                .bars
                .last()
                .map(|bar| Value::Num(bar.time.timestamp_millis() as f64))
                .unwrap_or(Value::Na),
            "last_bar_index" => Value::Num(self.bars.len().saturating_sub(1) as f64),
            "time" => self
                .bars
                .get(self.bar)
                .map(|bar| Value::Num(bar.time.timestamp_millis() as f64))
                .unwrap_or(Value::Na),
            "hour" => self.clock(|time| time.hour() as f64),
            "minute" => self.clock(|time| time.minute() as f64),
            "second" => self.clock(|time| time.second() as f64),
            "dayofmonth" => self.clock(|time| time.day() as f64),
            "weekofyear" => self.clock(|time| time.iso_week().week() as f64),
            "month" => self.clock(|time| time.month() as f64),
            "year" => self.clock(|time| time.year() as f64),
            "dayofweek" => self.clock(|time| time.weekday().num_days_from_sunday() as f64 + 1.0),
            // v4 scripts spell a colour without the namespace: `plot(close, color=red)`.
            _ => builtins::color_constant(name)
                .map(Value::Color)
                .unwrap_or(Value::Na),
        }
    }

    fn bar_value(&self, field: impl Fn(&Bar) -> f64) -> Value {
        self.bars
            .get(self.bar)
            .map(|bar| Value::Num(field(bar)))
            .unwrap_or(Value::Na)
    }

    fn bar_average(&self, weights: [f64; 4]) -> Value {
        self.bars
            .get(self.bar)
            .map(|bar| {
                Value::Num(
                    bar.open * weights[0]
                        + bar.high * weights[1]
                        + bar.low * weights[2]
                        + bar.close * weights[3],
                )
            })
            .unwrap_or(Value::Na)
    }

    fn clock(&self, field: impl Fn(chrono::DateTime<egui_charts::model::TimezoneId>) -> f64) -> Value {
        self.bars
            .get(self.bar)
            .map(|bar| Value::Num(field(bar.time.with_timezone(&self.context.timezone))))
            .unwrap_or(Value::Na)
    }

    fn member(&self, path: &str) -> Value {
        // An `enum` entry or a type's default: `Color.red` is a constant, not an object field.
        if let Some((namespace, name)) = path.split_once('.')
            && let Some(decl) = self.types.get(namespace)
            && let Some(field) = decl.fields.iter().find(|field| field.name == name)
        {
            return field
                .default
                .as_ref()
                .and_then(literal)
                .unwrap_or(Value::Na);
        }
        match path {
            "math.pi" => return Value::Num(std::f64::consts::PI),
            "math.e" => return Value::Num(std::f64::consts::E),
            "barstate.isfirst" => return Value::Bool(self.bar == 0),
            "barstate.islast" | "barstate.islastconfirmedhistory" => {
                return Value::Bool(self.bar + 1 == self.bars.len());
            }
            "barstate.isconfirmed" | "barstate.isnew" => return Value::Bool(true),
            "barstate.isrealtime" => return Value::Bool(false),
            "barstate.ishistory" => return Value::Bool(true),
            "syminfo.tickerid" | "syminfo.ticker" => {
                return Value::Str(self.context.symbol.clone());
            }
            "syminfo.prefix" => return Value::Str(self.context.exchange.clone()),
            "syminfo.root" => return Value::Str(self.context.root.clone()),
            "syminfo.type" => return Value::Str(self.context.symbol_type.clone()),
            // The instrument has no separate long name here, and the chart runs the regular session.
            "syminfo.description" => return Value::Str(self.context.symbol.clone()),
            "syminfo.session" => return Value::Str("regular".to_owned()),
            "syminfo.mintick" => return Value::Num(self.context.mintick),
            // A contract's point value is unknown to the engine; one is the unit contract.
            "syminfo.pointvalue" => return Value::Num(1.0),
            // The theme is not known while a study runs; a transparent background blends with any
            // pane and the foreground matches the light text of the app's dark theme.
            // shortcut: chart colours ignore the selected theme, pass it through Context if a light
            // theme ever needs them.
            "chart.bg_color" => return Value::Color(Color32::TRANSPARENT),
            "chart.fg_color" => return Value::Color(Color32::from_rgb(209, 212, 220)),
            // A study is computed over the whole series it holds, so all of it counts as visible.
            "chart.left_visible_bar_time" => {
                return Value::Num(
                    self.bars
                        .first()
                        .map_or(0.0, |bar| bar.time.timestamp_millis() as f64),
                );
            }
            "chart.right_visible_bar_time" => {
                return Value::Num(self.bars.last().map_or(0.0, |bar| {
                    (bar.time.timestamp_millis() + self.context.timeframe.duration_ms()) as f64
                }));
            }
            "chart.is_standard" => return Value::Bool(true),
            "chart.is_heikinashi"
            | "chart.is_kagi"
            | "chart.is_linebreak"
            | "chart.is_pnf"
            | "chart.is_range"
            | "chart.is_renko" => return Value::Bool(false),
            "syminfo.timezone" => return Value::Str(self.context.timezone.name().to_owned()),
            "timeframe.period" => return Value::Str(pine_period(self.context.timeframe)),
            "timeframe.multiplier" => return Value::Num(pine_multiplier(self.context.timeframe)),
            "timeframe.in_seconds" | "timeframe.in_seconds()" => {
                return Value::Num((self.context.timeframe.duration_ms() / 1000) as f64);
            }
            "timeframe.isseconds" => {
                return Value::Bool(self.context.timeframe.duration_ms() < 60_000);
            }
            "timeframe.isminutes" => {
                let duration = self.context.timeframe.duration_ms();
                return Value::Bool((60_000..3_600_000).contains(&duration));
            }
            "timeframe.isintraday" => {
                return Value::Bool(self.context.timeframe.duration_ms() < 86_400_000);
            }
            "timeframe.isdaily" => {
                return Value::Bool(self.context.timeframe.duration_ms() == 86_400_000);
            }
            "timeframe.isweekly" => {
                return Value::Bool(self.context.timeframe.duration_ms() == 604_800_000);
            }
            "timeframe.ismonthly" => {
                return Value::Bool(self.context.timeframe.duration_ms() >= 2_419_200_000);
            }
            "timeframe.isdwm" => {
                let duration = self.context.timeframe.duration_ms();
                return Value::Bool(
                    duration == 86_400_000 || duration == 604_800_000 || duration >= 2_419_200_000,
                );
            }
            // `ta.tr` is the true range the series' own bars give, without a `ta.tr()` call.
            "ta.tr" => {
                let Some(bar) = self.bars.get(self.bar) else {
                    return Value::Na;
                };
                let previous = self.bar.checked_sub(1).and_then(|at| self.bars.get(at));
                let range = bar.high - bar.low;
                let up = previous.map_or(0.0, |before| (bar.high - before.close).abs());
                let down = previous.map_or(0.0, |before| (bar.low - before.close).abs());
                return Value::Num(range.max(up).max(down));
            }
            _ => {}
        }
        let (namespace, name) = path.split_once('.').unwrap_or(("", path));
        match namespace {
            "color" => builtins::color_constant(name)
                .map(Value::Color)
                .unwrap_or(Value::Na),
            "dayofweek" => match name {
                "sunday" => Value::Num(1.0),
                "monday" => Value::Num(2.0),
                "tuesday" => Value::Num(3.0),
                "wednesday" => Value::Num(4.0),
                "thursday" => Value::Num(5.0),
                "friday" => Value::Num(6.0),
                "saturday" => Value::Num(7.0),
                _ => Value::Na,
            },
            "timeframe" => match name {
                "isintraday" => {
                    Value::Bool(self.context.timeframe.duration_ms() < 86_400_000)
                }
                "isminutes" => Value::Bool(self.context.timeframe.duration_ms() < 3_600_000),
                "isseconds" => Value::Bool(self.context.timeframe.duration_ms() < 60_000),
                "isdaily" => Value::Bool(self.context.timeframe.duration_ms() == 86_400_000),
                "isweekly" => Value::Bool(self.context.timeframe.duration_ms() == 604_800_000),
                "ismonthly" => Value::Bool(self.context.timeframe.duration_ms() >= 2_592_000_000),
                _ => Value::Bool(false),
            },
            "size" => Value::Num(match name {
                "tiny" => 8.0,
                "small" => 10.0,
                "normal" => 12.0,
                "large" => 16.0,
                "huge" => 20.0,
                _ => 12.0,
            }),
            // `shape.*`, `location.*`, `plot.*` and the like: the names the plot and input
            // calls read back as strings.
            _ if !namespace.is_empty() => Value::Str(name.to_owned()),
            // v4 scripts spell a colour without the namespace: `color=red`.
            _ => builtins::color_constant(path)
                .map(Value::Color)
                .unwrap_or(Value::Na),
        }
    }

    /// `line.new`, `label.new` and `box.new`: one object whose fields hold the coordinates,
    /// style and text that the `get_`/`set_` calls later touch.
    fn new_drawing(&mut self, kind: DrawingKind, args: &[Arg]) -> Value {
        let evaluated: Vec<(Option<String>, Value)> = args
            .iter()
            .map(|arg| (arg.name.clone(), self.eval(&arg.value)))
            .collect();
        let positional = |index: usize| -> Value {
            evaluated
                .iter()
                .filter(|(name, _)| name.is_none())
                .nth(index)
                .map(|(_, value)| value.clone())
                .unwrap_or(Value::Na)
        };
        let named_value = |name: &str| -> Option<Value> {
            evaluated
                .iter()
                .find(|(key, _)| key.as_deref() == Some(name))
                .map(|(_, value)| value.clone())
        };
        let argument = |index: usize, name: &str| -> Value {
            named_value(name).unwrap_or_else(|| positional(index))
        };
        let style = |index: usize, name: &str, fallback: &str| -> Value {
            Value::Str(
                argument(index, name)
                    .text()
                    .map(str::to_owned)
                    .unwrap_or_else(|| fallback.to_owned()),
            )
        };
        // `xloc` follows the corners in each drawing's own signature.
        let xloc = match kind {
            DrawingKind::Line => 4,
            DrawingKind::Label => 3,
            DrawingKind::Box => 8,
            DrawingKind::Fill => usize::MAX,
        };
        let xloc = Value::Str(
            argument(xloc, "xloc")
                .text()
                .unwrap_or("bar_index")
                .to_owned(),
        );
        let is_time = xloc.text() == Some("bar_time");
        let mut fields = HashMap::new();
        fields.insert("__kind".to_owned(), Value::Str(kind_name(kind).to_owned()));
        fields.insert("__deleted".to_owned(), Value::Bool(false));
        fields.insert("xloc".to_owned(), xloc);
        match kind {
            DrawingKind::Line => {
                for (index, key) in ["x1", "y1", "x2", "y2"].into_iter().enumerate() {
                    let value = argument(index, key);
                    let value = if index % 2 == 0 {
                        self.drawing_x_in(value, is_time)
                    } else {
                        value
                    };
                    fields.insert(key.to_owned(), value);
                }
                fields.insert(
                    "color".to_owned(),
                    argument(6, "color")
                        .color()
                        .map_or(Value::Color(Color32::from_rgb(33, 150, 243)), Value::Color),
                );
                fields.insert(
                    "width".to_owned(),
                    Value::Num(
                        argument(8, "width")
                            .number()
                            .unwrap_or(1.0),
                    ),
                );
                fields.insert("style".to_owned(), style(7, "style", "style_solid"));
            }
            DrawingKind::Label => {
                let x = self.drawing_x_in(argument(0, "x"), is_time);
                let y = argument(1, "y");
                fields.insert("x1".to_owned(), x.clone());
                fields.insert("x2".to_owned(), x);
                fields.insert("y1".to_owned(), y.clone());
                fields.insert("y2".to_owned(), y);
                fields.insert(
                    "text".to_owned(),
                    Value::Str(drawing_text(&argument(2, "text"))),
                );
                fields.insert(
                    "color".to_owned(),
                    argument(5, "color")
                        .color()
                        .map_or(Value::Color(Color32::TRANSPARENT), Value::Color),
                );
                fields.insert(
                    "text_color".to_owned(),
                    named_value("textcolor")
                        .or_else(|| named_value("text_color"))
                        .unwrap_or_else(|| positional(7))
                        .color()
                        .map_or(Value::Color(Color32::WHITE), Value::Color),
                );
                fields.insert("style".to_owned(), style(6, "style", "style_label_up"));
                fields.insert(
                    "font_size".to_owned(),
                    Value::Num(f64::from(font_size_of(&argument(8, "size")))),
                );
            }
            DrawingKind::Box => {
                let corners = [
                    ("x1", 0, "left", true),
                    ("y1", 1, "top", false),
                    ("x2", 2, "right", true),
                    ("y2", 3, "bottom", false),
                ];
                for (key, index, name, is_x) in corners {
                    let value = argument(index, name);
                    let value = if is_x {
                        self.drawing_x_in(value, is_time)
                    } else {
                        value
                    };
                    fields.insert(key.to_owned(), value);
                }
                fields.insert(
                    "border_color".to_owned(),
                    argument(4, "border_color")
                        .color()
                        .or_else(|| named_value("color").and_then(|value| value.color()))
                        .map_or(Value::Color(Color32::GRAY), Value::Color),
                );
                fields.insert(
                    "fill_color".to_owned(),
                    argument(9, "bgcolor")
                        .color()
                        .map_or(Value::Color(Color32::TRANSPARENT), Value::Color),
                );
                fields.insert(
                    "text".to_owned(),
                    Value::Str(drawing_text(&argument(10, "text"))),
                );
                fields.insert(
                    "text_color".to_owned(),
                    argument(12, "text_color")
                        .color()
                        .map_or(Value::Color(Color32::WHITE), Value::Color),
                );
                fields.insert(
                    "font_size".to_owned(),
                    Value::Num(f64::from(font_size_of(&argument(11, "text_size")))),
                );
                fields.insert(
                    "border_width".to_owned(),
                    Value::Num(argument(5, "border_width").number().unwrap_or(1.0)),
                );
                fields.insert(
                    "style".to_owned(),
                    style(6, "border_style", "style_solid"),
                );
            }
            DrawingKind::Fill => {
                // `linefill.new(line1, line2, color)`: the fill reads its corners from the two
                // lines, so the lines stay the source of truth.
                fields.insert("line1".to_owned(), argument(0, "line1"));
                fields.insert("line2".to_owned(), argument(1, "line2"));
                fields.insert(
                    "color".to_owned(),
                    named_value("color")
                        .or_else(|| positional(2).color().map(Value::Color))
                        .unwrap_or(Value::Color(Color32::from_rgba_unmultiplied(
                            33, 150, 243, 100,
                        ))),
                );
            }
        }
        let object = Value::object_of(fields);
        if let Some(inner) = object.object() {
            self.drawings.push(inner);
        }
        object
    }

    /// A `get_*`, `set_*`, `delete` or `copy` call on a `line`, `label` or `box`.
    fn drawing_method(
        &mut self,
        object: &value::ObjectRef,
        method: &str,
        args: &[Arg],
    ) -> Option<Value> {
        // Every drawing and user-type instance has a `copy()` that starts a new object from the
        // same fields; a copied drawing is drawn too.
        if method == "copy" {
            let fields = value::lock_object(object);
            let is_drawing = fields.contains_key("__kind");
            let copy = Value::object_of(fields.clone());
            drop(fields);
            if is_drawing
                && let Some(inner) = copy.object()
            {
                self.drawings.push(inner);
            }
            return Some(copy);
        }
        let is_time = {
            let fields = value::lock_object(object);
            if !fields.contains_key("__kind") {
                return None;
            }
            fields.get("xloc").and_then(Value::text) == Some("bar_time")
        };
        let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
        let argument = |index: usize| values.get(index).cloned().unwrap_or(Value::Na);
        let read = |key: &str| -> Value {
            value::lock_object(object)
                .get(key)
                .cloned()
                .unwrap_or(Value::Na)
        };
        match method {
            "get_x1" | "get_left" | "get_x" => {
                return Some(self.drawing_x_out(read("x1"), is_time));
            }
            "get_x2" | "get_right" => return Some(self.drawing_x_out(read("x2"), is_time)),
            "get_y1" | "get_top" | "get_y" | "get_price" => return Some(read("y1")),
            "get_y2" | "get_bottom" => return Some(read("y2")),
            "get_color" => return Some(read("color")),
            "get_line1" => return Some(read("line1")),
            "get_line2" => return Some(read("line2")),
            "get_textcolor" | "get_text_color" => return Some(read("text_color")),
            "get_bgcolor" | "get_fill_color" => return Some(read("fill_color")),
            "get_border_color" => return Some(read("border_color")),
            "get_border_width" => return Some(read("border_width")),
            "get_text" => return Some(read("text")),
            "get_style" => return Some(read("style")),
            "get_width" => return Some(read("width")),
            "get_size" | "get_text_size" | "get_font_size" => return Some(read("font_size")),
            "set_x1" | "set_left" => {
                set_field(object, "x1", self.drawing_x_in(argument(0), is_time));
            }
            "set_x2" | "set_right" => {
                set_field(object, "x2", self.drawing_x_in(argument(0), is_time));
            }
            "set_y1" | "set_top" => set_field(object, "y1", argument(0)),
            "set_y2" | "set_bottom" => set_field(object, "y2", argument(0)),
            "set_x" => {
                let x = self.drawing_x_in(argument(0), is_time);
                set_field(object, "x1", x.clone());
                set_field(object, "x2", x);
            }
            "set_y" => {
                let y = argument(0);
                set_field(object, "y1", y.clone());
                set_field(object, "y2", y);
            }
            "set_xy" => {
                let x = self.drawing_x_in(argument(0), is_time);
                let y = argument(1);
                set_field(object, "x1", x.clone());
                set_field(object, "x2", x);
                set_field(object, "y1", y.clone());
                set_field(object, "y2", y);
            }
            "set_xy1" => {
                set_field(object, "x1", self.drawing_x_in(argument(0), is_time));
                set_field(object, "y1", argument(1));
            }
            "set_xy2" => {
                set_field(object, "x2", self.drawing_x_in(argument(0), is_time));
                set_field(object, "y2", argument(1));
            }
            "set_lefttop" => {
                set_field(object, "x1", self.drawing_x_in(argument(0), is_time));
                set_field(object, "y1", argument(1));
            }
            "set_rightbottom" => {
                set_field(object, "x2", self.drawing_x_in(argument(0), is_time));
                set_field(object, "y2", argument(1));
            }
            "set_text" => set_field(object, "text", Value::Str(drawing_text(&argument(0)))),
            "set_color" => set_field(object, "color", argument(0)),
            "set_textcolor" | "set_text_color" => set_field(object, "text_color", argument(0)),
            "set_bgcolor" | "set_fill_color" => set_field(object, "fill_color", argument(0)),
            "set_border_color" => set_field(object, "border_color", argument(0)),
            "set_width" => set_field(object, "width", argument(0)),
            "set_border_width" => set_field(object, "border_width", argument(0)),
            "set_style" => set_field(object, "style", argument(0)),
            "set_size" | "set_text_size" => {
                set_field(
                    object,
                    "font_size",
                    Value::Num(f64::from(font_size_of(&argument(0)))),
                );
            }
            "set_xloc" => set_field(object, "xloc", argument(0)),
            "set_extend" => set_field(object, "extend", argument(0)),
            // The chart has no tooltip or per-drawing text alignment, so this is accepted and
            // ignored rather than reported as missing.
            "set_tooltip" | "set_textalign" | "set_text_halign" | "set_text_valign"
            | "set_text_wrap" | "set_text_font_family" => {}
            "delete" => set_field(object, "__deleted", Value::Bool(true)),
            "copy" => {
                let fields = value::lock_object(object).clone();
                let object = Value::object_of(fields);
                if let Some(inner) = object.object() {
                    self.drawings.push(inner);
                }
                return Some(object);
            }
            _ => return None,
        }
        Some(Value::Na)
    }

    /// A drawing x argument, in milliseconds when the drawing uses `xloc.bar_time`.
    fn drawing_x_in(&self, value: Value, is_time: bool) -> Value {
        if !is_time {
            return value;
        }
        match value.number() {
            Some(ms) => Value::Num(self.time_to_bar(ms)),
            None => Value::Na,
        }
    }

    /// A drawing x getter, back in milliseconds when the drawing uses `xloc.bar_time`.
    fn drawing_x_out(&self, value: Value, is_time: bool) -> Value {
        if !is_time {
            return value;
        }
        match value.number() {
            Some(bar) => Value::Num(self.bar_time(bar)),
            None => Value::Na,
        }
    }

    /// The bar nearest to a timestamp, so `xloc.bar_time` coordinates land on the chart.
    fn time_to_bar(&self, ms: f64) -> f64 {
        if self.bars.is_empty() {
            return 0.0;
        }
        let after = self
            .bars
            .partition_point(|bar| bar.time.timestamp_millis() as f64 <= ms);
        if after == 0 {
            return 0.0;
        }
        if after >= self.bars.len() {
            return (self.bars.len() - 1) as f64;
        }
        let before = self.bars[after - 1].time.timestamp_millis() as f64;
        let next = self.bars[after].time.timestamp_millis() as f64;
        if ms - before <= next - ms {
            (after - 1) as f64
        } else {
            after as f64
        }
    }

    /// The timestamp of a bar index, clamped to the bars on the chart.
    fn bar_time(&self, bar: f64) -> f64 {
        if self.bars.is_empty() {
            return 0.0;
        }
        let at = (bar.round().max(0.0) as usize).min(self.bars.len() - 1);
        self.bars[at].time.timestamp_millis() as f64
    }

    fn index(&mut self, target: &Expr, index: &Expr) -> Value {
        let offset = match self.eval(index).number() {
            Some(number) => number.round(),
            None => return Value::Na,
        };
        if offset < 0.0 {
            return Value::Na;
        }
        let offset = offset as usize;
        let Expr::Ident(name) = target else {
            return Value::Na;
        };
        if let Some(var) = self.vars.get(name) {
            if offset == 0 {
                return var.value.clone();
            }
            return var
                .history
                .len()
                .checked_sub(offset)
                .and_then(|at| var.history.get(at))
                .cloned()
                .unwrap_or(Value::Na);
        }
        let bar = match self.bar.checked_sub(offset) {
            Some(bar) => bar,
            None => return Value::Na,
        };
        match name.as_str() {
            "open" => Value::Num(self.bars[bar].open),
            "high" => Value::Num(self.bars[bar].high),
            "low" => Value::Num(self.bars[bar].low),
            "close" => Value::Num(self.bars[bar].close),
            "volume" => Value::Num(self.bars[bar].volume),
            "time" => Value::Num(self.bars[bar].time.timestamp_millis() as f64),
            "bar_index" => Value::Num(bar as f64),
            "hour" | "minute" | "second" | "dayofmonth" | "weekofyear" | "month" | "year" | "dayofweek" => {
                let time = self.bars[bar].time.with_timezone(&self.context.timezone);
                Value::Num(match name.as_str() {
                    "hour" => time.hour() as f64,
                    "minute" => time.minute() as f64,
                    "second" => time.second() as f64,
                    "dayofmonth" => time.day() as f64,
                    "weekofyear" => time.iso_week().week() as f64,
                    "month" => time.month() as f64,
                    "year" => time.year() as f64,
                    _ => time.weekday().num_days_from_sunday() as f64 + 1.0,
                })
            }
            _ => Value::Na,
        }
    }

    fn call(
        &mut self,
        id: NodeId,
        callee: &[String],
        receiver: Option<&Expr>,
        args: &[Arg],
        line: usize,
    ) -> Value {
        let name = callee.join(".");
        // A v3/v4 script calls its builtins without a namespace: `sma(...)`, `abs(...)`, `red`.
        let name = legacy_name(&name).unwrap_or(name);
        match name.as_str() {
            "plot" => return self.plot(id, args),
            "plotcandle" | "plotbar" => {
                return self.plot_candle(id, args, name == "plotbar");
            }
            "plotshape" | "plotchar" | "plotarrow" => {
                return self.plotshape(id, args, &name);
            }
            "fill" => return self.fill(id, args),
            "hline" => return self.hline(id, args),
            "barcolor" => return self.colour_plot(id, args, PlotType::BarColor),
            "bgcolor" => return self.colour_plot(id, args, PlotType::BackgroundColor),
            "alertcondition" | "alert" => return Value::Na,
            // `max_bars_back(source, length)` only sizes the history Pine keeps, and this engine
            // keeps the bars a study reads.
            "max_bars_back" => return Value::Na,
            // v4's `iff(condition, iftrue, iffalse)` is a function, so both branches evaluate.
            "iff" => {
                let condition = args
                    .first()
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.flag())
                    .unwrap_or(false);
                let at = if condition { 1 } else { 2 };
                return args
                    .get(at)
                    .map(|arg| self.eval(&arg.value))
                    .unwrap_or(Value::Na);
            }
            _ => {}
        }
        if name == "input" || name.starts_with("input.") {
            let value = self
                .inputs
                .get(&id)
                .cloned()
                .or_else(|| {
                    // Without a setting the script's own default is what Pine answers.
                    self.program
                        .inputs
                        .iter()
                        .find(|decl| decl.id == id)
                        .map(|decl| decl.default.clone())
                })
                .unwrap_or(Value::Na);
            // `input.source` holds the name of the series the chart feeds.
            if name.ends_with(".source")
                && let Some(source) = value.text()
            {
                return self.ident(source);
            }
            return value;
        }
        if name == "nz" || name == "fixnan" {
            let value = args.first().map(|arg| self.eval(&arg.value));
            let replacement = args.get(1).map(|arg| self.eval(&arg.value));
            return match value {
                Some(value) if !matches!(value, Value::Na) => value,
                _ => replacement.unwrap_or(Value::Num(0.0)),
            };
        }
        if name == "na" {
            let value = args.first().map(|arg| self.eval(&arg.value));
            return Value::Bool(matches!(value, None | Some(Value::Na)));
        }
        if matches!(
            name.as_str(),
            "int" | "float" | "bool" | "hour" | "minute" | "second" | "year" | "month"
                | "dayofmonth" | "dayofweek" | "weekofyear" | "time" | "time_close" | "timestamp"
        ) {
            return self.time_call(&name, args);
        }
        if let Some(rest) = name.strip_prefix("ta.") {
            let bar = self.bars[self.bar].clone();
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            let values = ta_arguments(rest, args, values);
            let state = self.ta.entry(id).or_default();
            return match builtins::ta(rest, state, &values, &bar) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if name == "math.round_to_mintick" {
            let value = args.first().map(|arg| self.eval(&arg.value));
            let tick = self.context.mintick;
            return value
                .and_then(|value| value.number())
                .map(|number| Value::Num((number / tick).round() * tick))
                .unwrap_or(Value::Na);
        }
        // `log.info`, `runtime.log` and their siblings only write to the editor console, which has
        // none here; a script that logs still runs.
        if name.starts_with("log.") || name.starts_with("runtime.") {
            return Value::Na;
        }
        if let Some(rest) = name.strip_prefix("math.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            // `math.sum(source, length)` sums the last `length` values, so it needs the bar
            // history the `ta` state keeps.
            if rest == "sum" {
                let bar = self.bars[self.bar].clone();
                let values = ta_arguments(rest, args, values);
                let state = self.ta.entry(id).or_default();
                return builtins::ta(rest, state, &values, &bar).unwrap_or(Value::Na);
            }
            return match builtins::math(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("color.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::color(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("str.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            // `str.match(regex)` reads the symbol, `str.match(text, regex)` the text it is given.
            if rest == "match" {
                return match values.as_slice() {
                    [pattern] => Value::Bool(matches_regex(&self.context.symbol, pattern.text())),
                    [subject, pattern] => Value::Bool(matches_regex(
                        subject.text().unwrap_or_default(),
                        pattern.text(),
                    )),
                    _ => Value::Na,
                };
            }
            // `str.tostring(price, format.mintick)` prints at the instrument's tick precision.
            if rest == "tostring" && values.get(1).and_then(Value::text) == Some("mintick") {
                return match values.first().and_then(Value::number) {
                    Some(number) => Value::Str(mintick_text(number, self.context.mintick)),
                    None => Value::Na,
                };
            }
            return match builtins::text(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("array.") {
            // `array.every` and `array.some` call their lambda, which needs the evaluator.
            if matches!(rest, "every" | "some") {
                return self.array_test(rest == "every", args);
            }
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::array(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("matrix.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::matrix(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("map.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::map(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        // `line.new(x1, y1, x2, y2)`, `box.get_left(box)` and the rest of the drawing API.
        if callee.len() == 2
            && let Some(kind) = drawing_kind(&callee[0])
        {
            if callee[1] == "new" {
                return self.new_drawing(kind, args);
            }
            let value = args
                .first()
                .map(|arg| self.eval(&arg.value))
                .unwrap_or(Value::Na);
            let Some(object) = value.object() else {
                // A `na` drawing is what an empty array gives back; a number is a call this
                // engine still cannot read.
                return if matches!(value, Value::Na) {
                    Value::Na
                } else {
                    self.unsupported(line, &name)
                };
            };
            return self
                .drawing_method(&object, &callee[1], &args[1..])
                .unwrap_or(Value::Na);
        }
        // `Point.new(x, y)`: a user type's constructor fills its fields in declaration order.
        if callee.len() == 2
            && callee[1] == "new"
            && let Some(decl) = self.types.get(callee[0].as_str()).copied()
        {
            let mut fields = HashMap::new();
            for field in &decl.fields {
                let default = match &field.default {
                    Some(default) => self.eval(default),
                    None => Value::Na,
                };
                fields.insert(field.name.clone(), default);
            }
            for (at, arg) in args.iter().enumerate() {
                let value = self.eval(&arg.value);
                let name = arg
                    .name
                    .clone()
                    .or_else(|| decl.fields.get(at).map(|field| field.name.clone()));
                if let Some(name) = name {
                    fields.insert(name, value);
                }
            }
            return Value::object_of(fields);
        }
        // A method call on a value: `values.push(x)`, `setup.check()`, `kz._box.size`. A bare
        // name the script never declared is a namespace, so `box.delete(x)` is not a user method.
        if let Some(receiver) = receiver
            && let Some(method) = callee.last()
            && !matches!(receiver, Expr::Ident(name) if !self.is_declared(name))
        {
            let value = self.eval(receiver);
            if let Some(items) = value.array() {
                let mut values = vec![Value::Array(items)];
                values.extend(args.iter().map(|arg| self.eval(&arg.value)));
                if let Some(result) = builtins::array(method, &values) {
                    return result;
                }
            }
            // `n.get(0, 1)` reads a matrix through its method form.
            if let Some(items) = value.matrix() {
                let mut values = vec![Value::Matrix(items)];
                values.extend(args.iter().map(|arg| self.eval(&arg.value)));
                if let Some(result) = builtins::matrix(method, &values) {
                    return result;
                }
            }
            // `m.get("key")` reads a map through its method form.
            if let Some(entries) = value.map() {
                let mut values = vec![Value::Map(entries)];
                values.extend(args.iter().map(|arg| self.eval(&arg.value)));
                if let Some(result) = builtins::map(method, &values) {
                    return result;
                }
            }
            // `kz._hi_line.get(0).set_x(time)`, `lbl.set_text(...)`.
            if let Some(object) = value.object()
                && let Some(result) = self.drawing_method(&object, method, args)
            {
                return result;
            }
            // A method on `na` — an empty array's `pop()`, an empty box list's `get(0)` — is a
            // no-op rather than a syntax the editor should report.
            if matches!(value, Value::Na) {
                return Value::Na;
            }
            if matches!(value, Value::Object(_))
                && let Some(function) = self.functions.get(method.as_str()).copied()
            {
                return self.call_function(function, args, Some(value), line);
            }
        }
        if let Some(rest) = name.strip_prefix("timeframe.") {
            return match rest {
                "in_seconds" => Value::Num((self.context.timeframe.duration_ms() / 1000) as f64),
                // True on the first bar of a new `period`, as `timeframe.change` documents.
                "change" => {
                    let period = args
                        .first()
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.text().map(str::to_owned));
                    let changed = period
                        .as_deref()
                        .and_then(Bucket::parse)
                        .zip(self.bar.checked_sub(1))
                        .map_or(self.bar == 0, |(bucket, previous)| {
                            let now = self.bars[self.bar].time.timestamp_millis();
                            let before = self.bars[previous].time.timestamp_millis();
                            bucket.key(now) != bucket.key(before)
                        });
                    Value::Bool(changed)
                }
                _ => Value::Na,
            };
        }
        // `syminfo.ticker(symbol)` reads the ticker part of a symbol, as the `syminfo.ticker`
        // property reads the chart's own.
        if name == "syminfo.ticker" {
            let symbol = args
                .first()
                .map(|arg| self.eval(&arg.value))
                .and_then(|value| value.text().map(str::to_owned))
                .unwrap_or_else(|| self.context.symbol.clone());
            return Value::Str(symbol.rsplit(':').next().unwrap_or_default().to_owned());
        }
        if name == "request.security" {
            return self.security(id, args);
        }        if name == "request.security_lower_tf" {
            return self.security_lower_tf(args);
        }
        if name.starts_with("request.") {
            // The terminal holds no fundamentals or corporate actions, which Pine also answers
            // `na` for when the data is not there.
            return Value::Na;
        }
        if name.starts_with("table.") {
            return self.table_call(&name["table.".len()..], args);
        }
        if callee.len() == 1
            && let Some(function) = self.functions.get(name.as_str()).copied()
        {
            return self.call_function(function, args, None, line);
        }
        self.unsupported(line, &name)
    }

    /// `request.security(symbol, timeframe, expression)`: the expression's value on the last
    /// closed bar of `timeframe`, sampled from the bars on the chart. Another symbol reads the
    /// history the host supplied for it. With `lookahead=barmerge.lookahead_on` the higher bar is
    /// taken whole as soon as it opens, as Pine does when it repaints historical security values.
    fn security(&mut self, id: NodeId, args: &[Arg]) -> Value {
        let symbol = args
            .first()
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(str::to_owned))
            .unwrap_or_default();
        let period = args
            .get(1)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(str::to_owned))
            .unwrap_or_default();
        let Some(expr) = args.get(2).map(|arg| &arg.value) else {
            return Value::Na;
        };
        let lookahead = named(args, "lookahead")
            .or_else(|| args.get(4).filter(|arg| arg.name.is_none()))
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(|text| text == "lookahead_on"))
            .unwrap_or(false);
        let chart = Bucket::chart(self.context.timeframe);
        // An empty timeframe means the chart's own, as Pine documents.
        let bucket = match period.trim() {
            "" => chart,
            text => match Bucket::parse(text) {
                Some(bucket) => bucket,
                None => return Value::Na,
            },
        };
        if !symbol.is_empty() && symbol != self.context.symbol {
            return self.security_other(id, &symbol, bucket, expr, lookahead);
        }
        if bucket == chart {
            return self.eval(expr);
        }
        if bucket.duration_ms() < self.context.timeframe.duration_ms() {
            // A lower timeframe is `request.security_lower_tf`'s series, which this engine does
            // not hold.
            return Value::Na;
        }
        if bucket == Bucket::Day && !lookahead {
            let zone = named(args, "timezone")
                .map(|arg| self.eval(&arg.value))
                .and_then(|value| value.text().and_then(egui_charts::model::TimezoneId::named))
                .unwrap_or_else(|| self.context.timezone.clone());
            return self.security_same_daily(id, &symbol, expr, &zone);
        }
        let Some(bar) = self.bars.get(self.bar).cloned() else {
            return Value::Na;
        };
        let key = bucket.key_in_timezone(bar.time.timestamp_millis(), &self.context.timezone);
        let mut slot = self.security.remove(&id).unwrap_or_else(|| SecuritySlot {
            bars: Vec::new(),
            values: Vec::new(),
            bucket,
            key: None,
            ta: HashMap::new(),
        });
        if lookahead {
            if slot.key != Some(key) {
                // The higher bar is known whole from the first chart bar of its bucket, which is
                // what `lookahead_on` repaints.
                let mut whole = bar.clone();
                let mut ahead = self.bar + 1;
                while let Some(next) = self.bars.get(ahead) {
                    if bucket.key_in_timezone(next.time.timestamp_millis(), &self.context.timezone) != key {
                        break;
                    }
                    whole.high = whole.high.max(next.high);
                    whole.low = whole.low.min(next.low);
                    whole.close = next.close;
                    whole.volume += next.volume;
                    ahead += 1;
                }
                slot.bars.push(whole);
                slot.key = Some(key);
                let completed = slot.bars.len() - 1;
                let value = self.eval_security(&mut slot, expr, completed);
                slot.values.push(value);
            }
        } else if slot.key != Some(key) {
            if slot.key.is_some() {
                let completed = slot.bars.len() - 1;
                let value = self.eval_security(&mut slot, expr, completed);
                slot.values.push(value);
            }
            slot.bars.push(bar);
            slot.key = Some(key);
        } else if !lookahead && let Some(live) = slot.bars.last_mut() {
            live.high = live.high.max(bar.high);
            live.low = live.low.min(bar.low);
            live.close = bar.close;
            live.volume += bar.volume;
        }
        // The chart's newest bar may still be developing. Re-evaluate the current
        // higher-timeframe bar there so live daily levels do not lag a full day.
        // Earlier bars keep the non-repainting, last-closed-bar behavior.
        let value = if !lookahead && self.bar + 1 == self.bars.len() && !slot.bars.is_empty() {
            let current = slot.bars.len() - 1;
            self.eval_security(&mut slot, expr, current)
        } else {
            slot.values.last().cloned().unwrap_or(Value::Na)
        };
        self.security.insert(id, slot);
        value
    }

    /// Read the provider's daily series instead of folding extended intraday bars into it.
    fn security_same_daily(&mut self, id: NodeId, symbol: &str, expr: &Expr, timezone: &egui_charts::model::TimezoneId) -> Value {
        use egui_charts::model::Timeframe;
        let request = (symbol.to_owned(), Timeframe::Day1);
        if !self.security_requests.contains(&request) {
            self.security_requests.push(request);
        }
        let Some(source) = self.sources.iter().find(|source| source.symbol == symbol && source.timeframe == Timeframe::Day1) else {
            return Value::Na;
        };
        let Some(current_time) = self.bars.get(self.bar).map(|bar| bar.time) else { return Value::Na; };
        let date = current_time.with_timezone(timezone).date_naive();
        let completed = source.bars.partition_point(|bar| bar.time.with_timezone(timezone).date_naive() < date);
        let mut slot = self.security.remove(&id).unwrap_or_else(|| SecuritySlot {
            bars: Vec::new(), values: Vec::new(), bucket: Bucket::Day, key: None, ta: HashMap::new(),
        });
        // Closed daily bars are evaluated only once as chart time advances.
        let new_bars = source.bars[slot.bars.len().min(completed)..completed].to_vec();
        let today = source.bars.get(completed).filter(|bar| bar.time.with_timezone(timezone).date_naive() == date).cloned();
        for bar in new_bars {
            slot.bars.push(bar);
            let index = slot.bars.len() - 1;
            let value = self.eval_security(&mut slot, expr, index);
            slot.values.push(value);
        }
        let result = if self.bar + 1 == self.bars.len() {
            let missing = Bar::new(current_time, f64::NAN, f64::NAN, f64::NAN, f64::NAN, 0.0);
            slot.bars.push(today.unwrap_or(missing));
            let index = slot.bars.len() - 1;
            self.eval_security(&mut slot, expr, index)
        } else {
            slot.values.last().cloned().unwrap_or(Value::Na)
        };
        self.security.insert(id, slot);
        result
    }

    /// `request.security` on another symbol: samples the host's history for that symbol and
    /// interval, evaluating the expression once per bar, as [`Self::security`] does for the
    /// chart's own bars. The request is recorded so the chart can fetch the history.
    fn security_other(
        &mut self,
        id: NodeId,
        symbol: &str,
        bucket: Bucket,
        expr: &Expr,
        lookahead: bool,
    ) -> Value {
        let Some(timeframe) = bucket.timeframe() else {
            return Value::Na;
        };
        let request = (symbol.to_owned(), timeframe);
        if !self.security_requests.contains(&request) {
            self.security_requests.push(request);
        }
        if bucket.duration_ms() < self.context.timeframe.duration_ms() {
            // A lower timeframe is `request.security_lower_tf`'s series.
            return Value::Na;
        }
        let Some(bar) = self.bars.get(self.bar) else {
            return Value::Na;
        };
        let now = bar.time.timestamp_millis();
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.symbol == symbol && source.timeframe == timeframe)
        else {
            return Value::Na;
        };
        let Some(first) = source.bars.first().map(|bar| bar.time.timestamp_millis()) else {
            return Value::Na;
        };
        // Without lookahead only a closed bar counts; with it the bar containing the chart
        // time is taken whole as soon as it opens.
        let seen = if lookahead {
            source
                .bars
                .partition_point(|bar| bar.time.timestamp_millis() <= now)
        } else {
            closed_bars(&source.bars, timeframe, now)
        };
        let mut slot = self.security.remove(&id).unwrap_or_else(|| SecuritySlot {
            bars: Vec::new(),
            values: Vec::new(),
            bucket,
            key: Some(first),
            ta: HashMap::new(),
        });
        // A changed interval or a deeper history re-reads the series from its start.
        if slot.bucket != bucket || slot.key != Some(first) {
            slot = SecuritySlot {
                bars: Vec::new(),
                values: Vec::new(),
                bucket,
                key: Some(first),
                ta: HashMap::new(),
            };
        }
        let new_bars: Vec<Bar> = source.bars[slot.bars.len().min(seen)..seen].to_vec();
        for bar in new_bars {
            let index = slot.bars.len();
            slot.bars.push(bar);
            let value = self.eval_security(&mut slot, expr, index);
            slot.values.push(value);
        }
        let value = slot.values.last().cloned().unwrap_or(Value::Na);
        self.security.insert(id, slot);
        value
    }

    /// Runs a `request.security` expression over the higher-timeframe series once per closed bar,
    /// keeping its `ta` state between those bars.
    fn eval_security(&mut self, slot: &mut SecuritySlot, expr: &Expr, bar: usize) -> Value {
        let mut sub = Evaluator::new(self.program, &[], self.context, self.inputs, self.sources);
        sub.bars = Cow::Owned(std::mem::take(&mut slot.bars));
        sub.ta = std::mem::take(&mut slot.ta);
        // A chart series passed to `request.security` keeps the value it had when the
        // higher-timeframe bar closed.
        sub.vars = self
            .vars
            .iter()
            .map(|(name, var)| {
                (
                    name.clone(),
                    Var {
                        value: var.value.clone(),
                        history: Vec::new(),
                    },
                )
            })
            .collect();
        sub.bar = bar;
        let value = sub.eval(expr);
        for diagnostic in sub.diagnostics.drain(..) {
            if self
                .reported
                .insert((diagnostic.line, diagnostic.message.clone()))
            {
                self.diagnostics.push(diagnostic);
            }
        }
        slot.bars = std::mem::take(&mut sub.bars).into_owned();
        slot.ta = std::mem::take(&mut sub.ta);
        for request in sub.security_requests.drain(..) {
            if !self.security_requests.contains(&request) {
                self.security_requests.push(request);
            }
        }
        value
    }

    /// `request.security_lower_tf`: this engine only holds the chart's bars, so each element is a
    /// one-bar list of the value on the current bar.
    fn security_lower_tf(&mut self, args: &[Arg]) -> Value {
        let Some(expr) = args.get(2).map(|arg| &arg.value) else {
            return Value::Na;
        };
        let value = self.eval(expr);
        match value.array() {
            Some(items) => {
                let wrapped: Vec<Value> = value::lock(&items)
                    .iter()
                    .map(|item| Value::array_of(vec![item.clone()]))
                    .collect();
                Value::array_of(wrapped)
            }
            None => Value::array_of(vec![Value::array_of(vec![value])]),
        }
    }

    /// `time("", session, timezone, bars_back = n)`: the bar's open time, or `na` when the bar
    /// is outside the session. `None` when this is the plain `time(timestamp)` form.
    fn session_time(&mut self, name: &str, args: &[Arg]) -> Option<Value> {
        let session = args
            .get(1)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(str::to_owned))?;
        if !session.contains('-') {
            return None;
        }
        let zone = named(args, "timezone")
            .or_else(|| args.get(2))
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().and_then(egui_charts::model::TimezoneId::named))
            .unwrap_or_else(|| self.context.timezone.clone());
        let back = named(args, "bars_back")
            .or_else(|| args.get(3))
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.number())
            .unwrap_or(0.0)
            .max(0.0) as usize;
        let bar = self.bar.checked_sub(back)?;
        let time = self.bars.get(bar)?.time.with_timezone(&zone);
        if !session_contains(&session, &time) {
            return Some(Value::Na);
        }
        let open = time.timestamp_millis() as f64;
        Some(Value::Num(if name == "time_close" {
            open + self.context.timeframe.duration_ms() as f64
        } else {
            open
        }))
    }

    /// `hour`, `dayofweek`, `int` and the other bare calls Pine also offers as variables.
    fn time_call(&mut self, name: &str, args: &[Arg]) -> Value {
        let first = args.first().map(|arg| self.eval(&arg.value));
        let bar_time = self.bars[self.bar].time.timestamp_millis() as f64;
        // Calendar and clock calls take the zone as their last argument, e.g. `hour(time, tz)`.
        let zone = args
            .get(1)
            .filter(|_| {
                matches!(
                    name,
                    "year"
                        | "month"
                        | "dayofmonth"
                        | "dayofweek"
                        | "weekofyear"
                        | "hour"
                        | "minute"
                        | "second"
                )
            })
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| {
                value
                    .text()
                    .and_then(egui_charts::model::TimezoneId::named)
            })
            .or_else(|| Some(self.context.timezone.clone()));
        match name {
            "int" => match first.and_then(|value| value.number()) {
                Some(number) => Value::Num(number.trunc()),
                None => Value::Na,
            },
            "float" => match first.and_then(|value| value.number()) {
                Some(number) => Value::Num(number),
                None => Value::Na,
            },
            "bool" => Value::Bool(first.and_then(|value| value.flag()).unwrap_or(false)),
            "time" | "time_close" => {
                let bucket = first
                    .as_ref()
                    .and_then(Value::text)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .and_then(Bucket::parse);
                let inside = |bucket: Bucket| {
                    let key = bucket.key(bar_time as i64);
                    Value::Num(if name == "time_close" {
                        bucket.close(key) as f64
                    } else {
                        bucket.start(key) as f64
                    })
                };
                if let Some(value) = self.session_time(name, args) {
                    // Outside the session `time` is `na`; inside it, a timeframe moves to that
                    // bar's open or close.
                    if let Value::Na = value {
                        return value;
                    }
                    return bucket.map_or(value, inside);
                }
                if let Some(bucket) = bucket {
                    return inside(bucket);
                }
                let millis = first.and_then(|value| value.number()).unwrap_or(bar_time);
                Value::Num(if name == "time_close" {
                    millis + self.context.timeframe.duration_ms() as f64
                } else {
                    millis
                })
            }
            "timestamp" => {
                let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
                let (zone, rest) = match values.first().and_then(Value::text) {
                    Some(name) => (
                        egui_charts::model::TimezoneId::named(name),
                        &values[1..],
                    ),
                    None => (None, &values[..]),
                };
                let numbers: Vec<f64> = rest.iter().filter_map(Value::number).collect();
                if numbers.len() < 3 {
                    return Value::Na;
                }
                let part = |at: usize, fallback: u32| numbers.get(at).map_or(fallback, |v| *v as u32);
                let (year, month, day) = (part(0, 1970), part(1, 1), part(2, 1));
                let date = chrono::NaiveDate::from_ymd_opt(year as i32, month, day);
                let Some(local) = date.and_then(|date| {
                    date.and_hms_opt(part(3, 0), part(4, 0), part(5, 0))
                }) else {
                    return Value::Na;
                };
                let utc = match zone {
                    Some(zone) => zone
                        .from_local_datetime(&local)
                        .earliest()
                        .map(|time| time.timestamp_millis()),
                    None => Some(local.and_utc().timestamp_millis()),
                };
                utc.map_or(Value::Na, |millis| Value::Num(millis as f64))
            }
            _ => {
                let millis = first.and_then(|value| value.number()).unwrap_or(bar_time);
                let Some(utc) = chrono::DateTime::from_timestamp_millis(millis as i64) else {
                    return Value::Na;
                };
                let time = match &zone {
                    Some(zone) => utc.with_timezone(zone).naive_local(),
                    None => utc.naive_utc(),
                };
                match name {
                    "year" => Value::Num(time.year() as f64),
                    "month" => Value::Num(time.month() as f64),
                    "dayofmonth" => Value::Num(time.day() as f64),
                    "dayofweek" => {
                        Value::Num(time.weekday().num_days_from_sunday() as f64 + 1.0)
                    }
                    "hour" => Value::Num(time.hour() as f64),
                    "minute" => Value::Num(time.minute() as f64),
                    "second" => Value::Num(time.second() as f64),
                    "weekofyear" => Value::Num(time.iso_week().week() as f64),
                    _ => Value::Na,
                }
            }
        }
    }

    fn call_function(
        &mut self,
        function: &'a Function,
        args: &[Arg],
        receiver: Option<Value>,
        line: usize,
    ) -> Value {
        if self.depth > 64 || self.charge(line) {
            return Value::Na;
        }
        // Each written argument fills its parameter: by name when it has one, else in order.
        let mut slots: Vec<Option<Value>> = vec![None; function.params.len()];
        let mut next = 0;
        // A method's first parameter is the object it was called on.
        if let Some(receiver) = receiver {
            if let Some(first) = slots.first_mut() {
                *first = Some(receiver);
            }
            next = 1;
        }
        for arg in args {
            let value = self.eval(&arg.value);
            let at = arg
                .name
                .as_deref()
                .and_then(|name| function.params.iter().position(|param| param == name))
                .unwrap_or(next);
            if let Some(slot) = slots.get_mut(at) {
                *slot = Some(value);
            }
            if arg.name.is_none() {
                next = at + 1;
            }
        }
        let mut scope = HashMap::new();
        for (at, param) in function.params.iter().enumerate() {
            scope.insert(
                param.clone(),
                slots[at].clone().unwrap_or(Value::Na),
            );
        }
        self.locals.push(scope);
        // A parameter the caller left out takes the default the script declared for it.
        for (at, default) in function.defaults.iter().enumerate() {
            let Some(default) = default else { continue };
            if slots[at].is_some() {
                continue;
            }
            let value = self.eval(default);
            if let Some(scope) = self.locals.last_mut() {
                scope.insert(function.params[at].clone(), value);
            }
        }
        self.depth += 1;
        let control = std::mem::replace(&mut self.control, Control::None);
        let value = self.run_block(&function.body);
        self.control = control;
        self.depth -= 1;
        self.locals.pop();
        value
    }

    fn plot(&mut self, id: NodeId, args: &[Arg]) -> Value {
        let Some(first) = args.first() else {
            return Value::Na;
        };
        let value = self.eval(&first.value);
        let bar_color = positional(args, "color", 2)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let slot = match self.plot_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let index = self.plots.len();
                let title = title_of(args).unwrap_or_else(|| format!("Plot {}", index + 1));
                let color = bar_color.unwrap_or_else(|| plot_color(index));
                let width = positional(args, "linewidth", 3)
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.number())
                    .unwrap_or(1.0) as f32;
                let offset = positional(args, "offset", 7)
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.number())
                    .unwrap_or(0.0) as isize;
                let plot_type = positional(args, "style", 4)
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.text().map(plot_type))
                    .unwrap_or(PlotType::Line);
                let pattern = named(args, "linestyle")
                    .map(|arg| self.eval(&arg.value))
                    .as_ref()
                    .map(line_pattern)
                    .unwrap_or_default();
                let visible = !matches!(
                    positional(args, "display", 11)
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.text().map(str::to_owned))
                        .as_deref(),
                    Some("none" | "data_window" | "status_line")
                );
                let track_price = positional(args, "trackprice", 5)
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.flag())
                    .unwrap_or(false);
                self.plots.push(PlotSlot {
                    series: PlotSeries {
                        title,
                        color,
                        width: width.clamp(0.5, 8.0),
                        plot_type,
                        pattern,
                        visible,
                        track_price,
                        track_price_solid: named(args, "linestyle").is_some() && pattern == egui_charts::model::LineStyle::Solid,
                        offset,
                        values: vec![f64::NAN; self.bars.len()],
                        colors: vec![None; self.bars.len()],
                    },
                });
                self.plot_at.insert(id, index);
                index
            }
        };
        if let Some(number) = value.number() {
            let plot = &mut self.plots[slot].series;
            plot.values[self.bar] = number;
            plot.colors[self.bar] = bar_color;
        }
        Value::Plot(slot)
    }

    fn hline(&mut self, id: NodeId, args: &[Arg]) -> Value {
        let Some(first) = args.first() else {
            return Value::Na;
        };
        let value = self.eval(&first.value);
        let slot = match self.plot_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let title =
                    title_of(args).unwrap_or_else(|| format!("Level {}", self.plots.len() + 1));
                let color = positional(args, "color", 2)
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.color())
                    .unwrap_or_else(|| plot_color(self.plots.len()));
                let index = self.plots.len();
                let pattern = positional(args, "linestyle", 3)
                    .map(|arg| self.eval(&arg.value))
                    .as_ref()
                    .map(line_pattern)
                    .unwrap_or_default();
                let width = positional(args, "linewidth", 4)
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.number())
                    .unwrap_or(1.0) as f32;
                let visible = !matches!(
                    positional(args, "display", 7)
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.text().map(str::to_owned))
                        .as_deref(),
                    Some("none" | "data_window" | "status_line")
                );
                self.plots.push(PlotSlot {
                    series: PlotSeries {
                        title,
                        color,
                        width: width.clamp(0.5, 8.0),
                        plot_type: PlotType::Line,
                        pattern,
                        visible,
                        track_price: false,
                        track_price_solid: false,
                        offset: 0,
                        values: vec![f64::NAN; self.bars.len()],
                        colors: vec![None; self.bars.len()],
                    },
                });
                self.plot_at.insert(id, index);
                index
            }
        };
        if let Some(number) = value.number() {
            self.plots[slot].series.values[self.bar] = number;
        }
        Value::Plot(slot)
    }

    /// `plotcandle(open, high, low, close)` and `plotbar(...)`: one OHLC candle per bar, styled by
    /// the plot their call site creates.
    fn plot_candle(&mut self, id: NodeId, args: &[Arg], bar_style: bool) -> Value {
        let mut numbers = [None; 4];
        for (at, name) in ["open", "high", "low", "close"].into_iter().enumerate() {
            let arg = named(args, name).or_else(|| args.get(at).filter(|arg| arg.name.is_none()));
            if let Some(arg) = arg {
                numbers[at] = self.eval(&arg.value).number();
            }
        }
        let color = positional(args, "color", 5)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let slot = match self.plot_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let index = self.plots.len();
                // `title_of` reads the second argument, which here is the high, so `title=` or
                // the fifth positional.
                let title = positional(args, "title", 4)
                    .map(|arg| &arg.value)
                    .and_then(literal)
                    .and_then(|value| value.text().map(str::to_owned))
                    .unwrap_or_else(|| format!("Plot {}", index + 1));
                let width = named(args, "linewidth")
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.number())
                    .unwrap_or(1.0) as f32;
                let visible = !matches!(
                    positional(args, "display", 10)
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.text().map(str::to_owned))
                        .as_deref(),
                    Some("none" | "data_window" | "status_line")
                );
                self.plots.push(PlotSlot {
                    series: PlotSeries {
                        title,
                        color: color.unwrap_or_else(|| plot_color(index)),
                        width: width.clamp(0.5, 8.0),
                        // The candles draw the output; the line stays empty.
                        plot_type: PlotType::Line,
                        pattern: Default::default(),
                        visible,
                        track_price: false,
                        track_price_solid: false,
                        offset: 0,
                        values: vec![f64::NAN; self.bars.len()],
                        colors: vec![None; self.bars.len()],
                    },
                });
                self.plot_at.insert(id, index);
                index
            }
        };
        if let [Some(open), Some(high), Some(low), Some(close)] = numbers {
            let source = &self.bars[self.bar];
            let candle = Bar::new(source.time, open, high, low, close, source.volume);
            self.candles.push(PlotCandle {
                bar: self.bar,
                candle,
                plot: slot,
                color,
                bar_style,
            });
        }
        Value::Plot(slot)
    }

    /// `array.every(id, function)` and `array.some(id, function)`.
    fn array_test(&mut self, every: bool, args: &[Arg]) -> Value {
        let Some(items) = args
            .first()
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.array())
        else {
            return Value::Na;
        };
        let Some(check) = args.get(1).map(|arg| self.eval(&arg.value)) else {
            return Value::Na;
        };
        let items = value::lock(&items).clone();
        for item in items {
            let passed = self.call_lambda(&check, vec![item]).flag().unwrap_or(false);
            if passed != every {
                return Value::Bool(!every);
            }
        }
        Value::Bool(every)
    }

    /// Calls `lambda` with `args` bound to its parameters, as one array element sees it.
    fn call_lambda(&mut self, lambda: &Value, args: Vec<Value>) -> Value {
        let Some(lambda) = lambda.lambda() else {
            return Value::Na;
        };
        let mut scope = HashMap::new();
        for (param, value) in lambda.params.iter().zip(args) {
            scope.insert(param.clone(), value);
        }
        self.locals.push(scope);
        let value = self.eval(&lambda.body);
        self.locals.pop();
        value
    }

    /// `barcolor(color)` and `bgcolor(color)`: the chart paints these from a plot whose per-bar
    /// colour carries the tint and whose defined value marks the bar.
    fn colour_plot(&mut self, id: NodeId, args: &[Arg], plot_type: PlotType) -> Value {
        let color = named(args, "color")
            .or_else(|| args.first())
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let slot = match self.plot_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let index = self.plots.len();
                let title = positional(args, "title", 4)
                    .map(|arg| &arg.value)
                    .and_then(literal)
                    .and_then(|value| value.text().map(str::to_owned))
                    .unwrap_or_else(|| match plot_type {
                        PlotType::BarColor => "Bar Color".to_owned(),
                        _ => "Background".to_owned(),
                    });
                self.plots.push(PlotSlot {
                    series: PlotSeries {
                        title,
                        color: color.unwrap_or(Color32::TRANSPARENT),
                        width: 1.0,
                        plot_type,
                        pattern: egui_charts::model::LineStyle::Solid,
                        visible: true,
                        track_price: false,
                        track_price_solid: false,
                        offset: 0,
                        values: vec![f64::NAN; self.bars.len()],
                        colors: vec![None; self.bars.len()],
                    },
                });
                self.plot_at.insert(id, index);
                index
            }
        };
        if let Some(color) = color {
            let plot = &mut self.plots[slot].series;
            plot.values[self.bar] = self.bars.get(self.bar).map_or(0.0, |bar| bar.close);
            plot.colors[self.bar] = Some(color);
        }
        Value::Na
    }

    /// `table.new`, `table.cell`, `table.clear` and `table.delete`. The library table holds text
    /// and cell colours, so spanning only shares its start cell's colours.
    fn table_call(&mut self, method: &str, args: &[Arg]) -> Value {
        let index = |value: Option<Value>| {
            value
                .and_then(|value| value.number())
                .unwrap_or(0.0)
                .max(0.0) as usize
        };
        if method == "new" {
            let object = Value::object_of(HashMap::new());
            let Some(shared) = object.object() else {
                return Value::Na;
            };
            let columns = index(args.get(1).map(|arg| self.eval(&arg.value)));
            let rows = index(args.get(2).map(|arg| self.eval(&arg.value)));
            let mut fields = value::lock_object(&shared);
            fields.insert("__columns".into(), Value::Num(columns as f64));
            fields.insert("__rows".into(), Value::Num(rows as f64));
            // `table.new(position.top_right, columns, rows, ...)`: the first argument is a member
            // path, which is read as text so it is not evaluated as an unknown name.
            let position = args
                .first()
                .and_then(|arg| dotted(&arg.value))
                .unwrap_or_else(|| "position.top_right".into());
            fields.insert("__position".into(), Value::Str(position));
            drop(fields);
            self.tables.push(shared);
            return object;
        }
        let Some(shared) = args
            .first()
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.object())
        else {
            return Value::Na;
        };
        match method {
            "cell" => {
                let column = index(args.get(1).map(|arg| self.eval(&arg.value)));
                let row = index(args.get(2).map(|arg| self.eval(&arg.value)));
                let text = named(args, "text")
                    .or_else(|| args.get(3))
                    .map(|arg| self.eval(&arg.value))
                    .filter(|value| !matches!(value, Value::Na))
                    .map(|value| builtins::display(&value));
                let key = format!("c:{column}:{row}");
                // `table.cell(table_id, column, row, text, width, height, text_color, ..., bgcolor)`.
                let mut style = |name: &str, at: usize| {
                    named(args, name)
                        .or_else(|| args.get(at))
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.color())
                };
                let bg = style("bgcolor", 10);
                let ink = style("text_color", 6);
                let mut fields = value::lock_object(&shared);
                match text {
                    Some(text) if !text.is_empty() => {
                        fields.insert(key, Value::Str(text));
                        for (prefix, color) in [("b", bg), ("t", ink)] {
                            let key = format!("{prefix}:{column}:{row}");
                            match color {
                                Some(color) => {
                                    fields.insert(key, Value::Color(color));
                                }
                                None => {
                                    fields.remove(&key);
                                }
                            }
                        }
                    }
                    _ => {
                        fields.remove(&key);
                        fields.remove(&format!("b:{column}:{row}"));
                        fields.remove(&format!("t:{column}:{row}"));
                    }
                }
            }
            "clear" => {
                let mut bound = |at: usize, fallback: usize| {
                    args.get(at)
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.number())
                        .map(|value| value.max(0.0) as usize)
                        .unwrap_or(fallback)
                };
                let (columns, rows) = {
                    let fields = value::lock_object(&shared);
                    (
                        fields.get("__columns").and_then(Value::number).unwrap_or(0.0) as usize,
                        fields.get("__rows").and_then(Value::number).unwrap_or(0.0) as usize,
                    )
                };
                let start_column = bound(1, 0);
                let start_row = bound(2, 0);
                let end_column = bound(3, columns.saturating_sub(1));
                let end_row = bound(4, rows.saturating_sub(1));
                let mut fields = value::lock_object(&shared);
                let cleared: Vec<String> = fields
                    .keys()
                    .filter(|key| {
                        key.strip_prefix("c:")
                            .and_then(|rest| {
                                let (column, row) = rest.split_once(':')?;
                                Some((
                                    column.parse::<usize>().ok()?,
                                    row.parse::<usize>().ok()?,
                                ))
                            })
                            .is_some_and(|(column, row)| {
                                column >= start_column
                                    && column <= end_column
                                    && row >= start_row
                                    && row <= end_row
                            })
                    })
                    .cloned()
                    .collect();
                for key in cleared {
                    fields.remove(&key);
                }
            }
            "delete" => {
                value::lock_object(&shared).insert("__deleted".into(), Value::Bool(true));
            }
            // `merge_cells`: the library table draws each cell, so the range only shares its
            // start cell's colours; `build_tables` spreads them.
            "merge_cells" => {
                let mut bound = |at: usize| {
                    args.get(at)
                        .map(|arg| self.eval(&arg.value))
                        .and_then(|value| value.number())
                        .map(|value| value.max(0.0) as usize)
                        .unwrap_or(0)
                };
                let key = format!(
                    "m:{}:{}:{}:{}",
                    bound(1),
                    bound(2),
                    bound(3),
                    bound(4)
                );
                value::lock_object(&shared).insert(key, Value::Bool(true));
            }
            // The remaining setters only change a layout the library table does not have.
            _ => {}
        }
        Value::Na
    }

    fn fill(&mut self, id: NodeId, args: &[Arg]) -> Value {
        if self.fill_at.contains_key(&id) || args.len() < 2 {
            return Value::Na;
        }
        let first = self.eval(&args[0].value);
        let second = self.eval(&args[1].value);
        let (Value::Plot(first), Value::Plot(second)) = (first, second) else {
            return Value::Na;
        };
        let fallback = self.plots[first].series.color;
        let color = positional(args, "color", 2)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color())
            .unwrap_or_else(|| {
                Color32::from_rgba_unmultiplied(fallback.r(), fallback.g(), fallback.b(), 40)
            });
        self.fills.push(FillSlot {
            first,
            second,
            color,
        });
        self.fill_at.insert(id, self.fills.len() - 1);
        Value::Na
    }

    fn plotshape(&mut self, id: NodeId, args: &[Arg], name: &str) -> Value {
        let Some(first) = args.first() else {
            return Value::Na;
        };
        let condition = self.eval(&first.value);
        let above = positional(args, "location", 3)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(str::to_owned))
            .is_none_or(|location| !location.contains("below"));
        let color = positional(args, "color", 4)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let offset = positional(args, "offset", 5)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.number())
            .unwrap_or(0.0) as isize;
        let text_color = positional(args, "textcolor", 7)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let font_size = positional(args, "size", 9)
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(shape_font_size))
            .unwrap_or(12.0);
        let slot = match self.shape_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let color = color.unwrap_or_else(|| plot_color(self.shapes.len()));
                let argument = |arg: Option<&Arg>, evaluate: &mut Self| {
                    arg.map(|arg| evaluate.eval(&arg.value))
                        .and_then(|value| value.text().map(str::to_owned))
                        .unwrap_or_default()
                };
                let text = argument(positional(args, "text", 6), self);
                // `plotchar` spells its glyph as a third positional, `plotshape` its style.
                let glyph = if name == "plotchar" {
                    argument(positional(args, "char", 2), self)
                } else {
                    argument(named(args, "char"), self)
                };
                let text = if !text.is_empty() {
                    text
                } else if !glyph.is_empty() {
                    glyph
                } else {
                    shape_glyph(&argument(positional(args, "style", 2), self))
                };
                self.shapes.push(ShapeSlot {
                    above,
                    color,
                    text,
                    text_color,
                    font_size,
                    offset,
                    marks: Vec::new(),
                });
                self.shape_at.insert(id, self.shapes.len() - 1);
                self.shapes.len() - 1
            }
        };
        let fires = match &condition {
            Value::Bool(flag) => *flag,
            Value::Num(number) => *number != 0.0,
            _ => false,
        };
        if fires {
            let Some(bar) = self.bars.get(self.bar) else {
                return Value::Na;
            };
            let value = condition
                .number()
                .filter(|number| *number > 1.0)
                .unwrap_or(if above { bar.high } else { bar.low });
            if let Some(target) = self.bar.checked_add_signed(self.shapes[slot].offset)
                && target < self.bars.len()
            {
                self.shapes[slot].marks.push((target, value));
            }
        }
        Value::Na
    }

    fn unsupported(&mut self, line: usize, name: &str) -> Value {
        let message = format!("`{name}` is not supported by the interpreter");
        if self.reported.insert((line, message.clone())) {
            self.diagnostics.push(Diagnostic { line, message });
        }
        Value::Na
    }
}

fn binary(op: &str, left: Value, right: Value) -> Value {
    if op == "+" && (matches!(left, Value::Str(_)) || matches!(right, Value::Str(_))) {
        let text = |value: &Value| match value {
            Value::Str(text) => text.clone(),
            Value::Na => String::new(),
            other => match other.number() {
                Some(number) => number.to_string(),
                None => String::new(),
            },
        };
        return Value::Str(format!("{}{}", text(&left), text(&right)));
    }
    if matches!(op, "==" | "!=") {
        let equal = match (&left, &right) {
            (Value::Str(first), Value::Str(second)) => first == second,
            (Value::Bool(first), Value::Bool(second)) => first == second,
            (Value::Color(first), Value::Color(second)) => first == second,
            _ => match (left.number(), right.number()) {
                (Some(first), Some(second)) => (first - second).abs() <= 1e-9,
                _ => matches!((&left, &right), (Value::Na, Value::Na)),
            },
        };
        return Value::Bool(if op == "==" { equal } else { !equal });
    }
    let (Some(left), Some(right)) = (left.number(), right.number()) else {
        return Value::Na;
    };
    match op {
        "+" => Value::Num(left + right),
        "-" => Value::Num(left - right),
        "*" => Value::Num(left * right),
        "/" => {
            if right == 0.0 {
                Value::Na
            } else {
                Value::Num(left / right)
            }
        }
        "%" => Value::Num(left % right),
        "<" => Value::Bool(left < right),
        "<=" => Value::Bool(left <= right),
        ">" => Value::Bool(left > right),
        ">=" => Value::Bool(left >= right),
        _ => Value::Na,
    }
}

/// The dotted path of a member expression, e.g. `color.new`.
fn dotted(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Member { target, name } => Some(format!("{}.{}", dotted(target)?, name)),
        _ => None,
    }
}

/// `"0930-1100"` as minutes from midnight; a window that ends before it starts wraps midnight.
fn session_window(session: &str) -> Option<(u32, u32)> {
    fn minutes(text: &str) -> Option<u32> {
        let digits: String = text.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 4 {
            return None;
        }
        let hour: u32 = digits.get(0..2)?.parse().ok()?;
        let minute: u32 = digits.get(2..4)?.parse().ok()?;
        Some(hour * 60 + minute)
    }
    let (start, end) = session.split_once('-')?;
    Some((minutes(start)?, minutes(end)?))
}

fn session_contains(
    session: &str,
    time: &chrono::DateTime<egui_charts::model::TimezoneId>,
) -> bool {
    let Some((start, end)) = session_window(session) else {
        return true;
    };
    let minutes = time.hour() * 60 + time.minute();
    if start == end {
        true
    } else if start < end {
        minutes >= start && minutes < end
    } else {
        minutes >= start || minutes < end
    }
}

fn drawing_kind(namespace: &str) -> Option<DrawingKind> {
    match namespace {
        "line" => Some(DrawingKind::Line),
        "label" => Some(DrawingKind::Label),
        "box" => Some(DrawingKind::Box),
        "linefill" => Some(DrawingKind::Fill),
        _ => None,
    }
}

fn kind_name(kind: DrawingKind) -> &'static str {
    match kind {
        DrawingKind::Line => "line",
        DrawingKind::Label => "label",
        DrawingKind::Box => "box",
        DrawingKind::Fill => "linefill",
    }
}

/// Label and box text: `na` is empty and anything else prints as Pine prints it.
fn drawing_text(value: &Value) -> String {
    match value {
        Value::Na => String::new(),
        Value::Str(text) => text.clone(),
        other => builtins::display(other),
    }
}

/// `size.tiny` and the strings an input offers for a label or box font.
fn font_size_of(value: &Value) -> f32 {
    match value.number() {
        Some(number) => number as f32,
        None => match value.text() {
            Some("tiny") => 8.0,
            Some("small") => 10.0,
            Some("large") => 16.0,
            Some("huge") => 20.0,
            _ => 12.0,
        },
    }
}

fn set_field(object: &value::ObjectRef, key: &str, value: Value) {
    value::lock_object(object).insert(key.to_owned(), value);
}

/// One finished `line`, `label` or `box` as the chart places it, or `None` once deleted.
fn drawing_output(object: &value::ObjectRef) -> Option<DrawingOutput> {
    let fields = value::lock_object(object);
    if fields
        .get("__deleted")
        .and_then(Value::flag)
        .unwrap_or(false)
    {
        return None;
    }
    let kind = match fields.get("__kind").and_then(Value::text)? {
        "line" => DrawingKind::Line,
        "label" => DrawingKind::Label,
        "box" => DrawingKind::Box,
        "linefill" => DrawingKind::Fill,
        _ => return None,
    };
    let number = |key: &str| fields.get(key).and_then(Value::number).unwrap_or(f64::NAN);
    let colour = |key: &str| fields.get(key).and_then(Value::color);
    let text = |key: &str| {
        fields
            .get(key)
            .and_then(Value::text)
            .unwrap_or_default()
            .to_owned()
    };
    let style = fields
        .get("style")
        .and_then(Value::text)
        .unwrap_or("style_solid");
    let style = if style.contains("dashed") {
        egui_charts::model::LineStyle::Dashed
    } else if style.contains("dotted") {
        egui_charts::model::LineStyle::Dotted
    } else {
        egui_charts::model::LineStyle::Solid
    };
    let first = (number("x1") as f32, number("y1"));
    let second = (number("x2") as f32, number("y2"));
    let font_size = number("font_size") as f32;
    Some(match kind {
        DrawingKind::Line => DrawingOutput {
            kind,
            first,
            second,
            quad: None,
            text: String::new(),
            color: colour("color").unwrap_or(Color32::WHITE),
            text_color: None,
            fill_color: None,
            width: number("width") as f32,
            style,
            font_size,
        },
        DrawingKind::Label => DrawingOutput {
            kind,
            first,
            second: first,
            quad: None,
            text: text("text"),
            color: colour("text_color").unwrap_or(Color32::WHITE),
            text_color: colour("text_color"),
            fill_color: colour("color"),
            width: 1.0,
            style,
            font_size,
        },
        DrawingKind::Box => DrawingOutput {
            kind,
            first,
            second,
            quad: None,
            text: text("text"),
            color: colour("border_color").unwrap_or(Color32::WHITE),
            text_color: colour("text_color"),
            fill_color: colour("fill_color"),
            width: number("border_width") as f32,
            style,
            font_size,
        },
        DrawingKind::Fill => {
            // The fill's corners come from the two lines it was made between.
            let ends = |key: &str| -> Option<((f32, f64), (f32, f64))> {
                let object = fields.get(key).and_then(Value::object)?;
                let line = value::lock_object(&object);
                if line.get("__deleted").and_then(Value::flag).unwrap_or(false) {
                    return None;
                }
                let number = |key: &str| line.get(key).and_then(Value::number);
                Some((
                    (number("x1")? as f32, number("y1")?),
                    (number("x2")? as f32, number("y2")?),
                ))
            };
            let (first_line, second_line) = (ends("line1")?, ends("line2")?);
            DrawingOutput {
                kind,
                first: first_line.0,
                second: first_line.1,
                quad: Some([second_line.1, second_line.0]),
                text: String::new(),
                color: Color32::TRANSPARENT,
                text_color: None,
                fill_color: colour("color"),
                width: 0.0,
                style,
                font_size,
            }
        }
    })
}

fn named<'a>(args: &'a [Arg], name: &str) -> Option<&'a Arg> {
    args.iter().find(|arg| arg.name.as_deref() == Some(name))
}

/// A named argument, or the positional one at `at` when the script passed it that way.
fn positional<'a>(args: &'a [Arg], name: &str, at: usize) -> Option<&'a Arg> {
    named(args, name).or_else(|| args.get(at).filter(|arg| arg.name.is_none()))
}

/// Puts named `ta.*` arguments where the implementation reads them, so their written order does
/// not matter.
fn ta_arguments(name: &str, args: &[Arg], values: Vec<Value>) -> Vec<Value> {
    if args.iter().all(|arg| arg.name.is_none()) {
        return values;
    }
    let Some(order) = builtins::ta_parameters(name) else {
        return values;
    };
    let mut slots: Vec<Option<Value>> = vec![None; order.len()];
    let mut next = 0;
    for (arg, value) in args.iter().zip(values) {
        let named_at = arg
            .name
            .as_deref()
            .and_then(|name| order.iter().position(|param| *param == name));
        let at = match named_at {
            Some(at) => at,
            None => {
                while next < slots.len() && slots[next].is_some() {
                    next += 1;
                }
                if next >= slots.len() {
                    continue;
                }
                next += 1;
                next - 1
            }
        };
        slots[at] = Some(value);
    }
    let mut values: Vec<Value> = slots.into_iter().map(|slot| slot.unwrap_or(Value::Na)).collect();
    // `ta.pivothigh(leftbars=…, rightbars=…)` leaves out the source, which the two-argument form
    // takes from the bar instead.
    if matches!(name, "pivothigh" | "pivotlow") {
        while values.len() > 2 && values.first().is_some_and(|value| matches!(value, Value::Na)) {
            values.remove(0);
        }
    }
    values
}

/// The caption of a plot: `title=` or the second positional argument.
fn title_of(args: &[Arg]) -> Option<String> {
    named(args, "title")
        .or_else(|| args.get(1).filter(|arg| arg.name.is_none()))
        .map(|arg| &arg.value)
        .and_then(literal)
        .and_then(|value| value.text().map(str::to_owned))
}

fn plot_type(style: &str) -> PlotType {
    // The text a `plot.style_*` constant reads as.
    match style.trim_start_matches("style_") {
        "stepline" | "stepline_diamond" => PlotType::StepLine,
        "histogram" => PlotType::Histogram,
        "columns" => PlotType::Columns,
        "area" | "areabr" => PlotType::Area,
        "circles" => PlotType::Circles,
        "cross" => PlotType::Crosses,
        "triangleup" => PlotType::TriangleUp,
        "triangledown" => PlotType::TriangleDown,
        _ => PlotType::Line,
    }
}

/// A price at the decimals its tick size implies, for `str.tostring(price, format.mintick)`.
fn mintick_text(value: f64, mintick: f64) -> String {
    let decimals = if mintick > 0.0 {
        (-mintick.log10()).round().clamp(0.0, 12.0) as usize
    } else {
        2
    };
    format!("{value:.decimals$}")
}

/// The namespaced name a v3/v4 script's bare builtin call means: `sma` is `ta.sma`, `abs` is
/// `math.abs`, `color(...)` is `color.new` and `security(...)` is `request.security`.
fn legacy_name(name: &str) -> Option<String> {
    match name {
        "na" => return None,
        "color" => return Some("color.new".to_owned()),
        "security" => return Some("request.security".to_owned()),
        "tostring" => return Some("str.tostring".to_owned()),
        "tonumber" => return Some("str.tonumber".to_owned()),
        // v4's `sum(source, length)` is the rolling sum; `max`, `min` and `avg` are variadic.
        "sum" => return Some("ta.sum".to_owned()),
        _ => {}
    }
    if name.contains('.') {
        return None;
    }
    if builtins::is_math(name) {
        return Some(format!("math.{name}"));
    }
    if builtins::is_ta(name) {
        return Some(format!("ta.{name}"));
    }
    None
}

/// The chart interval as Pine spells it: `1`, `15`, `60`, `240`, `D`, `W`, `M`, with a leading
/// count only when it is more than one.
fn pine_period(timeframe: egui_charts::model::Timeframe) -> String {
    // A trade-based interval has no time period; its chart label is the closest answer.
    if timeframe.duration_ms() == 0 {
        return timeframe.as_str().into_owned();
    }
    let unit = |count: i64, letter: &str| {
        if count == 1 {
            letter.to_owned()
        } else {
            format!("{count}{letter}")
        }
    };
    let ms = timeframe.duration_ms();
    if ms > 0 && ms % 2_592_000_000 == 0 {
        unit(ms / 2_592_000_000, "M")
    } else if ms > 0 && ms % 604_800_000 == 0 {
        unit(ms / 604_800_000, "W")
    } else if ms > 0 && ms % 86_400_000 == 0 {
        unit(ms / 86_400_000, "D")
    } else if ms >= 60_000 {
        (ms / 60_000).to_string()
    } else {
        (ms / 1_000).to_string() + "S"
    }
}

/// The count `timeframe.period` carries: minutes for an intraday interval, days or more above.
fn pine_multiplier(timeframe: egui_charts::model::Timeframe) -> f64 {
    let ms = timeframe.duration_ms();
    if ms == 0 {
        return 1.0;
    }
    if ms > 0 && ms % 2_592_000_000 == 0 {
        (ms / 2_592_000_000) as f64
    } else if ms > 0 && ms % 604_800_000 == 0 {
        (ms / 604_800_000) as f64
    } else if ms > 0 && ms % 86_400_000 == 0 {
        (ms / 86_400_000) as f64
    } else if ms >= 60_000 {
        (ms / 60_000) as f64
    } else {
        (ms / 1_000) as f64
    }
}

/// The glyph a `shape.*` style draws: the chart paints study `plotshape` marks as text labels.
fn shape_glyph(style: &str) -> String {
    match style {
        "triangleup" => "▲",
        "triangledown" => "▼",
        "circle" => "●",
        "cross" | "xcross" => "✕",
        "diamond" => "◆",
        "square" => "■",
        "flag" => "⚑",
        "arrowup" => "↑",
        "arrowdown" => "↓",
        // `shape.labelup` and `shape.labeldown` draw their `text=` alone.
        "labelup" | "labeldown" => "",
        _ => "✕",
    }
    .to_owned()
}

/// Pine's `size.*` constants as label point sizes.
fn shape_font_size(size: &str) -> f32 {
    match size {
        "tiny" => 8.0,
        "small" => 10.0,
        "large" => 16.0,
        "huge" => 24.0,
        _ => 12.0,
    }
}

/// Whether `text` matches a Pine regular expression; an invalid pattern matches nothing.
fn matches_regex(text: &str, pattern: Option<&str>) -> bool {
    pattern
        .and_then(|pattern| regex::Regex::new(pattern).ok())
        .is_some_and(|regex| regex.is_match(text))
}

/// The dash pattern a `plot.linestyle_*` constant names.
fn line_pattern(value: &Value) -> egui_charts::model::LineStyle {
    use egui_charts::model::LineStyle;
    match value.text() {
        Some("linestyle_dotted") => LineStyle::Dotted,
        Some("linestyle_dashed") => LineStyle::Dashed,
        _ => LineStyle::Solid,
    }
}

/// A stable colour for a plot that names none.
fn table_position(name: Option<&str>) -> StudyTablePosition {
    match name {
        Some("position.top_left") => StudyTablePosition::TopLeft,
        Some("position.top_center") => StudyTablePosition::TopCenter,
        Some("position.top_right") => StudyTablePosition::TopRight,
        Some("position.middle_left") => StudyTablePosition::MiddleLeft,
        Some("position.middle_center") => StudyTablePosition::MiddleCenter,
        Some("position.middle_right") => StudyTablePosition::MiddleRight,
        Some("position.bottom_left") => StudyTablePosition::BottomLeft,
        Some("position.bottom_center") => StudyTablePosition::BottomCenter,
        _ => StudyTablePosition::BottomRight,
    }
}

fn plot_color(index: usize) -> Color32 {
    const PALETTE: [Color32; 6] = [
        egui_charts::studies::palette::BLUE,
        egui_charts::studies::palette::WARM_ORANGE,
        egui_charts::studies::palette::GREEN,
        egui_charts::studies::palette::PURPLE,
        egui_charts::studies::palette::CYAN,
        egui_charts::studies::palette::YELLOW,
    ];
    PALETTE[index % PALETTE.len()]
}

/// How chart bars group into a higher timeframe, in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bucket {
    Fixed(i64),
    Day,
    Week,
    Month,
}

impl Bucket {
    /// Calendar periods follow the selected chart timezone.
    fn key_in_timezone(self, millis: i64, timezone: &egui_charts::model::TimezoneId) -> i64 {
        if matches!(self, Self::Fixed(_)) {
            return self.key(millis);
        }
        let local = chrono::DateTime::from_timestamp_millis(millis)
            .map(|time| time.with_timezone(timezone).naive_local().and_utc().timestamp_millis())
            .unwrap_or(millis);
        self.key(local)
    }
    /// A Pine timeframe string: minutes by default, or `S`, `H`, `D`, `W`, `M` units.
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let split = text
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(text.len());
        let (digits, unit) = text.split_at(split);
        let multiplier: i64 = if digits.is_empty() {
            1
        } else {
            digits.parse().ok()?
        };
        match unit.to_ascii_uppercase().as_str() {
            "" => Some(Self::Fixed(multiplier * 60_000)),
            "S" => Some(Self::Fixed(multiplier * 1_000)),
            "H" => Some(Self::Fixed(multiplier * 3_600_000)),
            "D" => Some(Self::Day),
            "W" => Some(Self::Week),
            "M" => Some(Self::Month),
            _ => None,
        }
    }

    fn chart(timeframe: egui_charts::model::Timeframe) -> Self {
        match timeframe.duration_ms() {
            0 | 86_400_000 => Self::Day,
            604_800_000 => Self::Week,
            millis if millis >= 2_592_000_000 => Self::Month,
            millis => Self::Fixed(millis),
        }
    }

    /// The interval a data request can name, or `None` when it cannot be expressed.
    fn timeframe(self) -> Option<egui_charts::model::Timeframe> {
        use egui_charts::model::Timeframe;
        use std::num::NonZeroU64;
        Some(match self {
            Self::Fixed(millis) => {
                if millis % 1_000 != 0 {
                    return None;
                }
                let seconds = u64::try_from(millis / 1_000).ok()?;
                let minutes = seconds / 60;
                if seconds % 60 == 0
                    && let Some(timeframe) = Timeframe::from_resolution(&minutes.to_string())
                {
                    timeframe
                } else {
                    Timeframe::Custom(NonZeroU64::new(seconds)?)
                }
            }
            Self::Day => Timeframe::Day1,
            Self::Week => Timeframe::Week1,
            Self::Month => Timeframe::Month1,
        })
    }

    fn duration_ms(self) -> i64 {
        match self {
            Self::Fixed(millis) => millis,
            Self::Day => 86_400_000,
            Self::Week => 604_800_000,
            Self::Month => 2_592_000_000,
        }
    }

    /// The bar group a timestamp belongs to.
    fn key(self, millis: i64) -> i64 {
        match self {
            Self::Fixed(span) => millis.div_euclid(span),
            Self::Day => millis.div_euclid(86_400_000),
            // The epoch began on a Thursday; three days later is a Monday.
            Self::Week => (millis.div_euclid(86_400_000) + 3).div_euclid(7),
            Self::Month => chrono::DateTime::from_timestamp_millis(millis)
                .map(|time| time.year() as i64 * 12 + time.month0() as i64)
                .unwrap_or(0),
        }
    }

    /// The open time of the group `key` names.
    fn start(self, key: i64) -> i64 {
        match self {
            Self::Fixed(span) => key * span,
            Self::Day => key * 86_400_000,
            Self::Week => (key * 7 - 3) * 86_400_000,
            Self::Month => month_first(key),
        }
    }

    /// The close time of the group `key` names.
    fn close(self, key: i64) -> i64 {
        match self {
            Self::Month => month_first(key + 1),
            _ => self.start(key) + self.duration_ms(),
        }
    }
}

/// The first instant of the month `year * 12 + month0`.
fn month_first(key: i64) -> i64 {
    let year = key.div_euclid(12) as i32;
    let month = (key.rem_euclid(12) as u32) + 1;
    chrono::NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc().timestamp_millis())
        .unwrap_or(0)
}

/// The number of `bars` closed by `now`: a bar closes when the next one opens, or after its
/// interval when it is the last one.
fn closed_bars(bars: &[Bar], timeframe: egui_charts::model::Timeframe, now: i64) -> usize {
    bars.iter()
        .enumerate()
        .take_while(|(index, bar)| {
            let close = bars.get(index + 1).map_or_else(
                || bar.time.timestamp_millis() + timeframe.duration_ms(),
                |next| next.time.timestamp_millis(),
            );
            close <= now
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pine_engine::parser::Parser;
    use chrono::{TimeZone, Utc};

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(at, close)| {
                Bar::new(
                    Utc.with_ymd_and_hms(2024, 1, 1, 0, at as u32 % 60, 0)
                        .unwrap(),
                    *close,
                    close + 1.0,
                    close - 1.0,
                    *close,
                    1.0,
                )
            })
            .collect()
    }

    fn run(source: &str, closes: &[f64]) -> RunOutput {
        run_bars(source, bars(closes))
    }

    fn run_bars(source: &str, bars: Vec<Bar>) -> RunOutput {
        let program = Parser::parse(source).unwrap();
        let context = Context::default();
        Evaluator::new(&program, &bars, &context, &HashMap::new(), &[]).run()
    }

    /// Minute bars from midnight, close `at + 1` and high `close + 1`.
    fn minute_bars(count: usize) -> Vec<Bar> {
        (0..count)
            .map(|at| {
                let close = at as f64 + 1.0;
                Bar::new(
                    Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
                        + chrono::TimeDelta::minutes(at as i64),
                    close - 0.5,
                    close + 1.0,
                    close - 1.0,
                    close,
                    1.0,
                )
            })
            .collect()
    }

    #[test]
    fn sma_warms_up_and_plots_one_column() {
        let output = run(
            "//@version=6\nindicator(\"T\")\nv = ta.sma(close, 3)\nplot(v)\n",
            &[1.0, 2.0, 3.0, 4.0, 5.0],
        );
        assert_eq!(output.plots.len(), 1);
        let values = &output.plots[0].values;
        assert!(values[0].is_nan());
        assert!(values[1].is_nan());
        assert!((values[2] - 2.0).abs() < 1e-9);
        assert!((values[3] - 3.0).abs() < 1e-9);
        assert!((values[4] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn series_index_reads_past_bars_and_na_before_them() {
        let output = run("indicator(\"T\")\nplot(close[1])\n", &[1.0, 2.0, 3.0]);
        let values = &output.plots[0].values;
        assert!(values[0].is_nan());
        assert!((values[1] - 1.0).abs() < 1e-9);
        assert!((values[2] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn var_keeps_its_first_value() {
        let output = run(
            "indicator(\"T\")\nvar first = close\nplot(first)\n",
            &[1.0, 2.0, 3.0],
        );
        assert_eq!(output.plots[0].values, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn if_expression_yields_the_assigned_value() {
        let output = run(
            "indicator(\"T\")\nx = if close > 2\n    y = 10\nelse\n    y = 20\nplot(x)\n",
            &[1.0, 3.0],
        );
        assert_eq!(output.plots[0].values, [20.0, 10.0]);
    }

    #[test]
    fn fills_join_two_plots() {
        let output = run(
            "indicator(\"T\")\na = plot(close)\nb = plot(open)\nfill(a, b, color=color.new(color.red, 80))\n",
            &[1.0, 2.0],
        );
        assert_eq!(output.fills.len(), 1);
        assert_eq!(output.fills[0].color.a(), 51);
    }

    #[test]
    fn user_function_runs_inside_the_caller_bar() {
        let output = run(
            "indicator(\"T\")\ndouble(x) =>\n    y = x * 2\n    y\nplot(double(close))\n",
            &[1.0, 2.0],
        );
        assert_eq!(output.plots[0].values, [2.0, 4.0]);
    }

    #[test]
    fn unsupported_calls_report_their_line_once() {
        let output = run(
            "indicator(\"T\")\nplot(math.unknown(close))\n",
            &[1.0, 2.0, 3.0],
        );
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(output.diagnostics[0].line, 2);
    }

    #[test]
    fn request_security_reads_the_last_closed_higher_bar() {
        let output = run_bars(
            "indicator(\"T\")\nplot(request.security(syminfo.tickerid, \"60\", close))\n",
            minute_bars(62),
        );
        let values = &output.plots[0].values;
        assert!(values[59].is_nan(), "the first hour has not closed yet");
        assert_eq!(values[60], 60.0);
        assert_eq!(values[61], 62.0, "the newest bar uses the developing hour");
    }

    #[test]
    fn request_security_indexes_the_higher_series() {
        let output = run_bars(
            "indicator(\"T\")\nplot(request.security(syminfo.tickerid, \"60\", high[1]))\n",
            minute_bars(180),
        );
        let values = &output.plots[0].values;
        assert!(values[119].is_nan());
        // At 02:00 the first hour is behind the second one, whose `high[1]` is its own high.
        assert_eq!(values[120], 61.0);
    }

    #[test]
    fn security_latest_bar_has_developing_and_previous_daily_highs() {
        use chrono::TimeZone;
        let bars: Vec<_> = [(2, 10.0), (2, 12.0), (3, 15.0), (3, 14.0)]
            .into_iter()
            .enumerate()
            .map(|(i, (day, high))| {
                let time = chrono::Utc.with_ymd_and_hms(2024, 1, day, 12 + (i % 2) as u32, 0, 0).unwrap();
                Bar::new(time, high - 1.0, high, high - 1.0, high, 1.0)
            })
            .collect();
        let source = SecurityData { symbol: "TEST".into(), timeframe: egui_charts::model::Timeframe::Day1,
            bars: [(2, 12.0), (3, 15.0)].into_iter().map(|(day, high)| Bar::new(chrono::Utc.with_ymd_and_hms(2024, 1, day, 0, 0, 0).unwrap(), high, high, high, high, 1.0)).collect() };
        let script = crate::pine_engine::Script::parse("//@version=4\nstudy(\"Daily fixture\")\nplot(security(syminfo.tickerid, 'D', high))\nplot(security(syminfo.tickerid, 'D', high[1]))\n").unwrap();
        let context = Context { symbol: "TEST".into(), ..Context::default() };
        let output = script.run(&bars, &context, &[source]);
        assert_eq!(output.plots[0].values.last().copied(), Some(15.0));
        assert_eq!(output.plots[1].values.last().copied(), Some(12.0));
    }

    #[test]
    fn lookahead_on_reads_the_higher_bar_from_its_first_chart_bar() {
        let output = run_bars(
            "indicator(\"T\")\nplot(request.security(syminfo.tickerid, \"60\", close, lookahead = barmerge.lookahead_on))\nplot(request.security(syminfo.tickerid, \"60\", high[1], barmerge.gaps_off, barmerge.lookahead_on))\n",
            minute_bars(180),
        );
        let close = &output.plots[0].values;
        assert_eq!(close[0], 60.0);
        assert_eq!(close[60], 120.0);
        let previous_high = &output.plots[1].values;
        assert!(previous_high[0].is_nan());
        // At 01:00 the whole first hour is known, so its `high[1]` is that hour's high.
        assert_eq!(previous_high[60], 61.0);
        assert_eq!(previous_high[120], 121.0);
    }

    #[test]
    fn request_security_keeps_ta_state_on_the_higher_series() {
        let output = run_bars(
            "indicator(\"T\")\nplot(request.security(syminfo.tickerid, \"60\", ta.sma(close, 2)))\n",
            minute_bars(180),
        );
        let values = &output.plots[0].values;
        // Hour closes are 60 and 120, so the two-bar average is 90 at 02:00.
        assert_eq!(values[120], 90.0);
    }

    #[test]
    fn another_symbol_is_na() {
        let output = run(
            "indicator(\"T\")\nplot(request.security(\"OTHER\", \"60\", close))\n",
            &[1.0, 2.0],
        );
        assert!(output.plots[0].values.iter().all(|value| value.is_nan()));
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }

    #[test]
    fn lower_timeframe_series_holds_the_chart_bar() {
        let output = run(
            "indicator(\"T\")\nltH = request.security_lower_tf(syminfo.tickerid, \"1\", [high])\nplot(array.get(array.get(ltH, 0), 0))\n",
            &[1.0, 2.0],
        );
        assert_eq!(output.plots[0].values, [2.0, 3.0]);
    }

    #[test]
    fn another_symbol_security_reads_its_closed_bars() {
        let program = Parser::parse(
            "indicator(\"T\")\nother = request.security(\"OTHER\", \"5\", close)\nplot(other)\n",
        )
        .unwrap();
        let context = Context::default();
        let chart = minute_bars(11);
        let source = SecurityData {
            symbol: "OTHER".into(),
            timeframe: egui_charts::model::Timeframe::Min5,
            bars: [100.0, 200.0, 300.0]
                .iter()
                .enumerate()
                .map(|(at, close)| {
                    Bar::new(
                        Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
                            + chrono::TimeDelta::minutes(at as i64 * 5),
                        *close,
                        *close,
                        *close,
                        *close,
                        1.0,
                    )
                })
                .collect(),
        };
        let output = Evaluator::new(&program, &chart, &context, &HashMap::new(), &[source]).run();
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let values = &output.plots[0].values;
        assert!(values[..5].iter().all(|value| value.is_nan()));
        assert_eq!(values[5..10], [100.0; 5]);
        assert_eq!(values[10], 200.0);
        assert_eq!(
            output.security_requests,
            [("OTHER".to_owned(), egui_charts::model::Timeframe::Min5)]
        );
    }

    #[test]
    fn another_symbol_lookahead_takes_the_open_bar_whole() {
        let program = Parser::parse(
            "indicator(\"T\")\nother = request.security(\"OTHER\", \"5\", close, lookahead=barmerge.lookahead_on)\nplot(other)\n",
        )
        .unwrap();
        let context = Context::default();
        let chart = minute_bars(11);
        let source = SecurityData {
            symbol: "OTHER".into(),
            timeframe: egui_charts::model::Timeframe::Min5,
            bars: [100.0, 200.0, 300.0]
                .iter()
                .enumerate()
                .map(|(at, close)| {
                    Bar::new(
                        Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()
                            + chrono::TimeDelta::minutes(at as i64 * 5),
                        *close,
                        *close,
                        *close,
                        *close,
                        1.0,
                    )
                })
                .collect(),
        };
        let output = Evaluator::new(&program, &chart, &context, &HashMap::new(), &[source]).run();
        let values = &output.plots[0].values;
        assert_eq!(values[..5], [100.0; 5]);
        assert_eq!(values[5..10], [200.0; 5]);
        assert_eq!(values[10], 300.0);
    }

    #[test]
    fn tables_collect_their_cells() {
        let output = run(
            "indicator(\"T\")\nvar table tbl = table.new(position.top_right, 2, 2)\ntable.cell(tbl, 0, 0, \"A\")\ntable.cell(tbl, 1, 0, \"B\")\ntable.cell(tbl, 0, 1, \"1\", bgcolor=color.red, text_color=color.white)\nplot(close)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values, [1.0]);
        let table = output.tables.first().expect("a table with cells");
        assert_eq!(table.columns, ["A", "B"]);
        assert_eq!(
            table.rows,
            vec![vec![
                StudyCell::Text("1".into()),
                StudyCell::Text(String::new())
            ]]
        );
        assert_eq!(table.cell_bg, vec![vec![Some(Color32::RED), None]]);
        assert_eq!(table.cell_text, vec![vec![Some(Color32::WHITE), None]]);
        assert_eq!(table.position, StudyTablePosition::TopRight);
    }

    #[test]
    fn every_table_is_kept_richest_first() {
        let output = run(
            "indicator(\"T\")\nvar table small = table.new(position.bottom_center, 1, 1)\ntable.cell(small, 0, 0, \"wm\")\nvar table big = table.new(position.top_right, 2, 2)\ntable.cell(big, 0, 0, \"A\")\ntable.cell(big, 1, 0, \"B\")\ntable.cell(big, 0, 1, \"1\")\ntable.cell(big, 1, 1, \"2\")\nplot(close)\n",
            &[1.0],
        );
        assert_eq!(output.tables.len(), 2);
        assert_eq!(output.tables[0].columns, ["A", "B"]);
        assert_eq!(output.tables[1].rows, vec![vec![StudyCell::Text("wm".into())]]);
    }

    #[test]
    fn plot_style_constants_choose_the_plot_type() {
        let output = run(
            "indicator(\"T\")\nplot(close, style=plot.style_histogram)\nplot(close, style=plot.style_circles)\nplot(close, style=plot.style_linebr)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].plot_type, PlotType::Histogram);
        assert_eq!(output.plots[1].plot_type, PlotType::Circles);
        assert_eq!(output.plots[2].plot_type, PlotType::Line);
    }

    #[test]
    fn merged_cells_share_their_colours() {
        let output = run(
            "indicator(\"T\")\nvar table t = table.new(position.top_right, 2, 2)\ntable.cell(t, 0, 0, \"A\")\ntable.cell(t, 1, 0, \"B\")\ntable.cell(t, 0, 1, \"one\", bgcolor=color.red, text_color=color.white)\ntable.merge_cells(t, 0, 1, 1, 1)\nplot(close)\n",
            &[1.0],
        );
        let table = output.tables.first().expect("a table with cells");
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.cell_bg[0][1], Some(Color32::RED));
        assert_eq!(table.cell_text[0][1], Some(Color32::WHITE));
    }

    #[test]
    fn clock_calls_use_their_timezone() {
        let program = Parser::parse(
            "indicator(\"T\")\nplot(hour(time, \"America/New_York\"))\nplot(dayofweek(time, \"America/New_York\"))\nplot(year(time, \"+05:00\"))\n",
        )
        .unwrap();
        let bars = vec![Bar::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 3, 0, 0).unwrap(),
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
        )];
        let context = Context::default();
        let output = Evaluator::new(&program, &bars, &context, &HashMap::new(), &[]).run();
        assert_eq!(output.plots[0].values[0], 22.0);
        // Sunday 22:00 in New York.
        assert_eq!(output.plots[1].values[0], 1.0);
        assert_eq!(output.plots[2].values[0], 2024.0);
    }

    #[test]
    fn timestamp_is_read_in_its_timezone() {
        let output = run(
            "indicator(\"T\")\nplot(timestamp(\"America/New_York\", 2024, 1, 1, 0, 0, 0))\n",
            &[1.0],
        );
        let expected = Utc
            .with_ymd_and_hms(2024, 1, 1, 5, 0, 0)
            .unwrap()
            .timestamp_millis() as f64;
        assert_eq!(output.plots[0].values[0], expected);
    }

    #[test]
    fn time_close_of_a_timeframe_is_its_bucket_close() {
        let bars = vec![Bar::new(
            Utc.with_ymd_and_hms(2024, 1, 1, 3, 7, 0).unwrap(),
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
        )];
        let output = run_bars(
            "indicator(\"T\")\nplot(time_close(\"60\"))\nplot(time(\"60\"))\n",
            bars,
        );
        let open = Utc
            .with_ymd_and_hms(2024, 1, 1, 3, 0, 0)
            .unwrap()
            .timestamp_millis() as f64;
        assert_eq!(output.plots[0].values[0], open + 3_600_000.0);
        assert_eq!(output.plots[1].values[0], open);
    }

    #[test]
    fn barcolor_and_bgcolor_paint_their_bars() {
        let output = run(
            "indicator(\"T\")\nbarcolor(close > 1 ? color.red : na)\nbgcolor(close > 1 ? color.blue : na)\n",
            &[1.0, 2.0],
        );
        let bars = &output.plots[0];
        assert_eq!(bars.plot_type, PlotType::BarColor);
        assert!(bars.values[0].is_nan());
        assert_eq!(bars.colors[1], Some(Color32::RED));
        assert!(bars.values[1].is_finite());
        assert_eq!(output.plots[1].plot_type, PlotType::BackgroundColor);
        assert_eq!(output.plots[1].colors[1], Some(Color32::BLUE));
    }

    #[test]
    fn pivot_high_answers_when_the_right_bars_confirm_it() {
        let output = run(
            "indicator(\"T\")\nplot(ta.pivothigh(1, 1))\n",
            &[1.0, 2.0, 3.0, 2.0, 1.0],
        );
        let values = &output.plots[0].values;
        assert!(values[..3].iter().all(|value| value.is_nan()));
        assert_eq!(values[3], 4.0);
        assert!(values[4].is_nan());
    }

    #[test]
    fn rising_needs_a_strictly_rising_window() {
        let output = run(
            "indicator(\"T\")\nplot(ta.rising(close, 2) ? 1 : 0)\n",
            &[1.0, 2.0, 3.0, 2.0],
        );
        assert_eq!(output.plots[0].values, [0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn highestbars_counts_back_from_the_last_bar() {
        let output = run(
            "indicator(\"T\")\nplot(ta.highestbars(close, 3))\n",
            &[1.0, 5.0, 3.0],
        );
        let values = &output.plots[0].values;
        assert!(values[..2].iter().all(|value| value.is_nan()));
        assert_eq!(values[2], -1.0);
    }

    #[test]
    fn linreg_follows_a_straight_line() {
        let output = run(
            "indicator(\"T\")\nplot(ta.linreg(close, 2))\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        let values = &output.plots[0].values;
        assert!(values[0].is_nan());
        assert!((values[3] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn vwap_weights_by_volume_and_median_sorts_the_window() {
        let output = run(
            "indicator(\"T\")\nplot(ta.vwap(close))\nplot(ta.median(close, 3))\n",
            &[1.0, 2.0, 3.0],
        );
        let vwap = &output.plots[0].values;
        assert!((vwap[2] - 2.0).abs() < 1e-9);
        let median = &output.plots[1].values;
        assert!(median[..2].iter().all(|value| value.is_nan()));
        assert_eq!(median[2], 2.0);
    }

    #[test]
    fn cci_cmo_and_wpr_follow_their_formulas() {
        let output = run(
            "indicator(\"T\")\nplot(ta.cci(close, 2))\nplot(ta.cmo(close, 2))\nplot(ta.wpr(2))\n",
            &[1.0, 2.0, 3.0, 2.0],
        );
        assert!((output.plots[0].values[2] - 66.666_666).abs() < 1e-4);
        assert_eq!(output.plots[1].values[2], 100.0);
        assert_eq!(output.plots[1].values[3], 0.0);
        // At the third bar the last two highs are 3 and 4, the lows 1 and 2, close 3.
        assert!((output.plots[2].values[2] + 33.333_332).abs() < 1e-4);
    }

    #[test]
    fn correlation_is_one_for_a_scaled_copy() {
        let output = run(
            "indicator(\"T\")\nplot(ta.correlation(close, close * 2, 3))\n",
            &[1.0, 2.0, 4.0, 8.0],
        );
        assert!(output.plots[0].values[0].is_nan());
        assert!((output.plots[0].values[2] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mfi_and_cmf_weight_by_the_bar_flow() {
        let output = run(
            "indicator(\"T\")\nplot(ta.mfi(2))\nplot(ta.cmf(1))\n",
            &[3.0, 2.0, 1.0],
        );
        assert!(output.plots[0].values[0].is_nan());
        assert_eq!(output.plots[0].values[1], 0.0);
        // The helper bars close at the middle of the range, so the flow multiplier is zero.
        assert_eq!(output.plots[1].values[2], 0.0);
        let output = run_bars(
            "indicator(\"T\")\nplot(ta.cmf(1))\n",
            vec![Bar::new(
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                1.0,
                2.0,
                0.0,
                2.0,
                1.0,
            )],
        );
        assert_eq!(output.plots[0].values[0], 1.0);
    }

    #[test]
    fn string_position_and_trim() {
        let output = run(
            "indicator(\"T\")\nplot(str.pos(\"a b\", \" \"))\nplot(str.length(str.trim(\"  a  \")))\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.plots[1].values[0], 1.0);
    }

    #[test]
    fn round_to_mintick_uses_the_instrument_step() {
        let output = run(
            "indicator(\"T\")\nplot(math.round_to_mintick(1.2345))\n",
            &[1.0],
        );
        assert!((output.plots[0].values[0] - 1.23).abs() < 1e-9);
    }

    #[test]
    fn mintick_text_uses_the_tick_decimals() {
        assert_eq!(mintick_text(1.23456, 0.01), "1.23");
        assert_eq!(mintick_text(1.23456, 0.001), "1.235");
    }

    #[test]
    fn tostring_with_format_mintick_rounds_to_the_tick() {
        let output = run(
            "indicator(\"T\")\nplot(str.length(str.tostring(1.23456, format.mintick)))\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values[0], 4.0);
    }

    #[test]
    fn display_none_keeps_the_plot_out_of_the_chart() {
        let output = run(
            "indicator(\"T\")\nplot(1, display=display.none)\nplot(2)\n",
            &[1.0],
        );
        assert!(!output.plots[0].visible);
        assert!(output.plots[1].visible);
    }

    #[test]
    fn plotshape_keeps_the_glyph_and_the_char() {
        let output = run(
            "indicator(\"T\")\nplotshape(true, style=shape.triangleup, location=location.belowbar)\nplotchar(true, char=\"A\", text=\"Mon\")\n",
            &[1.0],
        );
        assert_eq!(output.shapes[0].text, "▲");
        assert_eq!(output.shapes[1].text, "Mon");
        assert_eq!(output.shapes[1].font_size, 12.0);
    }

    #[test]
    fn plot_linestyle_sets_the_dash_pattern() {
        let output = run(
            "indicator(\"T\")\nplot(close, linestyle=plot.linestyle_dotted)\nhline(1, linestyle=plot.linestyle_dashed)\n",
            &[1.0],
        );
        use egui_charts::model::LineStyle;
        assert_eq!(output.plots[0].pattern, LineStyle::Dotted);
        assert_eq!(output.plots[1].pattern, LineStyle::Dashed);
    }

    #[test]
    fn chart_colours_and_pointvalue_are_defined() {
        let output = run(
            "indicator(\"T\")\nplot(color.a(chart.bg_color))\nplot(color.r(chart.fg_color))\nplot(syminfo.pointvalue)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values[0], 0.0);
        assert_eq!(output.plots[1].values[0], 209.0);
        assert_eq!(output.plots[2].values[0], 1.0);
    }

    #[test]
    fn syminfo_ticker_reads_the_ticker_of_a_symbol() {
        let output = run(
            "indicator(\"T\")\nplot(syminfo.ticker(\"BINANCE:BTCUSDT\") == \"BTCUSDT\" ? 1 : 0)\nplot(syminfo.ticker(\"SOLUSDT\") == \"SOLUSDT\" ? 1 : 0)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.plots[1].values[0], 1.0);
    }

    #[test]
    fn the_true_range_is_a_property_and_a_call() {
        let output = run(
            "indicator(\"T\")\nplot(ta.tr)\nplot(ta.tr(true))\n",
            &[1.0, 2.0, 10.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values, [2.0, 2.0, 9.0]);
        assert_eq!(output.plots[1].values, [2.0, 2.0, 9.0]);
    }

    #[test]
    fn max_bars_back_is_accepted() {
        let output = run(
            "indicator(\"T\")\nmax_bars_back(close, 1000)\nplot(close)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }

    #[test]
    fn log_calls_do_not_report() {
        let output = run(
            "indicator(\"T\")\nlog.info(\"x {0}\", close)\nplot(close)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }

    #[test]
    fn for_loop_with_step_updates_an_outer_accumulator() {
        let output = run(
            "indicator(\"T\")\ntotal = 0.0\nfor i = 1 to 5 by 2\n    total := total + i\nplot(total)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [9.0]);
    }

    #[test]
    fn for_in_walks_an_array() {
        let output = run(
            "indicator(\"T\")\nvalues = array.from(1, 2, 3)\ntotal = 0.0\nfor value in values\n    total := total + value\nplot(total)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [6.0]);
    }

    #[test]
    fn for_in_with_two_names_gives_the_index_and_the_value() {
        let output = run(
            "indicator(\"T\")\nvalues = array.from(10, 20, 30)\ntotal = 0.0\nfor [i, value] in values\n    total := total + i * value\nplot(total)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [80.0]);
    }

    #[test]
    fn while_honours_break_and_continue() {
        let output = run(
            "indicator(\"T\")\nn = 0\nwhile n < 10\n    n := n + 1\n    if n == 3\n        continue\n    if n > 5\n        break\nplot(n)\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [6.0]);
    }

    #[test]
    fn rarer_calls_run_without_diagnostics() {
        let script = r#"indicator("Rare", overlay=true)
sess = input.session("0930-1600", "Session")
tf = input.timeframe("60", "TF")
sym = input.symbol("BINANCE:BTCUSDT", "Symbol")
col = input.color(color.red, "Colour")
txt = input.text_area("hello", "Text")
prc = input.price(100.5, "Price")
levels = ta.pivot_point_levels("Traditional", timeframe.change("D"))
plot(array.size(levels))
m = matrix.new<float>(2, 2, 1)
matrix.set(m, 0, 0, 4)
matrix.set(m, 1, 1, 7)
matrix.swap_rows(m, 0, 1)
plot(matrix.get(m, 0, 1))
matrix.add_row(m, 1, array.from(2, 3))
plot(matrix.rows(m))
removed = matrix.remove_col(m, 0)
plot(array.size(removed))
plot(matrix.rank(matrix.identity(3)))
var table t = table.new(position.bottom_left, 2, 1)
if barstate.islast
    table.cell(t, 0, 0, str.tostring(prc) + txt + sess + tf + sym)
    table.merge_cells(t, 0, 0, 1, 0)
    lb = label.new(bar_index, high, "hi")
    label.set_xy(lb, bar_index - 1, low)
plot(close)
"#;
        let output = run(script, &[1.0, 2.0, 3.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        // Pine's eleven pivot slots, even while the levels themselves are `na`.
        assert_eq!(output.plots[0].values[2], 11.0);
        assert_eq!(output.plots[1].values[2], 7.0);
        assert_eq!(output.plots[2].values[2], 3.0);
        assert_eq!(output.plots[3].values[2], 3.0);
        assert_eq!(output.plots[4].values[2], 3.0);
    }

    #[test]
    fn maps_and_chart_times_run_without_diagnostics() {
        let script = r#"indicator("Maps", overlay=true)
m = map.new<string, float>()
map.put(m, "a", close)
map.put(m, "b", high)
map.put(m, "a", low)
plot(map.get(m, "a"))
plot(map.size(m))
plot(map.contains(m, "b") ? 1 : 0)
keys = map.keys(m)
plot(array.size(keys))
map.remove(m, "a")
plot(map.size(m))
plot(chart.left_visible_bar_time > 0 ? 1 : 0)
plot(chart.right_visible_bar_time > 0 ? 1 : 0)
plot(str.length(str.format_time(time, "yyyy-MM-dd")))
"#;
        let output = run(script, &[1.0, 2.0, 3.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |index: usize| output.plots[index].values[2];
        // `map.put` replaces an existing key, and `remove` drops it.
        assert_eq!(at(0), 2.0);
        assert_eq!(at(1), 2.0);
        assert_eq!(at(2), 1.0);
        assert_eq!(at(3), 2.0);
        assert_eq!(at(4), 1.0);
        assert_eq!(at(5), 1.0);
        assert_eq!(at(6), 1.0);
        assert_eq!(at(7), 10.0);
    }

    #[test]
    fn drawing_details_and_loops_run_without_diagnostics() {
        let script = r#"indicator("Draw", overlay=true)
type Counter
    int value
method bump(Counter self, int step) =>
    self.value += step
    self.value
c = Counter.new(0)
bump(c, 2)
plot(c.value)
label.new(bar_index, high, "x", yloc=yloc.abovebar, textalign=text.align_left, size=size.normal, style=label.style_none)
bx = box.new(bar_index, high, bar_index + 1, low, border_style=line.style_dashed, text="hi", text_size=size.small, text_halign=text.align_right)
box.set_text_size(bx, size.tiny)
[a, b] = request.security(syminfo.tickerid, "60", [close, open])
plot(a + b)
total = 0
for i = 0 to 3 by 2
    total += i
plot(total)
j = 0
while j < 2
    j += 1
plot(j)
plot(str.length(str.tostring(#ff0000)))
"#;
        let output = run(script, &[1.0, 2.0, 3.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[2], 2.0);
    }

    #[test]
    fn string_and_array_details_run_without_diagnostics() {
        let script = r#"indicator("Str", overlay=true)
opt = input.string("a", "Mode", options=["a", "b"])
n = input.int(defval=3, title="N")
plot(str.length(str.upper(str.trim("  ab  "))))
plot(str.tonumber("-1.5e2"))
arr = array.new_float(3, 1)
array.set(arr, 1, 5)
plot(array.sum(arr))
plot(array.indexof(arr, 5))
plot(array.slice(arr, 1, 3).size())
plot(str.split("a,b,c", ",").size())
plot(array.includes(arr, 5) ? 1 : 0)
plot(str.length(str.replace_all("aaa", "a", "b")))
plot(str.match("abc", "b") ? 1 : 0)
plot(str.startswith("abc", "ab") ? 1 : 0)
plot(display=display.none)
"#;
        let output = run(script, &[1.0, 2.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }

    #[test]
    fn positional_plot_arguments_and_named_user_arguments_answer() {
        let script = r#"indicator("P", overlay=true)
plot(close, "c", color.red, 3)
f(a, b) => a - b
plot(f(b = 1, a = 5))
plotshape(close > 0, "s", shape.circle, location.belowbar, color.blue, 0, "t", color.white, true, size.tiny)
"#;
        let output = run(script, &[1.0, 2.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].color, Color32::RED);
        assert_eq!(output.plots[0].width, 3.0);
        assert_eq!(output.plots[1].values[1], 4.0);
        assert_eq!(output.shapes[0].color, Color32::BLUE);
        assert!(!output.shapes[0].above);
        assert_eq!(output.shapes[0].text, "t");
    }

    #[test]
    fn defaults_and_loops_in_user_functions_answer() {
        let script = r#"indicator("F")
f(a, b = 2) =>
    a + b
plot(f(1))
plot(f(1, 3))
plot(nz(na, 5))
values = array.from(10, 20)
total = 0
for [i, v] in values
    total += i + v
plot(total)
g(x) =>
    if x > 1
        x * 2
    else
        x
plot(g(2))
"#;
        let output = run(script, &[1.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 3.0);
        assert_eq!(output.plots[1].values[0], 4.0);
        assert_eq!(output.plots[2].values[0], 5.0);
        assert_eq!(output.plots[3].values[0], 31.0);
        assert_eq!(output.plots[4].values[0], 4.0);
    }

    #[test]
    fn numeric_edges_answer_as_pine_documents() {
        let output = run(
            "indicator(\"Edge\")\nplot(array.size(array.slice(array.from(1, 2, 3, 4), 1, 3)))\nplot(math.round(1.2345, 2))\nplot(math.round(2.5))\nplot(str.tonumber(\"5.5\"))\nplot(array.get(array.slice(array.from(1, 2, 3, 4), 1, 3), 1))\nplot(str.length(str.substring(\"abcdef\", 2)))\nplot(math.sign(-3))\nplot(str.tonumber(\"abc\"))\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |index: usize| output.plots[index].values[0];
        assert_eq!(at(0), 2.0);
        assert_eq!(at(1), 1.23);
        assert_eq!(at(2), 3.0);
        assert_eq!(at(3), 5.5);
        assert_eq!(at(4), 3.0);
        assert_eq!(at(5), 4.0);
        assert_eq!(at(6), -1.0);
        assert!(at(7).is_nan());
    }

    #[test]
    fn timeframe_change_and_smoothed_averages_answer() {
        let output = run(
            "indicator(\"T\")\nplot(timeframe.change(\"D\") ? 1 : 0)\nplot(ta.dema(close, 2))\nplot(ta.tema(close, 2))\nplot(ta.percentile_linear_interpolation(close, 3, 50))\nplot(ta.percentile_nearest_rank(close, 3, 100))\n",
            &[5.0, 5.0, 5.0, 5.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(output.plots[1].values[3], 5.0);
        assert_eq!(output.plots[2].values[3], 5.0);
        assert_eq!(output.plots[3].values[3], 5.0);
        assert_eq!(output.plots[4].values[3], 5.0);
    }

    #[test]
    fn session_and_time_helpers_run_without_diagnostics() {
        let script = r#"indicator("Time", overlay=true)
inSession = not na(time(timeframe.period, "0930-1600"))
plot(inSession ? 1 : 0)
plot(dayofweek(time) == dayofweek.monday ? 1 : 0)
plot(year(time) >= 2024 ? 1 : 0)
plot(hour(time) < 24 ? 1 : 0)
plot(timestamp("2024-01-01T00:00:00") > 0 ? 1 : 0)
plot(str.length(str.format_time(time, "HH:mm", "UTC")) > 0 ? 1 : 0)
plot(dayofweek.monday == 2 ? 1 : 0)
"#;
        let output = run(script, &[1.0, 2.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        // The bars are outside the 0930-1600 session, and start on a Monday.
        assert_eq!(output.plots[0].values[1], 0.0);
        assert_eq!(output.plots[1].values[1], 1.0);
        assert_eq!(output.plots[2].values[1], 1.0);
        assert_eq!(output.plots[6].values[1], 1.0);
    }

    #[test]
    fn a_feature_heavy_script_runs_without_diagnostics() {
        let script = r#"indicator("Sink", overlay=true)
n = input.int(10, minval=1)
src = input.source(close, "Source")
ma = ta.sma(src, n)
dev = ta.stdev(src, n)
upper = ma + 2 * dev
lower = ma - 2 * dev
p = plot(upper)
q = plot(lower)
fill(p, q, color.new(color.blue, 90))
hline(50, "lvl", color=color.gray, linestyle=hline.style_dashed)
plotchar(ta.crossover(src, ma), "Up", "^", location.belowbar, color=color.green)
barcolor(src > ma ? color.green : color.red)
rsiVal = ta.rsi(src, 14)
zone = switch
    rsiVal > 70 => "over"
    rsiVal < 30 => "under"
    => "mid"
var string lastText = ""
if barstate.islast
    lastText := str.format("RSI {0,number,#.##} in {1}", rsiVal, zone)
    label.new(bar_index, high, lastText, style=label.style_label_down, color=color.new(color.blue, 20))
ln = line.new(bar_index - 1, low, bar_index, high, extend=extend.right, style=line.style_dotted)
line.set_extend(ln, extend.none)
m = matrix.new<float>(2, 3)
matrix.set(m, 0, 0, 1.5)
matrix.fill(m, 9)
parts = str.split("a,b,c", ",")
plot(array.size(parts))
plot(str.tonumber(array.get(parts, 1)))
plot(matrix.get(m, 0, 0))
"#;
        let output = run(script, &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots.last().unwrap().values[4], 9.0);
    }

    #[test]
    fn pivot_levels_answer_in_pine_slot_order() {
        // One bar: open 10, high 11, low 9, close 10.
        let output = run(
            "indicator(\"T\")\nlevels = ta.pivot_point_levels(\"Traditional\", false, true)\nplot(array.get(levels, 0))\nplot(array.get(levels, 1))\nplot(array.get(levels, 2))\nplot(array.get(levels, 10))\nfib = ta.pivot_point_levels(\"Fibonacci\", false, true)\nplot(array.get(fib, 1))\nwoodie = ta.pivot_point_levels(\"Woodie\", false, true)\nplot(array.get(woodie, 0))\n",
            &[10.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 10.0);
        assert_eq!(output.plots[1].values[0], 11.0);
        assert_eq!(output.plots[2].values[0], 9.0);
        assert_eq!(output.plots[3].values[0], 5.0);
        assert!((output.plots[4].values[0] - 10.764).abs() < 1e-9);
        // Woodie has no developing levels.
        assert!(output.plots[5].values[0].is_nan());
    }

    #[test]
    fn a_typical_script_runs_without_diagnostics() {
        // Written flush left: a `\`-continued literal would drop the indentation Pine needs.
        let script = r#"indicator("T", overlay=true)
len = input.int(20, "Length", minval=1)
showTable = input.bool(true, "Table")
emaFast = ta.ema(close, len)
emaSlow = ta.ema(close, len * 2)
[macdLine, signalLine, hist] = ta.macd(close, 12, 26, 9)
[st, dir] = ta.supertrend(3.0, 10)
bull = ta.crossover(emaFast, emaSlow)
bear = ta.crossunder(emaFast, emaSlow)
plot(emaFast, "Fast", color=color.new(color.teal, 0), linewidth=2)
plot(emaSlow, "Slow", color=color.orange)
plot(hist, "Hist", style=plot.style_columns, color=hist >= 0 ? color.green : color.red)
plotshape(bull, "Bull", shape.triangleup, location.belowbar, color.green, size=size.small)
plotshape(bear, "Bear", shape.triangledown, location.abovebar, color.red)
bgcolor(dir > 0 ? color.new(color.green, 90) : color.new(color.red, 90))
type Setup
    float price
    int bars
var setups = array.new<Setup>()
if bull
    array.push(setups, Setup.new(close, bar_index))
total = 0.0
for setup in setups
    total += setup.price
if showTable and barstate.islast
    t = table.new(position.top_right, 2, 2, border_width=1)
    table.cell(t, 0, 0, "Total", bgcolor=color.gray, text_color=color.white)
    table.cell(t, 1, 0, str.tostring(total, format.mintick))
alertcondition(bull, "Bull", "Bull signal")
"#;
        let output = run(script, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        // `ta.ema` needs `len` bars, so only the first bars are still warming up.
        assert!(!output.plots.is_empty());
        assert!(output.candles.is_empty());
    }

    #[test]
    fn tuple_assignment_takes_the_values_a_function_answers_with() {
        let output = run(
            "indicator(\"T\")\npair() =>\n    [close, close * 2]\n[a, b] = pair()\nplot(a + b)\n",
            &[2.0],
        );
        assert_eq!(output.plots[0].values, [6.0]);
    }

    #[test]
    fn array_builtins_read_sort_and_measure() {
        let output = run(
            "indicator(\"T\")\nvalues = array.new_float(0)\narray.push(values, 3)\narray.push(values, 1)\narray.push(values, 2)\narray.sort(values)\nplot(array.get(values, 0))\nplot(array.sum(values))\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [1.0]);
        assert_eq!(output.plots[1].values, [6.0]);
    }

    #[test]
    fn a_chained_call_runs_its_receiver() {
        let output = run(
            "indicator(\"T\")\nplot(array.from(1, 2, 3).size())\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [3.0]);
    }

    #[test]
    fn str_format_time_renders_a_time_in_another_zone() {
        // 2024-01-02 03:04 UTC is 22:04 the day before in New York.
        let output = run(
            "indicator(\"T\")\nplot(str.length(str.format_time(1704164640000, 'HH:mm', 'America/New_York')))\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [5.0]);
    }

    #[test]
    fn a_user_type_holds_fields_and_answers_its_methods() {
        let output = run(
            "indicator(\"T\")\ntype Point\n    float x = 0\n    float y = 0\nmethod sum(Point p) => p.x + p.y\np = Point.new(1, 2)\np.y := 5\nplot(p.sum())\n",
            &[1.0],
        );
        assert_eq!(output.plots[0].values, [6.0]);
    }

    #[test]
    fn an_undeclared_receiver_does_not_call_a_user_method() {
        // `box` is a namespace, so `box.delete(1)` must not mean the script's own `delete`.
        let output = run(
            "indicator(\"T\")\ndelete(x) =>\n    x\nplot(box.delete(1))\n",
            &[1.0],
        );
        assert!(output.plots[0].values[0].is_nan());
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("box.delete")),
            "{:?}",
            output.diagnostics
        );
    }

    #[test]
    fn a_runaway_recursion_stops_at_the_work_budget() {
        let output = run(
            "indicator(\"T\")\nf(n) =>\n    n < 1 ? 1 : f(n - 1) + f(n - 1)\nplot(f(64))\n",
            &[1.0],
        );
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("execution limit exceeded")),
            "{:?}",
            output.diagnostics
        );
    }

    #[test]
    fn plotshape_marks_the_bar_two_bars_behind() {
        let output = run(
            "indicator(\"T\")\nplotshape(close > 2, location=location.abovebar, offset=-2)\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        assert_eq!(output.shapes.len(), 2);
        assert_eq!(output.shapes[0].bar, 0);
        assert_eq!(output.shapes[0].value, 4.0);
        assert!(output.shapes[0].above);
    }

    #[test]
    fn a_user_type_array_field_takes_pushed_drawings() {
        let output = run(
            "indicator(\"T\")\ntype Holder\n    box[] boxes\nh = Holder.new(array.new_box())\nh.boxes.push(box.new(0, high, 1, low))\nplot(h.boxes.size())\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.drawings.len(), 1);
    }

    #[test]
    fn a_user_type_array_field_with_angle_brackets_is_read() {
        let output = run(
            "indicator(\"T\")\ntype Holder\n    array<box> boxes\nh = Holder.new(array.new_box())\nh.boxes.push(box.new(0, high, 1, low))\nplot(h.boxes.size())\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.drawings.len(), 1);
    }

    #[test]
    fn a_method_local_takes_the_nested_object_field() {
        let output = run(
            "indicator(\"T\")\ntype Inner\n    array<box> boxes\ntype Outer\n    Inner inner\nmethod manage(Outer this) =>\n    Inner kz = this.inner\n    kz.boxes.push(box.new(0, high, 1, low))\n    plot(kz.boxes.size())\no = Outer.new(Inner.new(array.new_box()))\no.manage()\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.drawings.len(), 1);
    }

    #[test]
    fn a_var_array_of_helpers_is_managed_by_a_method() {
        let output = run(
            "indicator(\"T\")\ntype Inner\n    array<box> boxes\ntype Outer\n    Inner inner\n    bool use = true\ninit() =>\n    array<Outer> res = array.new<Outer>()\n    if true\n        res.push(Outer.new(Inner.new(array.new_box()), true))\n    res\nvar array<Outer> all = init()\nmethod manage(Outer this) =>\n    if this.use\n        this.inner.boxes.push(box.new(0, high, 1, low))\nfor [_, value] in all\n    manage(value)\nplot(close)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.drawings.len(), 1);
    }

    #[test]
    fn sessions_include_their_start_and_wrap_past_midnight() {
        let utc = egui_charts::model::TimezoneId::utc();
        let at = |hour: u32, minute: u32| -> chrono::DateTime<egui_charts::model::TimezoneId> {
            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                    .unwrap()
                    .and_hms_opt(hour, minute, 0)
                    .unwrap(),
                chrono::Utc,
            )
            .with_timezone(&utc)
        };
        assert!(session_contains("0930-1100", &at(9, 30)));
        assert!(session_contains("0930-1100", &at(10, 59)));
        assert!(!session_contains("0930-1100", &at(11, 0)));
        assert!(session_contains("2200-0200", &at(23, 0)));
        assert!(session_contains("2200-0200", &at(1, 0)));
        assert!(!session_contains("2200-0200", &at(12, 0)));
    }

    #[test]
    fn drawings_keep_their_coordinates_text_and_time() {
        let output = run(
            "indicator(\"T\", overlay=true)\nif barstate.islast\n    ln = line.new(0, low, bar_index, high, color=color.red, width=2, style=line.style_dashed)\n    line.set_xy2(ln, bar_index + 2, high + 1)\n    lb = label.new(bar_index, high, \"hi\", textcolor=color.green, size=size.large)\n    lb.set_text(\"bye\")\n    bx = box.new(0, high, bar_index, low, bgcolor=color.blue)\n    box.delete(bx)\n    tl = line.new(time, low, time, high, xloc=xloc.bar_time)\nplot(close)\n",
            &[10.0, 11.0, 12.0],
        );
        assert_eq!(output.drawings.len(), 3, "{:?}", output.diagnostics);
        let line = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Line && drawing.first.0 == 0.0)
            .unwrap();
        assert_eq!(line.first, (0.0, 11.0));
        assert_eq!(line.second, (4.0, 14.0));
        assert_eq!(line.style, egui_charts::model::LineStyle::Dashed);
        assert_eq!(line.width, 2.0);
        let label = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Label)
            .unwrap();
        assert_eq!(label.first, (2.0, 13.0));
        assert_eq!(label.text, "bye");
        assert_eq!(label.font_size, 16.0);
        let timed = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Line && drawing.first.0 == 2.0)
            .unwrap();
        assert_eq!(timed.second.0, 2.0);
    }

    #[test]
    fn series_history_indexing_answers() {
        let output = run(
            "indicator(\"T\")\nplot(close[1])\nplot(close[bar_index > 0 ? 2 : 0])\nx = close * 2\nplot(x[1])\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert!(output.plots[0].values[0].is_nan());
        assert_eq!(output.plots[0].values[3], 3.0);
        assert_eq!(output.plots[1].values[3], 2.0);
        assert_eq!(output.plots[2].values[3], 6.0);
    }

    #[test]
    fn an_if_that_returns_a_negative_number_is_a_block() {
        // The indented `-1` is the `if`'s body, not a continuation of the line above.
        let output = run(
            "indicator(\"T\")\nvalue = if close > 1\n    -1\nplot(value)\n",
            &[1.0, 3.0],
        );
        assert!(output.plots[0].values[0].is_nan());
        assert_eq!(output.plots[0].values[1], -1.0);
    }

    #[test]
    fn a_strategy_call_is_not_a_second_header() {
        // `strategy.entry(...)` starts with a header word but is a call, not a declaration.
        let output = run("strategy.entry(\"X\", true)\nplot(close)\n", &[1.0, 2.0]);
        assert_eq!(output.plots.len(), 1);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("strategy.entry")),
            "{:?}",
            output.diagnostics
        );
    }

    #[test]
    fn a_v4_script_calls_its_builtins_without_a_namespace() {
        let script = r#"indicator("Legacy")
fast = sma(close, 2)
plot(fast, "sma")
plot(abs(close - 5), "abs")
plot(color.red == color(red, 0) ? 1 : 0, "colour")
plot(red == color.red ? 1 : 0, "red")
plot(cum(1), "cum")
plot(iff(close > 2, 1, 0), "iff")
plot(tostring(7) == "7" ? 1 : 0, "tostring")
text = "a"
    + "b"
    + "c"
plot(str.length(text), "joined")
"#;
        let output = run(script, &[1.0, 2.0, 3.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[2]
        };
        assert_eq!(at("sma"), 2.5);
        assert_eq!(at("abs"), 2.0);
        assert_eq!(at("colour"), 1.0);
        assert_eq!(at("red"), 1.0);
        assert_eq!(at("cum"), 3.0);
        assert_eq!(at("iff"), 1.0);
        assert_eq!(at("tostring"), 1.0);
        assert_eq!(at("joined"), 3.0);
    }

    #[test]
    fn a_for_loop_counts_down_when_from_is_above_to() {
        let output = run(
            "indicator(\"T\")\ntotal = 0\nfor i = 5 to 1\n    total += i\nplot(total, \"down\")\nsteps = 0\nfor i = 10 to 0 by 2\n    steps += 1\nplot(steps, \"steps\")\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[0]
        };
        assert_eq!(at("down"), 15.0);
        assert_eq!(at("steps"), 6.0);
    }

    #[test]
    fn tuple_returns_multi_value_switch_and_trailing_commas_run() {
        let script = r#"indicator("Odds5")
[macdLine, signalLine, histLine] = ta.macd(close, 12, 26, 9)
plot(macdLine, "macd")
[bbMiddle, bbUpper, bbLower] = ta.bb(close, 2, 2)
plot(bbMiddle, "middle")
[supertrend, direction] = ta.supertrend(3, 10)
plot(direction, "dir")
grade = "B"
value = switch grade
    "A", "B" => 1
    "C" => 2
    => 3
plot(value, "switch")
n = 1 +
    2 +
    3
plot(n, "n")
plot(close, "trailing", color=color.new(color.teal, 0), linewidth=2,)
"#;
        let output = run(script, &[10.0, 11.0, 12.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[2]
        };
        assert_eq!(at("switch"), 1.0);
        assert_eq!(at("n"), 6.0);
        assert!(at("middle").is_finite());
    }

    #[test]
    fn methods_enums_and_recursion_run_without_diagnostics() {
        let script = r#"indicator("Odds4")
type Point
    float x
    float y
method length(Point self) =>
    math.sqrt(self.x * self.x + self.y * self.y)
enum Side
    long = "long"
    short = "short"
float fact(float n) =>
    n <= 1 ? 1 : n * fact(n - 1)
p = Point.new(3, 4)
plot(p.length(), "length")
varip int seen = 0
seen += 1
plot(seen, "seen")
plot(fact(5), "fact")
side = Side.long
plot(side == "long" ? 1 : 0, "side")
plot(str.tostring(Side.short) == "short" ? 1 : 0, "short")
"#;
        let output = run(script, &[1.0, 2.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[1]
        };
        assert_eq!(at("length"), 5.0);
        assert_eq!(at("seen"), 2.0);
        assert_eq!(at("fact"), 120.0);
        assert_eq!(at("side"), 1.0);
        assert_eq!(at("short"), 1.0);
    }

    #[test]
    fn array_and_map_iteration_and_colour_channels_run_without_diagnostics() {
        let script = r#"indicator("Odds3")
values = array.from(10, 20, 30)
total = 0.0
for [index, value] in values
    total += value * (index + 1)
plot(total, "indexed")
entries = map.new<string, float>()
map.put(entries, "a", 1)
map.put(entries, "b", 2)
sum = 0.0
for [key, value] in entries
    sum += value
plot(sum, "summed")
plot(array.size(map.keys(entries)), "keys")
plot(array.size(map.values(entries)), "values")
base = color.new(color.red, 20)
plot(color.r(base), "r")
plot(color.g(base), "g")
plot(color.b(base), "b")
plot(color.a(base), "a")
tone = color.from_gradient(0.5, 0, 1, color.black, color.white)
plot(color.t(color.new(tone, 50)), "t")
plot(array.range(values), "range")
plot(array.max(values), "max")
plot(math.max(1, 2, 3, 4), "mmath")
plot(ta.change(close) != 0 or bar_index == 0 ? 1 : 0, "change")
p1 = plot(close, "fast")
p2 = plot(open, "slow")
fill(p1, p2, color=color.new(color.blue, 90), title="between")
"#;
        let output = run(script, &[10.0, 11.0, 12.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[2]
        };
        assert_eq!(at("indexed"), 140.0);
        assert_eq!(at("summed"), 3.0);
        assert_eq!(at("range"), 20.0);
        assert_eq!(at("t"), 50.0);
        assert_eq!(output.fills.len(), 1);
    }

    #[test]
    fn user_types_maps_and_method_calls_run_without_diagnostics() {
        let script = r#"indicator("Odds2", overlay=true)
type Point
    float x
    float y
    string label = "p"
p = Point.new(1, 2)
p2 = p.copy()
p2.x := 5
plot(p.x, "x")
plot(p2.x, "x2")
plot(p.label == "p" ? 1 : 0, "label")
m = matrix.new<float>(2, 2, 0)
m.set(0, 0, 3)
m.set(1, 1, 4)
plot(m.get(0, 0), "m00")
plot(m.det(), "det")
arr = array.from(3, 1, 2)
arr.push(4)
plot(arr.size(), "size")
plot(arr.get(1), "get")
arr.sort()
plot(arr.get(0), "sorted")
vals = map.new<string, float>()
map.put(vals, "a", 1)
plot(map.get(vals, "a"), "map")
plot(map.contains(vals, "a") ? 1 : 0, "has")
sum = 0.0
for i = 0 to 2
    sum += close[i]
plot(sum, "sum")
n = 0
while n < 3
    n += 1
plot(n, "while")
l = label.new(bar_index, high, "hi", color=color.red)
plot(label.get_text(l) == "hi" ? 1 : 0, "text")
ln = line.new(0, low, 0, high, extend=extend.both)
plot(line.get_price(ln, 0) > 0 ? 1 : 0, "price")
plot(str.split("a,b", ",").size(), "split")
plot(array.from(1, 2).avg(), "avg")
"#;
        let output = run(script, &[10.0, 11.0, 12.0, 13.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[3]
        };
        assert_eq!(at("x"), 1.0);
        assert_eq!(at("x2"), 5.0);
        assert_eq!(at("det"), 12.0);
        assert_eq!(at("sum"), 36.0);
        assert_eq!(at("while"), 3.0);
        assert_eq!(at("split"), 2.0);
    }

    #[test]
    fn the_text_helpers_and_alert_calls_run_without_diagnostics() {
        let script = r##"indicator("Odds", overlay=true)
alertcondition(ta.crossover(close, open), "Cross", "message")
if ta.crossover(close, open)
    alert("up", alert.freq_once_per_bar_close)
runtime.log("x")
plot(close, "c", color=color.new(color.red, 0), linewidth=2, style=plot.style_line, display=display.all, editable=true)
hline(50, "lvl", color=color.gray, linestyle=hline.style_dotted, extend=extend.right, display=display.none)
bgcolor(close > open ? color.new(color.green, 90) : na, title="bg")
barcolor(color.blue, title="bar")
plotshape(close > open, "S", shape.circle, location.belowbar, color.green, size=size.tiny, text="x", textcolor=color.white, editable=false)
plotchar(close, "char", "A", location.top, color.green, size=size.tiny)
plotcandle(open, high, low, close, "candles", color.green, color.red, wickcolor=color.gray, bordercolor=color.black)
parts = str.split("a,b,c", ",")
plot(array.size(parts), "parts")
plot(str.length(str.replace_all("a-b", "-", "+")), "replaced")
plot(str.length(str.repeat("x", 3)), "repeated")
plot(str.length(str.format_time(timenow, "{0,date,yyyy}", "UTC")))
plot(str.length(str.tostring(3.14159, "#.##")))
plot(str.length(str.format_number(1234.5, format.volume)))
plot(str.tonumber(timeframe.period), "period")
plot(str.length(str.tostring(syminfo.ticker)))
plot(request.financial("BINANCE:BTCUSDT", "total_revenue", barmerge.gaps_off, barmerge.lookahead_off) == 0 ? 0 : 1)
"##;
        let output = run(script, &[10.0, 11.0, 12.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[2]
        };
        assert_eq!(at("parts"), 3.0);
        assert_eq!(at("replaced"), 3.0);
        assert_eq!(at("repeated"), 3.0);
        // The chart runs on one-minute bars, so `timeframe.period` is the number `1`.
        assert_eq!(at("period"), 1.0);
    }

    #[test]
    fn rarer_script_idioms_run_without_diagnostics() {
        let script = r#"indicator("Idioms", overlay=true)
grade = input.string("A", "Grade", options=["A", "B"])
tf = input.timeframe("60")
since = input.time(timestamp("2024-01-01T00:00:00"))
src = switch grade
    "A" => close
    "B" => open
    => hl2
[otherOpen, otherHigh] = request.security(syminfo.tickerid, tf, [open, high], lookahead=barmerge.lookahead_off, gaps=barmerge.gaps_off)
ts = timestamp(2024, 1, 1, 0, 0)
var array<label> olds = array.new<label>()
array.push(olds, label.new(bar_index, high, "x"))
if barstate.islastconfirmedhistory and array.size(olds) > 0
    for old in olds
        old.delete()
    array.clear(olds)
b = box.new(0, high, 1, low)
plot(b.get_top())
plot(str.length(str.format("{0,date,yyyy-MM-dd}", ts)))
plot(ts > 0 ? 1 : 0)
plot(time(since) >= 0 ? 1 : 0)
plot(math.random(0, 1, 42) >= 0 ? 1 : 0)
plot(src == close ? 1 : 0)
plot(barstate.islastconfirmedhistory ? 1 : 0)
"#;
        let output = run(script, &[10.0, 11.0, 12.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[1].values[2], 10.0);
        assert_eq!(output.plots[6].values[2], 1.0);
        // Every label the loop deleted is gone from the drawings.
        assert!(
            output
                .drawings
                .iter()
                .all(|drawing| drawing.kind != DrawingKind::Label)
        );
    }

    #[test]
    fn a_statement_may_continue_on_an_indented_line() {
        let output = run(
            "indicator(\"T\")\nx = 1 +\n    2\nplot(x)\nplot(math.max(close,\n    open))\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 3.0);
        assert_eq!(output.plots[1].values[0], 1.0);
    }

    #[test]
    fn math_sum_sums_the_window_and_avg_averages_its_arguments() {
        let output = run(
            "indicator(\"T\")\nplot(math.sum(close, 3))\nplot(math.avg(1, 2, 3, 6))\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        // `math.sum` is a rolling sum, not a sum of its arguments.
        assert!(output.plots[0].values[1].is_nan());
        assert_eq!(output.plots[0].values[3], 9.0);
        assert_eq!(output.plots[1].values[3], 3.0);
    }

    #[test]
    fn tables_and_drawing_setters_run_without_diagnostics() {
        let script = r#"indicator("Misc", overlay=true)
t = table.new(position.top_right, 3, 2, bgcolor=color.new(color.black, 50), border_width=1, border_color=color.gray, frame_color=color.red, frame_width=2)
if barstate.islast
    table.cell(t, 0, 0, "A", bgcolor=color.blue, text_color=color.white, text_size=size.small)
    table.cell(t, 1, 0, str.format("{0} {1,number,#.##}", "x", 1.234))
    table.merge_cells(t, 0, 1, 2, 1)
    table.set_position(t, position.bottom_left)
    table.set_bgcolor(t, color.new(color.navy, 80))
    table.set_border_color(t, color.gray)
    table.set_border_width(t, 2)
    table.cell_set_text_color(t, 0, 0, color.yellow)
    table.cell_set_bgcolor(t, 0, 0, color.olive)
    table.cell_set_text(t, 1, 1, "upd")
    table.cell_set_text_size(t, 1, 1, size.large)
l = label.new(bar_index, high, "x")
label.set_style(l, label.style_label_left)
label.set_textcolor(l, color.red)
label.set_size(l, size.huge)
label.set_textalign(l, text.align_center)
label.set_tooltip(l, "tip")
ln = line.new(0, low, 1, high)
line.set_color(ln, color.red)
line.set_width(ln, 2)
line.set_style(ln, line.style_dashed)
line.set_extend(ln, extend.both)
bx = box.new(0, high, 1, low)
box.set_bgcolor(bx, color.blue)
box.set_border_color(bx, color.red)
box.set_border_width(bx, 2)
box.set_border_style(bx, line.style_dotted)
box.set_text(bx, "t")
box.set_text_color(bx, color.white)
box.set_text_size(bx, size.small)
box.set_text_halign(bx, text.align_left)
plot(close)
"#;
        let output = run(script, &[10.0, 11.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let line = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Line)
            .unwrap();
        assert_eq!(line.color, Color32::RED);
        assert_eq!(line.style, egui_charts::model::LineStyle::Dashed);
        let rectangle = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Box)
            .unwrap();
        assert_eq!(rectangle.fill_color, Some(Color32::BLUE));
        assert_eq!(rectangle.text, "t");
    }

    #[test]
    fn inputs_answer_their_declared_defaults() {
        let output = run(
            "indicator(\"T\")\nlen = input.int(3, \"Len\")\nsrc = input.source(low, \"Source\")\nplot(ta.sma(src, len))\nplot(input.float(2.5, \"F\"))\nplot(input.string(\"x\", \"S\") == \"x\" ? 1 : 0)\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        // `ta.sma` of the last three lows, which the fed source names.
        assert_eq!(output.plots[0].values[3], 2.0);
        assert_eq!(output.plots[1].values[3], 2.5);
        assert_eq!(output.plots[2].values[3], 1.0);
    }

    #[test]
    fn a_zone_and_label_script_runs_without_diagnostics() {
        let script = r##"indicator("Sink2", overlay=true)
len = input.int(14, "Len", minval=1, maxval=200)
src = input.source(close, "Source")
tf = input.timeframe("240", "Higher TF")
grade = input.string("A", "Grade", options=["A", "B", "C"])
hline(0.0, "Zero", color=color.new(color.gray, 50), linestyle=hline.style_dotted)
atr = ta.atr(len)
basis = ta.sma(src, len)
dev = ta.stdev(src, len)
upper = basis + dev * 2.0
lower = basis - dev * 2.0
p1 = plot(upper, "Upper", color=color.new(color.blue, 60))
p2 = plot(lower, "Lower", color=color.new(color.red, 60))
fill(p1, p2, color=color.new(color.gray, 90))
[hi, lo] = request.security(syminfo.tickerid, tf, [ta.highest(high, len), ta.lowest(low, len)])
var array<box> zones = array.new<box>()
var array<label> marks = array.new<label>()
if array.size(zones) > 20
    box.delete(array.shift(zones))
    label.delete(array.shift(marks))
bull = ta.crossover(src, basis)
if bull
    b = box.new(bar_index - len, upper, bar_index, lower, border_color=color.green, bgcolor=color.new(color.green, 85), text="BUY " + grade, text_size=size.tiny, text_color=color.white, border_width=2)
    array.push(zones, b)
    l = label.new(bar_index, low, str.tostring(atr, "#.##"), style=label.style_label_up, color=color.new(color.teal, 20), textcolor=color.white, size=size.small)
    array.push(marks, l)
plotshape(ta.crossunder(src, basis), "Sell", shape.triangledown, location.abovebar, color.red, size=size.small, text="S")
bgcolor(ta.highestbars(high, len) == 0 ? color.new(color.red, 90) : na)
plot(color.from_gradient(atr, 0, ta.highest(atr, 100), color.green, color.red))
values = array.from(3, 1, 2)
array.insert(values, 1, 9)
array.remove(values, 2)
array.unshift(values, 7)
arr2 = array.concat(values, array.from(1))
array.fill(arr2, 0, 0, 2)
plot(array.sum(arr2), "sum")
plot(str.length(array.join(values, "-")), "joined")
plot(str.length(str.tostring(array.avg(values), format.mintick)), "formatted")
plot(array.every(values, (v) => v > 0) ? 1 : 0, "all")
plot(array.some(values, (v) => v == 9) ? 1 : 0, "any")
"##;
        let output = run(
            script,
            &[
                1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0,
                7.0, 8.0, 9.0, 10.0,
            ],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |title: &str| {
            output
                .plots
                .iter()
                .find(|plot| plot.title == title)
                .unwrap()
                .values[19]
        };
        // `values` is [7, 3, 9, 2] and the filled copy is [0, 0, 0, 2, 1].
        assert_eq!(at("sum"), 3.0);
        assert_eq!(at("joined"), 7.0);
        assert_eq!(at("all"), 1.0);
        assert_eq!(at("any"), 1.0);
        assert!(
            output
                .drawings
                .iter()
                .any(|drawing| drawing.kind == DrawingKind::Box && drawing.text.starts_with("BUY")),
            "{:?}",
            output.diagnostics
        );
    }

    #[test]
    fn a_drawing_takes_its_style_from_the_documented_positions() {
        let script = r#"indicator("T", overlay=true)
if barstate.islast
    line.new(0, low, 1, high, xloc.bar_index, extend.none, color.red, line.style_dotted, 3)
    label.new(1, high, "p", xloc.bar_index, yloc.abovebar, color.blue, label.style_label_down, color.green, size.large)
    box.new(0, high, 1, low, color.purple, 2, line.style_dashed, extend.none, xloc.bar_index, color.blue, "b", size.small, color.yellow)
plot(close)
"#;
        let output = run(script, &[10.0, 11.0, 12.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let line = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Line)
            .unwrap();
        assert_eq!(line.color, Color32::RED);
        assert_eq!(line.width, 3.0);
        assert_eq!(line.style, egui_charts::model::LineStyle::Dotted);
        let label = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Label)
            .unwrap();
        assert_eq!(label.color, Color32::GREEN);
        assert_eq!(label.font_size, 16.0);
        let rectangle = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Box)
            .unwrap();
        assert_eq!(rectangle.color, Color32::from_rgb(156, 39, 176));
        assert_eq!(rectangle.fill_color, Some(Color32::BLUE));
        assert_eq!(rectangle.text_color, Some(Color32::YELLOW));
        assert_eq!(rectangle.text, "b");
        assert_eq!(rectangle.width, 2.0);
        assert_eq!(rectangle.font_size, 10.0);
        assert_eq!(rectangle.style, egui_charts::model::LineStyle::Dashed);
    }

    #[test]
    fn an_hour_bar_feeds_hour() {
        let program = Parser::parse("indicator(\"T\")\nplot(hour)\n").unwrap();
        let bars = bars(&[1.0]);
        let context = Context::default();
        let output = Evaluator::new(&program, &bars, &context, &HashMap::new(), &[]).run();
        assert_eq!(output.plots[0].values[0], 0.0);
    }

    #[test]
    fn the_timeframe_flags_answer_from_the_interval() {
        let check = |timeframe: egui_charts::model::Timeframe, expected: [f64; 5]| {
            let program = Parser::parse(
                "indicator(\"T\")\nplot(timeframe.isintraday ? 1 : 0)\nplot(timeframe.isdaily ? 1 : 0)\nplot(timeframe.isdwm ? 1 : 0)\nplot(weekofyear(time))\nplot(timeframe.multiplier)\n",
            )
            .unwrap();
            let bars = bars(&[1.0]);
            let context = Context {
                timeframe,
                ..Context::default()
            };
            let output = Evaluator::new(&program, &bars, &context, &HashMap::new(), &[]).run();
            assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
            for (at, value) in expected.iter().enumerate() {
                assert_eq!(output.plots[at].values[0], *value, "{timeframe:?}");
            }
        };
        // Both bars fall on 2024-01-01, ISO week 1.
        check(egui_charts::model::Timeframe::Min5, [1.0, 0.0, 0.0, 1.0, 5.0]);
        check(egui_charts::model::Timeframe::Hour4, [1.0, 0.0, 0.0, 1.0, 240.0]);
        check(egui_charts::model::Timeframe::Day1, [0.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn a_bollinger_band_answers_its_three_series() {
        let output = run(
            "indicator(\"T\")\n[middle, upper, lower] = ta.bb(close, 2, 1)\nplot(middle)\nplot(upper)\nplot(lower)\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let last = |at: usize| output.plots[at].values[3];
        assert!((last(0) - 3.5).abs() < 1e-9);
        assert!((last(1) - 4.0).abs() < 1e-9);
        assert!((last(2) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn dmi_supertrend_and_sar_keep_drawing() {
        let output = run(
            "indicator(\"T\")\n[plus, minus, adx] = ta.dmi(3, 3)\nplot(plus)\nplot(minus)\nplot(adx)\nplot(ta.adx(3, 1))\n[line, direction] = ta.supertrend(1, 3)\nplot(line)\nplot(ta.sar())\nplot(ta.stoch(close, high, low, 3))\nplot(ta.alma(close, 3))\nplot(ta.range(close, 2))\nplot(ta.percentile_nearest_rank(close, 3, 50))\n",
            &[1.0, 2.0, 3.0, 4.0, 3.0, 5.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        for (at, plot) in output.plots.iter().enumerate() {
            let value = plot.values[5];
            assert!(value.is_finite(), "plot {at} stopped drawing");
        }
    }

    #[test]
    fn cross_cog_hma_macd_mode_percentrank_and_tsi_keep_drawing() {
        let output = run(
            "indicator(\"T\")\nplot(ta.cross(close, ta.sma(close, 2)) ? 1 : 0)\nplot(ta.cog(close, 3))\nplot(ta.hma(close, 4))\n[macd, signal, hist] = ta.macd(close, 2, 4, 2)\nplot(macd)\nplot(signal)\nplot(hist)\nplot(ta.mode(close, 3))\nplot(ta.percentrank(close, 3))\nplot(ta.tsi(close, 2, 4))\n",
            &[1.0, 2.0, 3.0, 4.0, 3.0, 5.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        for (at, plot) in output.plots.iter().enumerate() {
            assert!(plot.values[5].is_finite(), "plot {at} stopped drawing");
        }
        // The last three closes are 4, 3, 5, so two of the other bars sit below the current one.
        assert_eq!(output.plots[7].values[5], 100.0);
        // The middle of that window is the value seen twice nowhere, so the newest answer is `4`.
        assert_eq!(output.plots[6].values[3], 4.0);
        assert_eq!(
            output.plots[0].values[3], 0.0,
            "a pair that has not crossed answers 0"
        );
    }

    #[test]
    fn a_cross_answers_in_either_direction() {
        let output = run(
            "indicator(\"T\")\nplot(ta.cross(close, 2) ? 1 : 0)\nplot(ta.crossover(close, 2) ? 1 : 0)\n",
            &[1.0, 3.0, 1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values, [0.0, 1.0, 1.0]);
        assert_eq!(output.plots[1].values, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn str_replace_answers_the_named_occurrence() {
        let output = run(
            "indicator(\"T\")\nplot(0)\ns = str.replace(\"a-b-c\", \"-\", \"++\", 2)\nplot(str.length(s))\nplot(str.length(str.replace(\"a-b-c\", \"-\", \"++\")))\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        // The second dash is the only one replaced, and the default replaces them all.
        assert_eq!(output.plots[1].values[0], 6.0);
        assert_eq!(output.plots[2].values[0], 7.0);
    }

    #[test]
    fn str_format_reads_its_number_patterns() {
        let output = run(
            "indicator(\"T\")\nlabel.new(bar_index, close, str.format(\"{0,number,#.##}\", 123.456))\nlabel.new(bar_index, close, str.format(\"{0,number,000.00}\", 1.5))\nlabel.new(bar_index, close, str.format(\"{0,number,percent}\", 0.2))\nlabel.new(bar_index, close, str.format(\"{0}!\", \"ok\"))\nlabel.new(bar_index, close, str.tostring(1.23456, \"#.##\"))\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let texts: Vec<&str> = output
            .drawings
            .iter()
            .map(|drawing| drawing.text.as_str())
            .collect();
        assert_eq!(texts, ["123.46", "001.50", "20%", "ok!", "1.23"]);
    }

    #[test]
    fn plotcandle_draws_the_four_prices_on_every_bar() {
        let output = run(
            "indicator(\"T\")\nplotcandle(open, high, low, close, color = close > 1.5 ? color.green : color.red, title = \"C\")\n",
            &[1.0, 2.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.candles.len(), 2);
        assert_eq!(output.candles[1].candle.high, 3.0);
        assert_eq!(output.candles[1].candle.low, 1.0);
        assert_eq!(output.candles[1].plot, 0);
        assert_eq!(output.candles[1].color, Some(Color32::GREEN));
        assert_eq!(output.candles[0].color, Some(Color32::RED));
        assert_eq!(output.plots[0].title, "C");
    }

    #[test]
    fn plotbar_marks_open_and_close_as_bars() {
        let output = run(
            "indicator(\"T\")\nplotbar(open, high, low, close)\n",
            &[1.0, 2.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.candles.len(), 2);
        assert!(output.candles.iter().all(|candle| candle.bar_style));
    }

    #[test]
    fn named_ta_arguments_may_be_written_in_any_order() {
        let output = run(
            "indicator(\"T\")\nplot(ta.sma(length=2, source=close))\n[middle, upper, lower] = ta.bb(mult=1, series=close, length=2)\nplot(middle)\nplot(upper)\nplot(lower)\nplot(ta.atr(length=2))\n",
            &[1.0, 2.0, 3.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert!((output.plots[0].values[2] - 2.5).abs() < 1e-9);
        assert!((output.plots[1].values[2] - 2.5).abs() < 1e-9);
        assert!((output.plots[2].values[2] - 3.0).abs() < 1e-9);
        assert!((output.plots[3].values[2] - 2.0).abs() < 1e-9);
        assert!((output.plots[4].values[2] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn array_every_and_some_call_a_lambda() {
        let output = run(
            "indicator(\"T\")\nvalues = array.from(1, 2, 3)\nlimit = 0\nplot(array.every(values, (x) => x > 0) ? 1 : 0)\nplot(array.some(values, x => x > 2) ? 1 : 0)\nplot(array.every(values, x => x > limit) ? 1 : 0)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.plots[1].values[0], 1.0);
        assert_eq!(output.plots[2].values[0], 1.0);
    }

    #[test]
    fn str_match_reads_a_regular_expression() {
        let output = run(
            "indicator(\"T\")\nplot(str.match(\"BINANCE:BTCUSDT\", \"^BINANCE:.*$\") ? 1 : 0)\nplot(str.match(\"BTCUSDT\", \"ETH\") ? 1 : 0)\nplot(str.match(\"NOPE\") ? 1 : 0)\nplot(str.match(\"x\", \"[\") ? 1 : 0)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 1.0);
        assert_eq!(output.plots[1].values[0], 0.0);
        // `str.match(regex)` reads the symbol, which is empty in this test context.
        assert_eq!(output.plots[2].values[0], 0.0);
        assert_eq!(output.plots[3].values[0], 0.0);
    }

    #[test]
    fn matrix_reads_writes_and_multiplies() {
        let output = run(
            "indicator(\"T\")\nm = matrix.new(2, 2, 1)\nm.set(0, 1, 3)\nplot(matrix.get(m, 0, 1))\nplot(m.get(1, 0))\nplot(matrix.sum(m))\nplot(matrix.rows(m))\nplot(matrix.columns(m))\nn = matrix.new(2, 2, 2)\nproduct = matrix.mult(m, n)\nplot(product.get(0, 0))\nplot(matrix.det(m))\nplot(matrix.trace(m))\nplot(m.row(0).get(1))\nplot(matrix.transpose(m).get(1, 0))\ninverse = matrix.inv(matrix.identity(2))\nplot(inverse.get(1, 1))\nplot(matrix.is_identity(matrix.identity(2)) ? 1 : 0)\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let at = |index: usize| output.plots[index].values[0];
        assert_eq!(at(0), 3.0);
        assert_eq!(at(1), 1.0);
        assert_eq!(at(2), 6.0);
        assert_eq!(at(3), 2.0);
        assert_eq!(at(4), 2.0);
        assert_eq!(at(5), 8.0);
        assert_eq!(at(6), -2.0);
        assert_eq!(at(7), 2.0);
        assert_eq!(at(8), 3.0);
        assert_eq!(at(9), 3.0);
        assert_eq!(at(10), 1.0);
        assert_eq!(at(11), 1.0);
    }

    #[test]
    fn matrix_operations_accept_the_same_matrix_twice() {
        let output = run("indicator(\"Matrix fixture\")\nm = matrix.new(2, 2, 2)\na = matrix.add(m, m)\nb = matrix.mult(m, m)\nc = matrix.kron(m, m)\nplot(matrix.get(a, 0, 0))\nplot(matrix.get(b, 0, 0))\nplot(matrix.get(c, 0, 0))\n", &[1.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots.iter().map(|p| p.values[0]).collect::<Vec<_>>(), [4.0, 8.0, 4.0]);
    }

    #[test]
    fn anniversary_oscillator_and_cumulative_series_answer() {
        let closes: Vec<f64> = (1..=40).map(|value| value as f64).collect();
        let output = run(
            "indicator(\"T\")\nplot(ta.ao())\nplot(ta.accdist())\nplot(ta.iii())\nplot(ta.nvi())\nplot(ta.pvi())\nplot(ta.pvt())\nplot(ta.wad())\nplot(ta.trix(close, 2))\nplot(ta.max(close, 2))\nplot(ta.min(close, 2))\n",
            &closes,
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[8].values[39], 40.0);
        assert_eq!(output.plots[9].values[39], 39.0);
        for (at, plot) in output.plots.iter().enumerate() {
            assert!(plot.values[39].is_finite(), "plot {at} stopped drawing");
        }
    }

    #[test]
    fn array_mode_percentile_and_binary_search_answer() {
        let output = run(
            "indicator(\"T\")\na = array.from(3, 1, 2, 2)\nplot(array.mode(a))\nplot(array.percentile_linear_interpolation(a, 50))\nb = array.from(1, 2, 3)\nplot(array.binary_search(b, 2))\nplot(array.binary_search(b, 9))\nplot(array.binary_search_leftmost(b, 2))\nplot(array.binary_search_rightmost(b, 2))\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 2.0);
        assert!((output.plots[1].values[0] - 2.0).abs() < 1e-9);
        assert_eq!(output.plots[2].values[0], 1.0);
        assert_eq!(output.plots[3].values[0], -1.0);
        assert_eq!(output.plots[4].values[0], 1.0);
        assert_eq!(output.plots[5].values[0], 1.0);
    }

    #[test]
    fn str_repeat_and_math_random_stay_in_range() {
        let output = run(
            "indicator(\"T\")\nplot(str.length(str.repeat(\"ab\", 3)))\nplot(math.random())\nplot(math.random(5, 6))\n",
            &[1.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values[0], 6.0);
        assert!((0.0..1.0).contains(&output.plots[1].values[0]));
        assert!((5.0..6.0).contains(&output.plots[2].values[0]));
    }

    #[test]
    fn a_linefill_spans_its_two_lines() {
        let output = run(
            "indicator(\"T\", overlay=true)\nif barstate.islast\n    a = line.new(0, low, bar_index, low)\n    b = line.new(0, high, bar_index, high)\n    f = linefill.new(a, b, color.new(color.red, 80))\n    linefill.set_color(f, color.new(color.blue, 80))\nplot(close)\n",
            &[10.0, 11.0, 12.0],
        );
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let fill = output
            .drawings
            .iter()
            .find(|drawing| drawing.kind == DrawingKind::Fill)
            .expect("the linefill was kept");
        assert_eq!(fill.first, (0.0, 11.0));
        assert_eq!(fill.second, (2.0, 11.0));
        let quad = fill.quad.unwrap();
        assert_eq!(quad, [(2.0, 13.0), (0.0, 13.0)]);
        let fill_color = fill.fill_color.unwrap();
        // `set_color` replaced the creation colour; 80% transparency leaves alpha 51.
        assert_eq!(
            (
                fill_color.r(),
                fill_color.g(),
                fill_color.b(),
                fill_color.a()
            ),
            (0, 0, 51, 51)
        );
    }
    #[test]
    fn signal_cross_detects_both_directions() {
        let output = run("indicator(\"Cross fixture\")\nplot(ta.cross(close, 2) ? 1 : 0)\n", &[1.0, 2.0, 3.0, 2.0, 1.0, 2.0, 3.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values, [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn legacy_session_and_hour_follow_exchange_timezone() {
        use chrono::TimeZone;
        let script = crate::pine_engine::Script::parse("//@version=4\nstudy(\"Session fixture\")\ninside = not na(time(\"1440\", \"0400-0930\"))\nplot(inside ? 1 : 0)\nplot(hour)\n").unwrap();
        let bars: Vec<_> = [(2024, 1, 2, 13, 0), (2024, 7, 2, 13, 0), (2024, 7, 2, 14, 0)]
            .into_iter().map(|(y, m, d, h, min)| Bar::new(chrono::Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap(), 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
        let context = Context { timezone: egui_charts::model::TimezoneId::named("America/New_York").unwrap(), ..Context::default() };
        let output = script.run(&bars, &context, &[]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots[0].values, [1.0, 1.0, 0.0]);
        assert_eq!(output.plots[1].values, [8.0, 9.0, 10.0]);
    }

    #[test]
    fn explicit_new_york_clock_handles_daylight_saving_with_a_pacific_chart() {
        use chrono::TimeZone;
        let script = crate::pine_engine::Script::parse("//@version=4\nstudy(\"Clock fixture\")\nplot(hour(time, \"America/New_York\"))\nplot(minute(time, \"America/New_York\"))\n").unwrap();
        let bars = [(1, 14), (7, 13)].into_iter().map(|(month, hour)| {
            Bar::new(chrono::Utc.with_ymd_and_hms(2024, month, 2, hour, 30, 0).unwrap(), 1.0, 1.0, 1.0, 1.0, 1.0)
        }).collect::<Vec<_>>();
        let context = Context { timezone: "America/Los_Angeles".parse().unwrap(), ..Context::default() };
        let output = script.run(&bars, &context, &[]);
        assert_eq!(output.plots[0].values, [9.0, 9.0]);
        assert_eq!(output.plots[1].values, [30.0, 30.0]);
    }

    #[test]
    fn legacy_levels_fixture_evaluates_security_session_and_labels() {
        let script = r#"//@version=4
study(title="Session level fixture", shorttitle="Levels", overlay=true)
prior = security(syminfo.tickerid, 'D', high[1])
session = time("1440", "0400-0930")
started = not na(session) and na(session[1])
level = float(na)
if started and barstate.isnew and hour < 10
    level := high
else
    level := level[1]
plot(prior, title="Prior", color=#FFFFFF, linewidth=2)
plot(level, title="Session", color=#00FF00)
tag = label.new(x=time, y=level, text="Session", xloc=xloc.bar_time, style=label.style_none, textcolor=#00FF00)
label.delete(tag[1])
"#;
        let parsed = crate::pine_engine::Script::parse(script).unwrap();
        assert!(parsed.diagnostics().is_empty(), "{:?}", parsed.diagnostics());
        let bars = bars(&[10.0, 11.0, 12.0]);
        let output = parsed.run(&bars, &Context::default(), &[]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        assert_eq!(output.plots.len(), 2);
        assert_eq!(output.drawings.len(), 1);
    }

    #[test]
    fn valuewhen_occurrence_is_zero_based() {
        let output = run("indicator(\"Event fixture\")\ncondition = close > close[1]\nplot(ta.valuewhen(condition, close, 0))\nplot(ta.valuewhen(condition, close, 1))\n", &[1.0, 2.0, 1.0, 4.0, 3.0, 5.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let recent = &output.plots[0].values;
        let previous = &output.plots[1].values;
        assert!(recent[0].is_nan() && previous[1].is_nan());
        assert_eq!(&recent[1..], &[2.0, 2.0, 4.0, 4.0, 5.0]);
        assert_eq!(&previous[3..], &[2.0, 2.0, 4.0]);
    }

    #[test]
    fn macd_tuple_matches_the_engines_separate_ema_calculations() {
        let output = run("indicator(\"Momentum fixture\")\n[m, s, h] = ta.macd(close, 2, 3, 2)\nmanual = ta.ema(close, 2) - ta.ema(close, 3)\nmanualSignal = ta.ema(manual, 2)\nplot(m)\nplot(s)\nplot(h)\nplot(manual)\nplot(manualSignal)\n", &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        for i in 0..5 {
            for (a, b) in [(0, 3), (1, 4)] {
                let (x, y) = (output.plots[a].values[i], output.plots[b].values[i]);
                assert!(x == y || (x.is_nan() && y.is_nan()));
            }
        }
        assert_eq!(&output.plots[2].values[3..], &[0.0, 0.0]);
    }
}
