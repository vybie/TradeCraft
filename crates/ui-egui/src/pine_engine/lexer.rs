//! Tokens of the Pine subset. Pine is indentation-based, so the lexer turns leading spaces into
//! `Indent`/`Dedent` and keeps line numbers for the editor's diagnostics.

/// A lexical token, without its position.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Ident(String),
    Number(f64),
    Str(String),
    /// `#RRGGBB` or `#RRGGBBAA`, as `0xRRGGBBAA`.
    Color(u32),
    /// A fixed operator, keyword-free: punctuation and arithmetic.
    Op(&'static str),
    Newline,
    Indent,
    Dedent,
    Eof,
}

/// A token and the 1-based source line it came from.
#[derive(Clone, Debug)]
pub struct Spanned {
    pub token: Token,
    pub line: usize,
}

impl Spanned {
    pub fn is_op(&self, op: &str) -> bool {
        matches!(&self.token, Token::Op(found) if *found == op)
    }

    pub fn is_ident(&self, name: &str) -> bool {
        matches!(&self.token, Token::Ident(found) if found == name)
    }
}

/// Where a line continues on the next one: inside brackets, or after an operator.
fn continues(line: &str, depth: i32) -> bool {
    if depth > 0 {
        return true;
    }
    let code = line.split("//").next().unwrap_or_default().trim_end();
    // `=>` at the end of a line opens an indented function body, so it does not continue.
    code.ends_with(',')
        || code.ends_with('=')
        || code.ends_with('+')
        || code.ends_with('-')
        || code.ends_with('*')
        || code.ends_with('/')
        || code.ends_with('%')
        || code.ends_with(" and")
        || code.ends_with(" or")
        || code.ends_with(" ?")
        || code.ends_with(" :")
        || code.ends_with("(")
}

/// Whether a line starts with a binary operator, so it continues the statement above it.
fn begins_with_operator(content: &str) -> bool {
    // `-` is left out: `if cond` followed by an indented `-1` is a local block, not a
    // continuation. shortcut: a sum wrapped before its `-` needs the `-` on the line above.
    const OPERATORS: &[&str] = &[
        "+", "*", "/", "%", "==", "!=", "<=", ">=", "<", ">", "and ", "or ", "? ", ": ",
    ];
    OPERATORS
        .iter()
        .any(|operator| content.starts_with(operator))
}

/// Splits `source` into tokens, or describes the first lexical error.
pub fn lex(source: &str) -> Result<Vec<Spanned>, String> {
    let mut tokens: Vec<Spanned> = Vec::new();
    // Leading-space widths of the open blocks.
    let mut indents: Vec<usize> = vec![0];
    let mut depth = 0i32;
    let mut continuing = false;
    // The indentation of the line the current statement started on.
    let mut statement_indent = 0usize;

    for (at, raw) in source.lines().enumerate() {
        let line = at + 1;
        let trimmed = raw.trim_end();
        let content = trimmed.trim_start();
        if content.is_empty() || content.starts_with("//") {
            continue;
        }
        // A tab counts as one indentation level, like four spaces.
        let width: usize = trimmed
            .chars()
            .take_while(|character| character.is_whitespace())
            .map(|character| if character == '\t' { 4 } else { 1 })
            .sum();
        // `labelText =` then `"a"` then `+ "b"`: a deeper line that starts with an operator
        // continues the statement above it, as Pine reads it.
        let merged = !continuing
            && width > statement_indent
            && begins_with_operator(content)
            && matches!(
                tokens.last().map(|spanned| &spanned.token),
                Some(Token::Newline)
            );
        if merged {
            tokens.pop();
        }
        if !continuing
            && !merged
            && matches!(
                tokens.last().map(|spanned| &spanned.token),
                None | Some(Token::Newline) | Some(Token::Indent) | Some(Token::Dedent)
            )
        {
            statement_indent = width;
        }

        if !continuing && !merged {
            if width > *indents.last().unwrap_or(&0) {
                indents.push(width);
                tokens.push(Spanned {
                    token: Token::Indent,
                    line,
                });
            } else {
                while width < *indents.last().unwrap_or(&0) {
                    indents.pop();
                    tokens.push(Spanned {
                        token: Token::Dedent,
                        line,
                    });
                }
            }
        }

        lex_line(content, line, &mut tokens, &mut depth)?;

        continuing = merged || continues(trimmed, depth);
        if !continuing {
            tokens.push(Spanned {
                token: Token::Newline,
                line,
            });
        }
    }
    while indents.len() > 1 {
        indents.pop();
        tokens.push(Spanned {
            token: Token::Dedent,
            line: source.lines().count(),
        });
    }
    tokens.push(Spanned {
        token: Token::Eof,
        line: source.lines().count(),
    });
    Ok(tokens)
}

fn lex_line(
    line_text: &str,
    line: usize,
    tokens: &mut Vec<Spanned>,
    depth: &mut i32,
) -> Result<(), String> {
    let bytes: Vec<char> = line_text.chars().collect();
    let mut at = 0;
    while at < bytes.len() {
        let c = bytes[at];
        if c.is_whitespace() {
            at += 1;
            continue;
        }
        if c == '/' && bytes.get(at + 1) == Some(&'/') {
            break;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let mut text = String::new();
            at += 1;
            while at < bytes.len() && bytes[at] != quote {
                if bytes[at] == '\\' && at + 1 < bytes.len() {
                    text.push(bytes[at + 1]);
                    at += 2;
                    continue;
                }
                text.push(bytes[at]);
                at += 1;
            }
            if at >= bytes.len() {
                return Err(format!("line {line}: unterminated string"));
            }
            at += 1;
            tokens.push(Spanned {
                token: Token::Str(text),
                line,
            });
            continue;
        }
        if c == '#' {
            let start = at + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            let hex: String = bytes[start..end].iter().collect();
            let value = u32::from_str_radix(&hex, 16)
                .map_err(|_| format!("line {line}: bad colour #{hex}"))?;
            let rgba = match hex.len() {
                6 => value << 8 | 0xFF,
                8 => value,
                _ => return Err(format!("line {line}: colour #{hex} is not 6 or 8 digits")),
            };
            at = end;
            tokens.push(Spanned {
                token: Token::Color(rgba),
                line,
            });
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && bytes.get(at + 1).is_some_and(char::is_ascii_digit)) {
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == '.') {
                at += 1;
            }
            if at < bytes.len() && (bytes[at] == 'e' || bytes[at] == 'E') {
                at += 1;
                if at < bytes.len() && (bytes[at] == '+' || bytes[at] == '-') {
                    at += 1;
                }
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            let text: String = bytes[start..at].iter().collect();
            let number = text
                .parse::<f64>()
                .map_err(|_| format!("line {line}: bad number {text}"))?;
            tokens.push(Spanned {
                token: Token::Number(number),
                line,
            });
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = at;
            while at < bytes.len() && (bytes[at].is_alphanumeric() || bytes[at] == '_') {
                at += 1;
            }
            let name: String = bytes[start..at].iter().collect();
            tokens.push(Spanned {
                token: Token::Ident(name),
                line,
            });
            continue;
        }
        let two: String = bytes[at..(at + 2).min(bytes.len())].iter().collect();
        let op = match two.as_str() {
            "==" | "!=" | "<=" | ">=" | ":=" | "=>" => Some(two.as_str()),
            _ => None,
        };
        if let Some(op) = op {
            at += 2;
            let static_op = match op {
                "==" => "==",
                "!=" => "!=",
                "<=" => "<=",
                ">=" => ">=",
                ":=" => ":=",
                "=>" => "=>",
                _ => unreachable!(),
            };
            tokens.push(Spanned {
                token: Token::Op(static_op),
                line,
            });
            continue;
        }
        let single = match c {
            '+' => Some("+"),
            '-' => Some("-"),
            '*' => Some("*"),
            '/' => Some("/"),
            '%' => Some("%"),
            '<' => Some("<"),
            '>' => Some(">"),
            '=' => Some("="),
            '(' => Some("("),
            ')' => Some(")"),
            '[' => Some("["),
            ']' => Some("]"),
            ',' => Some(","),
            '?' => Some("?"),
            ':' => Some(":"),
            '.' => Some("."),
            _ => None,
        };
        let Some(op) = single else {
            return Err(format!("line {line}: unexpected character {c:?}"));
        };
        match op {
            "(" | "[" => *depth += 1,
            ")" | "]" => *depth = (*depth - 1).max(0),
            _ => {}
        }
        at += 1;
        tokens.push(Spanned {
            token: Token::Op(op),
            line,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_becomes_blocks() {
        let tokens = lex("if a\n    b = 1\nc = 2\n").unwrap();
        let shape: Vec<&str> = tokens
            .iter()
            .map(|spanned| match &spanned.token {
                Token::Ident(_) => "id",
                Token::Number(_) => "num",
                Token::Op(op) => op,
                Token::Newline => "nl",
                Token::Indent => "in",
                Token::Dedent => "de",
                Token::Eof => "eof",
                Token::Str(_) => "str",
                Token::Color(_) => "color",
            })
            .collect();
        assert_eq!(
            shape,
            [
                "id", "id", "nl", "in", "id", "=", "num", "nl", "de", "id", "=", "num", "nl", "eof"
            ]
        );
    }

    #[test]
    fn a_tab_is_four_spaces_of_indentation() {
        // A tab-indented line and its four-space sibling are one block, not two.
        let tokens = lex("x = 1\n\ta = 2\n    b = 3\n").unwrap();
        let indents = tokens
            .iter()
            .filter(|spanned| spanned.token == Token::Indent)
            .count();
        assert_eq!(indents, 1);
    }

    #[test]
    fn strings_colours_and_comments() {
        let tokens = lex("a = \"x // y\" // tail\nb = #0f816e\n").unwrap();
        assert!(
            tokens
                .iter()
                .any(|t| t.token == Token::Str("x // y".into()))
        );
        assert!(tokens.iter().any(|t| t.token == Token::Color(0x0f81_6eff)));
    }

    #[test]
    fn brackets_continue_the_line() {
        let tokens = lex("a = f(\n    1,\n    2)\n").unwrap();
        assert!(!tokens.iter().any(|t| t.token == Token::Indent));
    }
}
