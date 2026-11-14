//! Recursive-descent parser for the Pine subset, with the indentation the lexer produced.
//!
//! Everything it parses but does not run is recorded in [`Program::diagnostics`] with its line, so
//! the editor can say what is missing instead of failing the whole script.

use crate::pine_engine::ast::{
    Arg, Block, Diagnostic, Expr, FieldDecl, Function, InputDecl, InputKind, NodeId, Program, Stmt,
    SwitchArm, TypeDecl,
};
use crate::pine_engine::lexer::{Spanned, Token, lex};
use crate::pine_engine::value::Value;

/// Type words a Pine declaration may start with.
const TYPES: &[&str] = &[
    "float", "int", "bool", "string", "color", "line", "label", "box", "table", "array", "matrix",
    "map",
];

/// Constructs that are parsed and skipped, with the reason the editor shows.
fn unsupported(reason: &str) -> String {
    format!("{reason}: parsed but not supported")
}

pub struct Parser {
    tokens: Vec<Spanned>,
    at: usize,
    next_id: NodeId,
    inputs: Vec<InputDecl>,
    diagnostics: Vec<Diagnostic>,
}

impl Parser {
    /// Parses `source`, or fails on syntax this parser cannot read at all.
    pub fn parse(source: &str) -> Result<Program, String> {
        // A script saved by Windows may start with a byte-order mark, which is not Pine source.
        let source = source.trim_start_matches('\u{feff}');
        let tokens = lex(source)?;
        let mut parser = Self {
            tokens,
            at: 0,
            next_id: 1,
            inputs: Vec::new(),
            diagnostics: Vec::new(),
        };
        let mut title = "Pine script".to_owned();
        let mut overlay = true;
        let body = parser.parse_body(&mut title, &mut overlay)?;
        inputs_of(&body, &mut parser.inputs);
        Ok(Program {
            title,
            overlay,
            body,
            inputs: parser.inputs,
            diagnostics: parser.diagnostics,
        })
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.at].token
    }

    fn line(&self) -> usize {
        self.tokens[self.at].line
    }

    fn advance(&mut self) -> Spanned {
        let token = self.tokens[self.at].clone();
        if self.at + 1 < self.tokens.len() {
            self.at += 1;
        }
        token
    }

    fn at_op(&self, op: &str) -> bool {
        self.tokens[self.at].is_op(op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.at_op(op) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<(), String> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(format!("line {}: expected `{op}`", self.line()))
        }
    }

    fn at_ident(&self, name: &str) -> bool {
        self.tokens[self.at].is_ident(name)
    }

    fn eat_ident(&mut self, name: &str) -> bool {
        if self.at_ident(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.peek().clone() {
            Token::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(format!("line {}: expected a name", self.line())),
        }
    }

    fn eat_newlines(&mut self) {
        while matches!(self.peek(), Token::Newline) {
            self.advance();
        }
    }

    fn id(&mut self) -> NodeId {
        self.next_id += 1;
        self.next_id
    }

    fn parse_body(&mut self, title: &mut String, overlay: &mut bool) -> Result<Block, String> {
        let mut body = Vec::new();
        loop {
            match self.peek() {
                Token::Eof => return Ok(body),
                Token::Newline | Token::Dedent => {
                    self.advance();
                }
                Token::Indent => return Err(format!("line {}: unexpected indent", self.line())),
                _ => {
                    if let Some((new_title, new_overlay)) = self.parse_declaration()? {
                        *title = new_title;
                        *overlay = new_overlay;
                        continue;
                    }
                    let before = self.at;
                    let statement = self.parse_stmt()?;
                    body.push(statement);
                    // A statement that reads no token would repeat forever, so refuse it.
                    if self.at == before {
                        return Err(format!("line {}: cannot read this statement", self.line()));
                    }
                }
            }
        }
    }

    /// The `indicator(...)`/`study(...)`/`strategy(...)` header, when this statement is one.
    fn parse_declaration(&mut self) -> Result<Option<(String, bool)>, String> {
        let saves = self.at;
        let Token::Ident(kind) = self.peek().clone() else {
            return Ok(None);
        };
        if !matches!(kind.as_str(), "indicator" | "study" | "strategy") {
            return Ok(None);
        }
        self.advance();
        if !self.at_op("(") {
            self.at = saves;
            return Ok(None);
        }
        if kind == "strategy" {
            return Err("Strategies are not supported; use an indicator script.".into());
        }
        let args = self.parse_args()?;
        let mut title = None;
        let mut overlay = true;
        for (at, arg) in args.iter().enumerate() {
            match (arg.name.as_deref(), literal(&arg.value)) {
                (Some("title"), Some(Value::Str(text))) => title = Some(text),
                // v4 wrote the title first: `study("Title", ...)`.
                (None, Some(Value::Str(text))) if at == 0 => title = Some(text),
                (Some("overlay"), Some(Value::Bool(flag))) => overlay = flag,
                _ => {}
            }
        }
        self.end_line()?;
        Ok(Some((
            title.unwrap_or_else(|| "Pine script".to_owned()),
            overlay,
        )))
    }

    fn parses_declaration(&self) -> bool {
        matches!(self.peek(), Token::Ident(name) if matches!(name.as_str(), "indicator" | "study" | "strategy"))
            && self
                .tokens
                .get(self.at + 1)
                .is_some_and(|next| next.is_op("("))
    }

    fn end_line(&mut self) -> Result<(), String> {
        match self.peek() {
            Token::Newline | Token::Dedent | Token::Eof => Ok(()),
            // A statement that ends in its own indented block ends with a `Dedent`, and the
            // next statement starts without a newline between them.
            _ if self.at > 0 && matches!(self.tokens[self.at - 1].token, Token::Dedent) => Ok(()),
            other => Err(format!(
                "line {}: unexpected {other:?} after a statement",
                self.line()
            )),
        }
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        if self.parses_declaration() {
            let _ = self.parse_declaration()?;
            return Ok(Stmt::Unsupported {
                line,
                message: unsupported("Multiple indicator declarations"),
            });
        }
        if self.at_ident("if") {
            return self.parse_if_statement();
        }
        if self.at_ident("for") {
            return self.parse_for();
        }
        if self.at_ident("while") {
            return self.parse_while();
        }
        if self.at_ident("break") {
            self.advance();
            self.end_line()?;
            self.eat_newlines();
            return Ok(Stmt::Break);
        }
        if self.at_ident("continue") {
            self.advance();
            self.end_line()?;
            self.eat_newlines();
            return Ok(Stmt::Continue);
        }
        if self.at_ident("type") || self.at_ident("enum") {
            let decl = self.parse_type()?;
            return Ok(Stmt::Type(decl));
        }
        if self.at_ident("method") {
            self.advance();
            return Ok(Stmt::Function(self.parse_function()?));
        }
        if self.at_ident("export") || self.at_ident("import") {
            self.skip_statement()?;
            return Ok(Stmt::Unsupported {
                line,
                message: unsupported("import/export"),
            });
        }
        if self.looks_like_function() {
            return Ok(Stmt::Function(self.parse_function()?));
        }

        // `const` is a compile-time constant, which this engine reads as an ordinary value.
        self.eat_ident("const");
        // `var`/`varip`, an optional type word, then a name and an assignment.
        let mut sticky = false;
        if self.at_ident("var") || self.at_ident("varip") {
            sticky = true;
            self.advance();
        }
        // A type word starts a declaration only before a name, not before `line.new`'s `.`.
        // A script's own lowercase type (`hz _hz`) is two identifiers; `switch x` is not.
        if self.starts_with_type() || (self.at_pair_of_idents() && !self.at_ident("switch")) {
            self.advance();
            if self.at_op("<") {
                self.skip_type_arguments()?;
            }
            // `float[] values` declares an array.
            if self.eat_op("[") {
                self.expect_op("]")?;
            }
        }
        if self.at_op("[") {
            return self.parse_tuple_assignment(line, sticky);
        }
        if self.looks_like_field_assignment() {
            return self.parse_field_assignment();
        }
        let starts_assign = matches!(self.peek(), Token::Ident(_))
            && self
                .tokens
                .get(self.at + 1)
                .is_some_and(|next| next.is_op("=") || next.is_op(":="));
        if starts_assign {
            let name = self.ident()?;
            let reassign = self.eat_op(":=");
            if !reassign {
                self.expect_op("=")?;
            }
            let value = self.parse_expr()?;
            self.end_line()?;
            self.eat_newlines();
            return Ok(Stmt::Assign {
                name,
                value,
                sticky,
                reassign,
            });
        }
        if sticky {
            self.skip_statement()?;
            return Ok(Stmt::Unsupported {
                line,
                message: unsupported("`var` without an assignment"),
            });
        }
        let value = self.parse_expr()?;
        self.end_line()?;
        self.eat_newlines();
        Ok(Stmt::Expr(value))
    }

    fn looks_like_function(&self) -> bool {
        let mut at = self.at;
        if !matches!(self.tokens.get(at).map(|s| &s.token), Some(Token::Ident(_))) {
            return false;
        }
        at += 1;
        // A declared return type comes first: `float myAverage(...) =>`.
        if matches!(self.tokens.get(at).map(|s| &s.token), Some(Token::Ident(_)))
            && self
                .tokens
                .get(at + 1)
                .is_some_and(|next| next.is_op("("))
        {
            at += 1;
        }
        if !self
            .tokens
            .get(at)
            .is_some_and(|spanned| spanned.is_op("("))
        {
            return false;
        }
        // Skip the parameter list, then look for `=>`.
        let mut depth = 0i32;
        while let Some(spanned) = self.tokens.get(at) {
            if spanned.is_op("(") {
                depth += 1;
            } else if spanned.is_op(")") {
                depth -= 1;
                if depth == 0 {
                    return self.tokens.get(at + 1).is_some_and(|next| next.is_op("=>"));
                }
            }
            at += 1;
        }
        false
    }

    /// Whether two identifiers follow, as in `dwm_info n`: a type the script declared itself.
    fn at_pair_of_idents(&self) -> bool {
        let is_ident = |at: usize| {
            matches!(
                self.tokens.get(at).map(|spanned| &spanned.token),
                Some(Token::Ident(_))
            )
        };
        is_ident(self.at) && is_ident(self.at + 1)
    }

    /// Whether the tokens at the cursor are `name.field := …` rather than a plain name assignment.
    fn looks_like_field_assignment(&self) -> bool {
        let is_ident = |at: usize| {
            matches!(
                self.tokens.get(at).map(|spanned| &spanned.token),
                Some(Token::Ident(_))
            )
        };
        if !is_ident(self.at) {
            return false;
        }
        let mut at = self.at + 1;
        let mut dotted = false;
        loop {
            match self.tokens.get(at).map(|spanned| &spanned.token) {
                Some(Token::Op(".")) => {
                    dotted = true;
                    at += 1;
                    if !is_ident(at) {
                        return false;
                    }
                    at += 1;
                }
                Some(Token::Op("=")) | Some(Token::Op(":=")) => return dotted,
                // `+=` and friends lex as two tokens.
                Some(Token::Op(op))
                    if matches!(*op, "+" | "-" | "*" | "/" | "%")
                        && self
                            .tokens
                            .get(at + 1)
                            .is_some_and(|spanned| spanned.is_op("=")) =>
                {
                    return true;
                }
                _ => return false,
            }
        }
    }

    /// Whether the tokens here start with a type word such as `int` or a user type `Settings`.
    fn starts_with_type(&self) -> bool {
        let Token::Ident(word) = self.peek() else {
            return false;
        };
        let is_type =
            TYPES.contains(&word.as_str()) || word.starts_with(|c: char| c.is_ascii_uppercase());
        is_type
            && matches!(
                self.tokens.get(self.at + 1).map(|spanned| &spanned.token),
                Some(Token::Ident(_)) | Some(Token::Op("[")) | Some(Token::Op("<"))
            )
    }

    fn parse_function(&mut self) -> Result<Function, String> {
        // A declared return type is accepted and dropped: `float myAverage(...) =>`.
        if self.at_pair_of_idents()
            && self
                .tokens
                .get(self.at + 2)
                .is_some_and(|next| next.is_op("("))
        {
            self.advance();
        }
        let name = self.ident()?;
        self.expect_op("(")?;
        let mut params = Vec::new();
        let mut defaults = Vec::new();
        while !self.eat_op(")") {
            // Parameter types are accepted and dropped: scripts are dynamically typed here.
            if self.starts_with_type() || self.at_pair_of_idents() {
                self.advance();
                if self.at_op("<") {
                    self.skip_type_arguments()?;
                }
            }
            // `float[] values` is an array parameter.
            if self.eat_op("[") {
                self.expect_op("]")?;
            }
            params.push(self.ident()?);
            // A caller may leave out a parameter that declares a default.
            defaults.push(if self.eat_op("=") {
                Some(self.parse_expr()?)
            } else {
                None
            });
            if !self.eat_op(",") {
                self.expect_op(")")?;
                break;
            }
        }
        self.expect_op("=>")?;
        let body = if matches!(self.peek(), Token::Newline) {
            self.advance();
            self.parse_block()?
        } else {
            let value = self.parse_expr()?;
            self.end_line()?;
            vec![Stmt::Expr(value)]
        };
        self.eat_newlines();
        Ok(Function {
            name,
            params,
            defaults,
            body,
        })
    }

    /// Pine's `for i = from to to [by step]` and `for value in iterable`.
    fn parse_for(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        self.expect_ident("for")?;
        let vars = if self.at_op("[") {
            self.expect_op("[")?;
            let mut names = Vec::new();
            while !self.at_op("]") {
                names.push(self.ident()?);
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op("]")?;
            names
        } else {
            vec![self.ident()?]
        };
        if self.eat_ident("in") {
            let iterable = self.parse_expr()?;
            self.end_line()?;
            self.eat_newlines();
            let body = self.parse_block()?;
            self.eat_newlines();
            return Ok(Stmt::ForIn {
                line,
                vars,
                iterable,
                body,
            });
        }
        let Some(var) = vars.first().cloned() else {
            return Err(format!("line {line}: `for` needs a variable"));
        };
        self.expect_op("=")?;
        let from = self.parse_expr()?;
        self.expect_ident("to")?;
        let to = self.parse_expr()?;
        let step = if self.eat_ident("by") {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.end_line()?;
        self.eat_newlines();
        let body = self.parse_block()?;
        self.eat_newlines();
        Ok(Stmt::For {
            line,
            var,
            from,
            to,
            step,
            body,
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        self.expect_ident("while")?;
        let cond = self.parse_expr()?;
        self.end_line()?;
        self.eat_newlines();
        let body = self.parse_block()?;
        self.eat_newlines();
        Ok(Stmt::While { line, cond, body })
    }

    /// `[a, b] = f(...)`, whose names take the values the call answers with. A bracketed line that
    /// is not an assignment is an array value, which is how a function returns several values.
    fn parse_tuple_assignment(&mut self, line: usize, sticky: bool) -> Result<Stmt, String> {
        self.expect_op("[")?;
        let mut items = Vec::new();
        while !self.at_op("]") {
            items.push(self.parse_expr()?);
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op("]")?;
        if !self.eat_op("=") && !self.eat_op(":=") {
            self.end_line()?;
            self.eat_newlines();
            return Ok(Stmt::Expr(Expr::Array(items)));
        }
        let mut names = Vec::new();
        for item in items {
            let Expr::Ident(name) = item else {
                return Err(format!("line {line}: expected a name in a tuple assignment"));
            };
            names.push(name);
        }
        let value = self.parse_expr()?;
        self.end_line()?;
        self.eat_newlines();
        Ok(Stmt::Tuple {
            names,
            value,
            sticky,
        })
    }

    /// `type Foo` or `enum Bar`: the indented fields with their defaults.
    fn parse_type(&mut self) -> Result<TypeDecl, String> {
        self.advance();
        let name = self.ident()?;
        self.end_line()?;
        self.eat_newlines();
        let body = self.parse_block()?;
        let fields = body
            .into_iter()
            .filter_map(|statement| match statement {
                Stmt::Assign { name, value, .. } => Some(FieldDecl {
                    name,
                    default: Some(value),
                }),
                // `box[] _box` leaves the type word behind and parses as the bare name.
                Stmt::Expr(Expr::Ident(name)) => Some(FieldDecl {
                    name,
                    default: None,
                }),
                _ => None,
            })
            .collect();
        Ok(TypeDecl { name, fields })
    }

    /// `x := value`, `x += value`, `object.field := value` and the compound field form.
    fn parse_field_assignment(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let parsed = self.parse_postfix()?;
        let compound = if self.at_op(":=") || self.at_op("=") {
            None
        } else {
            let op = match self.peek() {
                Token::Op(op) if matches!(*op, "+" | "-" | "*" | "/" | "%") => *op,
                _ => return Err(format!("line {line}: expected an assignment")),
            };
            self.advance();
            self.expect_op("=")?;
            Some(op)
        };
        let value = match compound {
            None => {
                if !self.eat_op(":=") {
                    self.expect_op("=")?;
                }
                self.parse_expr()?
            }
            Some(op) => {
                let right = self.parse_expr()?;
                Expr::Binary {
                    op,
                    left: Box::new(parsed.clone()),
                    right: Box::new(right),
                }
            }
        };
        self.end_line()?;
        self.eat_newlines();
        match parsed {
            Expr::Member { target, name } => Ok(Stmt::FieldAssign {
                target: *target,
                name,
                value,
            }),
            Expr::Ident(name) => Ok(Stmt::Assign {
                name,
                value,
                sticky: false,
                reassign: true,
            }),
            _ => Err(format!("line {line}: expected an assignment")),
        }
    }

    fn parse_if_statement(&mut self) -> Result<Stmt, String> {
        self.expect_ident("if")?;
        let cond = self.parse_expr()?;
        self.end_line()?;
        self.eat_newlines();
        let then = self.parse_block()?;
        let otherwise = if self.eat_ident("else") {
            // `else if` is one line, an `else` block starts on the next indented line.
            if self.at_ident("if") {
                Some(vec![self.parse_if_statement()?])
            } else {
                self.end_line()?;
                self.eat_newlines();
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        self.eat_newlines();
        Ok(Stmt::If {
            cond,
            then,
            otherwise,
        })
    }

    fn expect_ident(&mut self, name: &str) -> Result<(), String> {
        if self.eat_ident(name) {
            Ok(())
        } else {
            Err(format!("line {}: expected `{name}`", self.line()))
        }
    }

    /// The indented statements after a line that ends in a block header.
    fn parse_block(&mut self) -> Result<Block, String> {
        if !matches!(self.peek(), Token::Indent) {
            return Err(format!("line {}: expected an indented block", self.line()));
        }
        self.advance();
        let mut block = Vec::new();
        loop {
            match self.peek() {
                Token::Dedent => {
                    self.advance();
                    return Ok(block);
                }
                Token::Eof => {
                    return Err(format!("line {}: unterminated block", self.line()));
                }
                Token::Newline => {
                    self.advance();
                }
                _ => block.push(self.parse_stmt()?),
            }
        }
    }

    /// Skips a statement this subset does not run, including its indented block.
    fn skip_statement(&mut self) -> Result<(), String> {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Token::Eof => return Ok(()),
                Token::Indent => {
                    depth += 1;
                    self.advance();
                }
                Token::Dedent => {
                    depth -= 1;
                    self.advance();
                    if depth <= 0 {
                        return Ok(());
                    }
                }
                Token::Newline => {
                    self.advance();
                    if depth == 0 {
                        // A loop or `switch` statement owns the indented block after it.
                        if matches!(self.peek(), Token::Indent) {
                            depth = 1;
                            self.advance();
                            continue;
                        }
                        return Ok(());
                    }
                }
                Token::Op("(") | Token::Op("[") => {
                    self.advance();
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    pub fn parse_expr(&mut self) -> Result<Expr, String> {
        if let Some(lambda) = self.try_lambda()? {
            return Ok(lambda);
        }
        self.parse_ternary()
    }

    /// Reads a lambda when the expression is one, and leaves the position alone otherwise.
    fn try_lambda(&mut self) -> Result<Option<Expr>, String> {
        let start = self.at;
        let mut params = Vec::new();
        match self.peek().clone() {
            Token::Ident(name) => {
                let arrow = self.tokens.get(self.at + 1).is_some_and(|next| next.is_op("=>"));
                if !arrow {
                    return Ok(None);
                }
                self.advance();
                self.advance();
                params.push(name);
            }
            Token::Op("(") => {
                self.advance();
                while !self.eat_op(")") {
                    let Token::Ident(name) = self.peek().clone() else {
                        self.at = start;
                        return Ok(None);
                    };
                    self.advance();
                    params.push(name);
                    if !self.eat_op(",") && !self.at_op(")") {
                        self.at = start;
                        return Ok(None);
                    }
                }
                if !self.eat_op("=>") {
                    self.at = start;
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
        let body = self.parse_expr()?;
        Ok(Some(Expr::Lambda {
            params,
            body: Box::new(body),
        }))
    }

    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_or()?;
        if self.eat_op("?") {
            let then = self.parse_ternary()?;
            self.expect_op(":")?;
            let otherwise = self.parse_ternary()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            });
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while self.eat_ident("or") {
            let right = self.parse_and()?;
            left = Expr::Binary {
                op: "or",
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_equality()?;
        while self.eat_ident("and") {
            let right = self.parse_equality()?;
            left = Expr::Binary {
                op: "and",
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = if self.eat_op("==") {
                "=="
            } else if self.eat_op("!=") {
                "!="
            } else {
                return Ok(left);
            };
            let right = self.parse_comparison()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_additive()?;
        loop {
            let op = if self.eat_op("<=") {
                "<="
            } else if self.eat_op(">=") {
                ">="
            } else if self.eat_op("<") {
                "<"
            } else if self.eat_op(">") {
                ">"
            } else {
                return Ok(left);
            };
            let right = self.parse_additive()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = if self.eat_op("+") {
                "+"
            } else if self.eat_op("-") {
                "-"
            } else {
                return Ok(left);
            };
            let right = self.parse_multiplicative()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = if self.eat_op("*") {
                "*"
            } else if self.eat_op("/") {
                "/"
            } else if self.eat_op("%") {
                "%"
            } else {
                return Ok(left);
            };
            let right = self.parse_unary()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if self.eat_op("-") {
            let value = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: "-",
                value: Box::new(value),
            });
        }
        if self.eat_op("+") {
            return self.parse_unary();
        }
        if self.eat_ident("not") {
            let value = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: "not",
                value: Box::new(value),
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut target = self.parse_primary()?;
        loop {
            // A line break ends the chain, so the statement after a `switch` block is not
            // read as an index or a call of the switch's value.
            if self.at > 0
                && matches!(
                    self.tokens[self.at - 1].token,
                    Token::Newline | Token::Dedent
                )
            {
                return Ok(target);
            }
            // `array.new<float>()`: a constructor's type argument is dropped.
            if self.at_op("<")
                && let Some(path) = path_of(&target)
                && path.last().is_some_and(|name| name.starts_with("new"))
            {
                self.skip_type_arguments()?;
                continue;
            }
            if self.eat_op("(") {
                let args = self.parse_args_after_open()?;
                // `values.push(1)` keeps `values` as the receiver the runtime dispatches on;
                // `f().g()` keeps `f()` there, whose dotted path has no name.
                let callee = path_of(&target).unwrap_or_else(|| match &target {
                    Expr::Member { name, .. } => vec![name.clone()],
                    _ => Vec::new(),
                });
                let receiver = match &target {
                    Expr::Member { target, .. } => Some(target.clone()),
                    _ => None,
                };
                let expr = Expr::Call {
                    id: self.id(),
                    callee,
                    receiver,
                    args,
                    line: self.line(),
                };
                target = expr;
            } else if self.eat_op("[") {
                let index = self.parse_expr()?;
                self.expect_op("]")?;
                target = Expr::Index {
                    target: Box::new(target),
                    index: Box::new(index),
                };
            } else if self.eat_op(".") {
                let name = self.ident()?;
                target = Expr::Member {
                    target: Box::new(target),
                    name,
                };
            } else {
                return Ok(target);
            }
        }
    }

    /// Skips `<…>` after a constructor name, including `array<array<float>>`.
    fn skip_type_arguments(&mut self) -> Result<(), String> {
        let mut depth = 0i32;
        loop {
            if self.at_op("<") {
                depth += 1;
            } else if self.at_op(">") {
                depth -= 1;
                if depth == 0 {
                    self.advance();
                    return Ok(());
                }
            } else if matches!(self.peek(), Token::Eof | Token::Newline) {
                return Err(format!("line {}: unterminated type arguments", self.line()));
            }
            self.advance();
        }
    }

    fn parse_args(&mut self) -> Result<Vec<Arg>, String> {
        self.expect_op("(")?;
        self.parse_args_after_open()
    }

    fn parse_args_after_open(&mut self) -> Result<Vec<Arg>, String> {
        let mut args = Vec::new();
        loop {
            while matches!(self.peek(), Token::Newline) {
                self.advance();
            }
            if self.eat_op(")") {
                return Ok(args);
            }
            let name = match (self.peek(), self.tokens.get(self.at + 1)) {
                (Token::Ident(name), Some(next)) if next.is_op("=") => {
                    let name = name.clone();
                    self.advance();
                    self.advance();
                    Some(name)
                }
                _ => None,
            };
            let value = self.parse_expr()?;
            args.push(Arg { name, value });
            while matches!(self.peek(), Token::Newline) {
                self.advance();
            }
            if self.eat_op(",") {
                continue;
            }
            self.expect_op(")")?;
            return Ok(args);
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            Token::Number(number) => {
                self.advance();
                Ok(Expr::Number(number))
            }
            Token::Str(text) => {
                self.advance();
                Ok(Expr::Str(text))
            }
            Token::Color(rgba) => {
                self.advance();
                Ok(Expr::Color(rgba))
            }
            Token::Op("(") => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect_op(")")?;
                Ok(inner)
            }
            Token::Op("[") => {
                self.advance();
                let mut items = Vec::new();
                while !self.eat_op("]") {
                    items.push(self.parse_expr()?);
                    if !self.eat_op(",") {
                        self.expect_op("]")?;
                        return Ok(Expr::Array(items));
                    }
                }
                Ok(Expr::Array(items))
            }
            Token::Ident(name) => {
                self.advance();
                match name.as_str() {
                    "true" => Ok(Expr::Bool(true)),
                    "false" => Ok(Expr::Bool(false)),
                    // `na(x)` is a call that tests a value; a bare `na` is the missing value.
                    "na" if !self.at_op("(") => Ok(Expr::Na),
                    "if" => self.parse_if_expression(),
                    "switch" => self.parse_switch(),
                    _ => Ok(Expr::Ident(name)),
                }
            }
            other => Err(format!("line {}: unexpected {other:?}", self.line())),
        }
    }

    /// Pine's `if` in value position: the block's last value is the result.
    fn parse_if_expression(&mut self) -> Result<Expr, String> {
        let cond = self.parse_expr()?;
        self.end_line()?;
        self.eat_newlines();
        let then = self.parse_block()?;
        let otherwise = if self.eat_ident("else") {
            if self.at_ident("if") {
                // `else if` nests as a block whose only value is another if-expression.
                self.expect_ident("if")?;
                let nested = self.parse_if_expression()?;
                self.eat_newlines();
                Some(vec![Stmt::Expr(nested)])
            } else {
                self.end_line()?;
                self.eat_newlines();
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        self.eat_newlines();
        Ok(Expr::If {
            cond: Box::new(cond),
            then,
            otherwise,
        })
    }

    /// Pine's `switch`, after its keyword: a subject and one arm per line below it.
    fn parse_switch(&mut self) -> Result<Expr, String> {
        let subject = if matches!(self.peek(), Token::Newline | Token::Eof | Token::Dedent) {
            None
        } else {
            Some(Box::new(self.parse_expr()?))
        };
        self.end_line()?;
        self.eat_newlines();
        let mut arms = Vec::new();
        if matches!(self.peek(), Token::Indent) {
            self.advance();
        }
        loop {
            match self.peek() {
                Token::Dedent => {
                    self.advance();
                    break;
                }
                Token::Eof => break,
                Token::Newline => {
                    self.advance();
                    continue;
                }
                _ => {}
            }
            // `switch` arms may list several values: `"A", "B" => 1`.
            let conds = if self.eat_op("=>") {
                Vec::new()
            } else {
                let mut conds = vec![self.parse_expr()?];
                while self.eat_op(",") {
                    conds.push(self.parse_expr()?);
                }
                self.expect_op("=>")?;
                conds
            };
            let body = if matches!(self.peek(), Token::Newline) {
                self.advance();
                self.eat_newlines();
                if matches!(self.peek(), Token::Indent) {
                    self.parse_block()?
                } else {
                    vec![Stmt::Expr(Expr::Na)]
                }
            } else {
                let value = self.parse_expr()?;
                self.end_line()?;
                vec![Stmt::Expr(value)]
            };
            arms.push(SwitchArm { conds, body });
            self.eat_newlines();
        }
        Ok(Expr::Switch { subject, arms })
    }
}

/// The dotted name of a callee, e.g. `["ta", "ema"]`.
fn path_of(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Ident(name) => Some(vec![name.clone()]),
        Expr::Member { target, name } => {
            let mut path = path_of(target)?;
            path.push(name.clone());
            Some(path)
        }
        _ => None,
    }
}

/// A literal value, for the arguments of a declaration.
pub fn literal(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Number(number) => Some(Value::Num(*number)),
        Expr::Bool(flag) => Some(Value::Bool(*flag)),
        Expr::Str(text) => Some(Value::Str(text.clone())),
        Expr::Na => Some(Value::Na),
        Expr::Color(rgba) => Some(Value::Color(rgba_of(*rgba))),
        _ => None,
    }
}

pub fn rgba_of(rgba: u32) -> egui::Color32 {
    let channel = |shift: u32| ((rgba >> shift) & 0xFF) as u8;
    egui::Color32::from_rgba_unmultiplied(channel(24), channel(16), channel(8), channel(0))
}

/// Collects the `input.*` declarations of a parsed body, so the settings dialog can show them.
pub fn inputs_of(block: &Block, inputs: &mut Vec<InputDecl>) {
    for statement in block {
        match statement {
            Stmt::Assign { value, .. } | Stmt::Expr(value) => inputs_of_expr(value, inputs),
            Stmt::Tuple { value, .. } => inputs_of_expr(value, inputs),
            Stmt::FieldAssign { target, value, .. } => {
                inputs_of_expr(target, inputs);
                inputs_of_expr(value, inputs);
            }
            Stmt::For {
                from,
                to,
                step,
                body,
                ..
            } => {
                inputs_of_expr(from, inputs);
                inputs_of_expr(to, inputs);
                if let Some(step) = step {
                    inputs_of_expr(step, inputs);
                }
                inputs_of(body, inputs);
            }
            Stmt::ForIn { iterable, body, .. } => {
                inputs_of_expr(iterable, inputs);
                inputs_of(body, inputs);
            }
            Stmt::While { cond, body, .. } => {
                inputs_of_expr(cond, inputs);
                inputs_of(body, inputs);
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                inputs_of_expr(cond, inputs);
                inputs_of(then, inputs);
                if let Some(otherwise) = otherwise {
                    inputs_of(otherwise, inputs);
                }
            }
            Stmt::Function(function) => inputs_of(&function.body, inputs),
            Stmt::Break | Stmt::Continue | Stmt::Type(_) | Stmt::Unsupported { .. } => {}
        }
    }
}

fn inputs_of_expr(expr: &Expr, inputs: &mut Vec<InputDecl>) {
    match expr {
        Expr::Call {
            callee, args, id, ..
        } => {
            if callee.first().is_some_and(|name| name == "input")
                && let Some(kind) = callee.get(1).map(String::as_str).or(Some("input"))
                && let Some(decl) = input_declaration(*id, kind, args)
            {
                inputs.push(decl);
            }
            for arg in args {
                inputs_of_expr(&arg.value, inputs);
            }
        }
        Expr::Member { target, .. } => inputs_of_expr(target, inputs),
        Expr::Index { target, index } => {
            inputs_of_expr(target, inputs);
            inputs_of_expr(index, inputs);
        }
        Expr::Unary { value, .. } => inputs_of_expr(value, inputs),
        Expr::Binary { left, right, .. } => {
            inputs_of_expr(left, inputs);
            inputs_of_expr(right, inputs);
        }
        Expr::Ternary {
            cond,
            then,
            otherwise,
        } => {
            inputs_of_expr(cond, inputs);
            inputs_of_expr(then, inputs);
            inputs_of_expr(otherwise, inputs);
        }
        Expr::If {
            cond,
            then,
            otherwise,
        } => {
            inputs_of_expr(cond, inputs);
            inputs_of(then, inputs);
            if let Some(otherwise) = otherwise {
                inputs_of(otherwise, inputs);
            }
        }
        Expr::Array(items) => {
            for item in items {
                inputs_of_expr(item, inputs);
            }
        }
        _ => {}
    }
}

fn arg<'a>(args: &'a [Arg], name: &str) -> Option<&'a Expr> {
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .or_else(|| {
            // The legacy positional form: `input(title, defval, ...)`.
            let position = match name {
                "title" => 0,
                "defval" => 1,
                "minval" => 2,
                "maxval" => 3,
                "options" => 4,
                _ => return None,
            };
            args.iter().filter(|arg| arg.name.is_none()).nth(position)
        })
        .map(|arg| &arg.value)
}

/// The schema of one `input.*` call.
fn input_declaration(id: NodeId, kind: &str, args: &[Arg]) -> Option<InputDecl> {
    let legacy = kind == "input";
    let positional: Vec<&Arg> = args.iter().filter(|arg| arg.name.is_none()).collect();
    let text_of = |expr: &Expr| literal(expr).and_then(|value| value.text().map(str::to_owned));
    let title = args
        .iter()
        .find(|arg| arg.name.as_deref() == Some("title"))
        .and_then(|arg| text_of(&arg.value))
        .or_else(|| {
            // v4 takes the title first, v5+ after the value: `input.int(14, "Length")`.
            let at = if legacy { 0 } else { 1 };
            positional.get(at).and_then(|arg| text_of(&arg.value))
        })
        .unwrap_or_default();
    let options = arg(args, "options").and_then(|expr| match expr {
        Expr::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| literal(item)?.text().map(str::to_owned))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    });
    // v4 wrote `input(title, defval, type=input.integer)`; v5+ has `input.int`.
    let legacy_kind = match arg(args, "type") {
        Some(Expr::Member { target, name })
            if matches!(path_of(target)?.first()?.as_str(), "input") =>
        {
            Some(name.clone())
        }
        _ => None,
    };
    let kind = if kind == "input" {
        match legacy_kind.as_deref() {
            Some("integer") => "integer",
            Some("float") => "float",
            Some("bool") => "bool",
            Some("string") => "string",
            Some("color") => "color",
            Some("session") => "session",
            Some("symbol") => "symbol",
            Some("resolution") => "timeframe",
            _ => "string",
        }
        .to_owned()
    } else {
        kind.to_owned()
    };
    let default_expr = if legacy {
        arg(args, "defval")
            .or_else(|| arg(args, "default"))
            .map(default_value)
            .unwrap_or(Value::Na)
    } else {
        // v5+ takes the value as the first positional argument.
        positional
            .first()
            .map(|arg| default_value(&arg.value))
            .unwrap_or(Value::Na)
    };
    let number = |name: &str| arg(args, name).and_then(|expr| const_number(expr));
    let (kind, default) = match kind.as_str() {
        "integer" | "int" => (
            InputKind::Integer {
                min: number("minval"),
                max: number("maxval"),
            },
            match default_expr {
                Value::Num(number) => Value::Num(number),
                _ => arg(args, "defval")
                    .and_then(|expr| const_number(expr))
                    .map(Value::Num)
                    .unwrap_or(Value::Na),
            },
        ),
        "float" | "number" => (
            InputKind::Number {
                min: number("minval"),
                max: number("maxval"),
            },
            default_expr,
        ),
        "bool" | "toggle" => (InputKind::Toggle, default_expr),
        "color" => (InputKind::Color, default_expr),
        "timeframe" | "resolution" => (InputKind::Timeframe, default_expr),
        "session" => (InputKind::Session, default_expr),
        "symbol" => (InputKind::Symbol, default_expr),
        "source" => (
            InputKind::Text,
            // A source input names a series such as `close`; the chart feeds it at run time.
            if matches!(default_expr, Value::Na) {
                positional
                    .first()
                    .and_then(|arg| path_of(&arg.value))
                    .map(|path| Value::Str(path.join(".")))
                    .unwrap_or(Value::Na)
            } else {
                default_expr
            },
        ),
        "string" => (
            match options {
                Some(options) if !options.is_empty() => InputKind::Choice(options),
                _ => InputKind::Text,
            },
            default_expr,
        ),
        _ => (InputKind::Text, default_expr),
    };
    Some(InputDecl {
        id,
        kind,
        title,
        default,
    })
}

fn const_number(expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Number(number) => Some(*number),
        Expr::Unary { op: "-", value } => const_number(value).map(|number| -number),
        _ => None,
    }
}

/// A literal, or the `color.new(...)` a default often is.
fn default_value(expr: &Expr) -> Value {
    if let Some(value) = literal(expr) {
        return value;
    }
    if let Some(path) = path_of(expr) {
        let joined = path.join(".");
        if let Some(name) = joined.strip_prefix("color.")
            && let Some(color) = crate::pine_engine::builtins::color_constant(name)
        {
            return Value::Color(color);
        }
    }
    if let Expr::Call { callee, args, .. } = expr
        && callee == &["color".to_owned(), "new".to_owned()]
    {
        let base = args
            .first()
            .and_then(|arg| literal(&arg.value))
            .and_then(|value| value.color())
            .unwrap_or(egui::Color32::WHITE);
        let transparency = args
            .get(1)
            .and_then(|arg| const_number(&arg.value))
            .unwrap_or(0.0);
        let factor = 1.0 - (transparency / 100.0).clamp(0.0, 1.0);
        let alpha = (f64::from(base.a()) * factor).round() as u8;
        return Value::Color(egui::Color32::from_rgba_unmultiplied(
            base.r(),
            base.g(),
            base.b(),
            alpha,
        ));
    }
    Value::Na
}
