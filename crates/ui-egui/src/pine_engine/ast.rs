//! The parsed shape of a script: statements, expressions, inputs and the diagnostics the editor
//! shows for what this subset does not run.

use crate::pine_engine::value::Value;

/// Identifies a call site, so a builtin keeps its state (`ta.ema`'s previous value) between bars
/// and an `input.*` keeps the value the user set.
pub type NodeId = u32;

/// A parse-time note about a construct this subset parses but does not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
}

/// What kind of edit an `input.*` call offers.
#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    Integer { min: Option<f64>, max: Option<f64> },
    Number { min: Option<f64>, max: Option<f64> },
    Toggle,
    Text,
    Color,
    Timeframe,
    Session,
    Symbol,
    Choice(Vec<String>),
}

/// One `input.*` call, in source order.
#[derive(Clone, Debug)]
pub struct InputDecl {
    pub id: NodeId,
    pub kind: InputKind,
    pub title: String,
    pub default: Value,
}

/// A call argument, named when the script names it.
#[derive(Clone, Debug)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Number(f64),
    Bool(bool),
    Str(String),
    Color(u32),
    Na,
    Ident(String),
    Member {
        target: Box<Expr>,
        name: String,
    },
    Call {
        id: NodeId,
        /// `ta.ema` is `["ta", "ema"]`; a script's own function is `["myFunc"]`.
        callee: Vec<String>,
        /// The object or array a method is called on: `values.push(1)` keeps `values` here.
        receiver: Option<Box<Expr>>,
        args: Vec<Arg>,
        line: usize,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: &'static str,
        value: Box<Expr>,
    },
    Binary {
        op: &'static str,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// Pine's `x = if cond` … `else` …, which yields the last value of the taken block.
    If {
        cond: Box<Expr>,
        then: Block,
        otherwise: Option<Block>,
    },
    /// A literal list, as `input.string` options use.
    Array(Vec<Expr>),
    /// A lambda: `(x) => x > 0`, passed to `array.every` and its siblings.
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    /// Pine's `switch`: the body of the first arm whose condition matches the subject.
    Switch {
        /// The value the arms compare against; absent for a plain boolean `switch`.
        subject: Option<Box<Expr>>,
        arms: Vec<SwitchArm>,
    },
}

/// One branch of a `switch`.
#[derive(Clone, Debug)]
pub struct SwitchArm {
    /// The values or conditions to compare; empty for the default `=> …` arm.
    pub conds: Vec<Expr>,
    /// The statements whose last value the arm answers with.
    pub body: Block,
}

pub type Block = Vec<Stmt>;

#[derive(Clone, Debug)]
pub enum Stmt {
    Assign {
        name: String,
        value: Expr,
        /// `var x = …` evaluates on the first bar only.
        sticky: bool,
        /// `x := …` writes the variable in the scope that declared it.
        reassign: bool,
    },
    /// `[a, b] = f()`: the names take the values a call answers with.
    Tuple {
        names: Vec<String>,
        value: Expr,
        /// `var [a, b] = …` evaluates on the first bar only.
        sticky: bool,
    },
    If {
        cond: Expr,
        then: Block,
        otherwise: Option<Block>,
    },
    /// Pine's `for i = from to to [by step]`.
    For {
        line: usize,
        var: String,
        from: Expr,
        to: Expr,
        step: Option<Expr>,
        body: Block,
    },
    /// Pine's `for value in array`, and `for [a, b] in array` for tuples.
    ForIn {
        line: usize,
        vars: Vec<String>,
        iterable: Expr,
        body: Block,
    },
    While {
        line: usize,
        cond: Expr,
        body: Block,
    },
    Break,
    Continue,
    /// `object.field := value`.
    FieldAssign {
        target: Expr,
        name: String,
        value: Expr,
    },
    Expr(Expr),
    Function(Function),
    /// A user-defined `type` or `enum`, whose instances `Type.new(…)` builds.
    Type(TypeDecl),
    /// Parsed and reported, not run: `switch`, `strategy.*` calls and the like.
    Unsupported {
        line: usize,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<String>,
    /// The declared default of each parameter, by position; `None` when it has none.
    pub defaults: Vec<Option<Expr>>,
    pub body: Block,
}

/// One field of a user type, with the default its `new()` starts from.
#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub default: Option<Expr>,
}

/// A `type` declaration, so `Point.new(x, y)` can build an instance.
#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

/// A whole script.
#[derive(Clone, Debug)]
pub struct Program {
    pub title: String,
    pub overlay: bool,
    pub body: Block,
    pub inputs: Vec<InputDecl>,
    pub diagnostics: Vec<Diagnostic>,
}
