//! A Pine Script subset: lexer, parser, evaluator and the [`PineStudy`] that charts it.
//!
//! The subset covers what a user's ordinary indicator needs: series with `[n]`, `var`, `:=`,
//! assignments and `if` values, user functions, `input.*`, `ta.*`, `math.*`, `color.*`, `str.*`,
//! `plot`, `fill`, `plotshape` and `hline`. Everything else parses into a diagnostic with its
//! line, so the editor can say what a script uses that this engine does not run.

pub mod ast;
pub mod builtins;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod study;
pub mod value;

use std::collections::HashMap;

pub use study::PineStudy;

use crate::pine_engine::ast::{Block, Diagnostic, InputDecl, NodeId, Program, Stmt};
use crate::pine_engine::runtime::{Context, RunOutput};
use crate::pine_engine::value::Value;

/// A parsed script and its current input values.
#[derive(Clone, Debug)]
pub struct Script {
    program: Program,
    values: HashMap<NodeId, Value>,
    diagnostics: Vec<Diagnostic>,
}

impl Script {
    /// Parses `source`, or fails on syntax the parser cannot read at all.
    pub fn parse(source: &str) -> Result<Self, String> {
        let program = parser::Parser::parse(source)?;
        let mut diagnostics = program.diagnostics.clone();
        collect_unsupported(&program.body, &mut diagnostics);
        dedup(&mut diagnostics);
        let values = program
            .inputs
            .iter()
            .map(|input| (input.id, input.default.clone()))
            .collect();
        Ok(Self {
            program,
            values,
            diagnostics,
        })
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The `input.*` schema, in source order.
    pub fn inputs(&self) -> &[InputDecl] {
        &self.program.inputs
    }

    /// Constructs the value of one input.
    pub fn set_input(&mut self, id: NodeId, value: Value) {
        self.values.insert(id, value);
    }

    /// Runs every bar. `diagnostics` then holds what the run itself could not do.
    pub fn run(
        &self,
        bars: &[egui_charts::model::Bar],
        context: &Context,
        sources: &[runtime::SecurityData],
    ) -> RunOutput {
        let mut output =
            runtime::Evaluator::new(&self.program, bars, context, &self.values, sources).run();
        output.diagnostics.extend(self.diagnostics.iter().cloned());
        dedup(&mut output.diagnostics);
        output
    }

    /// The notes about constructs this subset does not run, with their lines.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Collects the `Stmt::Unsupported` notes the parser left in the body.
fn collect_unsupported(block: &Block, diagnostics: &mut Vec<Diagnostic>) {
    for statement in block {
        match statement {
            Stmt::Unsupported { line, message } => diagnostics.push(Diagnostic {
                line: *line,
                message: message.clone(),
            }),
            Stmt::If {
                then, otherwise, ..
            } => {
                collect_unsupported(then, diagnostics);
                if let Some(otherwise) = otherwise {
                    collect_unsupported(otherwise, diagnostics);
                }
            }
            Stmt::For { body, .. } | Stmt::ForIn { body, .. } | Stmt::While { body, .. } => {
                collect_unsupported(body, diagnostics);
            }
            Stmt::Function(function) => collect_unsupported(&function.body, diagnostics),
            _ => {}
        }
    }
}

fn dedup(diagnostics: &mut Vec<Diagnostic>) {
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|diagnostic| seen.insert((diagnostic.line, diagnostic.message.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pine_engine::parser::Parser;

    #[test]
    fn legacy_and_modern_inputs_keep_their_defaults() {
        let script = Script::parse(
            "//@version=4\nstudy(\"W\")\nn = input(title=\"Periods\", defval=2, minval=2, type=input.integer)\nmode = input(title=\"Mode\", defval=\"3\", options=[\"3\", \"5\"])\nlen = input.int(14, \"Length\")\ncolor = input.color(color.red, \"Color\")\n",
        )
        .unwrap();
        let inputs = script.inputs();
        assert_eq!(inputs.len(), 4);
        assert_eq!(inputs[0].title, "Periods");
        assert_eq!(inputs[0].default, Value::Num(2.0));
        assert!(matches!(inputs[1].kind, ast::InputKind::Choice(_)));
        assert_eq!(inputs[2].title, "Length");
        assert_eq!(inputs[2].default, Value::Num(14.0));
        assert!(matches!(inputs[3].default, Value::Color(_)));
    }

    #[test]
    fn loops_parse_without_diagnostics() {
        let script =
            Script::parse("indicator(\"T\")\nfor i = 0 to 3\n    plot(close)\n").unwrap();
        assert!(script.diagnostics().is_empty(), "{:?}", script.diagnostics());
    }

    #[test]
    fn authored_indicator_parses_and_runs() {
        let source = "indicator(\"Range fixture\")\nplot(high - low)\n";
        let script = Script::parse(source).unwrap();
        assert_eq!(script.program().title, "Range fixture");
        let bars: Vec<_> = (0..10)
            .map(|at| {
                egui_charts::model::Bar::new(
                    chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(at),
                    at as f64,
                    at as f64 + 1.0,
                    at as f64 - 1.0,
                    at as f64,
                    1.0,
                )
            })
            .collect();
        let output = script.run(&bars, &Context::default(), &[]);
        assert_eq!(output.plots[0].values, vec![2.0; 10]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }

    #[test]
    fn parser_still_reads_a_plain_script() {
        assert!(Parser::parse("indicator(\"T\")\nplot(close)\n").is_ok());
    }

}
