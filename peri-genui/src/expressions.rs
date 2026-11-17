use indexmap::IndexMap;

use crate::{builtins::is_builtin, lexer::Token, Expr, MAX_DEPTH};

pub(crate) fn parse_expression(tokens: &[Token]) -> Result<Expr, String> {
    Parser { tokens, pos: 0 }.expr(0, 0).map(|(expr, _)| expr)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl Parser<'_> {
    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }
    fn eat(&mut self, symbol: &str) {
        if matches!(self.current(), Some(Token::Symbol(s)) if *s == symbol) {
            self.pos += 1;
        }
    }
    fn key(&mut self) -> String {
        let key = match self.current() {
            Some(Token::Name(s) | Token::String(s)) => s.clone(),
            Some(Token::State(s)) => s.trim_start_matches('$').to_owned(),
            Some(Token::Number(n)) => n.to_string(),
            _ => "?".into(),
        };
        self.pos += 1;
        key
    }

    fn expr(&mut self, minimum: u8, depth: usize) -> Result<(Expr, usize), String> {
        if depth >= MAX_DEPTH {
            return Err("表达式嵌套超过安全预算".into());
        }
        let (mut left, mut height) = self.prefix(depth + 1)?;
        while let Some(Token::Symbol(op)) = self.current() {
            let op = *op;
            let precedence = precedence(op);
            if precedence <= minimum {
                break;
            }
            self.pos += 1;
            let (next, next_height) = match op {
                "." => (
                    Expr::Member {
                        obj: Box::new(left),
                        field: self.key(),
                    },
                    height + 1,
                ),
                "[" => {
                    let (index, h) = self.expr(0, depth + 1)?;
                    self.eat("]");
                    (
                        Expr::Index {
                            obj: Box::new(left),
                            index: Box::new(index),
                        },
                        height.max(h) + 1,
                    )
                }
                "?" => {
                    let (then_expr, h) = self.expr(0, depth + 1)?;
                    self.eat(":");
                    let (else_expr, eh) = self.expr(0, depth + 1)?;
                    (
                        Expr::Ternary {
                            cond: Box::new(left),
                            then_expr: Box::new(then_expr),
                            else_expr: Box::new(else_expr),
                        },
                        height.max(h).max(eh) + 1,
                    )
                }
                _ => {
                    let (right, h) = self.expr(precedence, depth + 1)?;
                    (
                        Expr::BinOp {
                            op: op.into(),
                            left: Box::new(left),
                            right: Box::new(right),
                        },
                        height.max(h) + 1,
                    )
                }
            };
            if next_height >= MAX_DEPTH {
                return Err("表达式深度超过安全预算".into());
            }
            left = next;
            height = next_height;
        }
        Ok((left, height))
    }

    fn prefix(&mut self, depth: usize) -> Result<(Expr, usize), String> {
        let token = self.current().cloned();
        self.pos += 1;
        let expr = match token {
            Some(Token::String(v)) => Expr::String { v },
            Some(Token::Number(v)) => Expr::Number { v },
            Some(Token::True) => Expr::Bool { v: true },
            Some(Token::False) => Expr::Bool { v: false },
            Some(Token::State(n)) => {
                if self.current() == Some(&Token::Symbol("=")) {
                    self.pos += 1;
                    let (value, h) = self.expr(0, depth)?;
                    return Ok((
                        Expr::Assign {
                            target: n,
                            value: Box::new(value),
                        },
                        h + 1,
                    ));
                }
                Expr::StateRef { n }
            }
            Some(Token::Call(name)) => {
                if self.current() == Some(&Token::Symbol("(")) {
                    let (args, h) = self.list(")", depth)?;
                    let function = !is_builtin(&name);
                    return Ok((Expr::call(name, args, function), h + 1));
                }
                Expr::Ref { n: name }
            }
            Some(Token::Name(name)) => {
                let uppercase = name.starts_with(|c: char| c.is_ascii_uppercase());
                if uppercase
                    && self.current() == Some(&Token::Symbol("("))
                    && (!is_builtin(&name) || name == "Action")
                {
                    let (args, h) = self.list(")", depth)?;
                    return Ok((Expr::call(name, args, false), h + 1));
                }
                Expr::Ref { n: name }
            }
            Some(Token::Symbol("!" | "-")) => {
                let op = if token == Some(Token::Symbol("!")) {
                    "!"
                } else {
                    "-"
                };
                let (operand, h) = self.expr(8, depth)?;
                return Ok((
                    Expr::UnaryOp {
                        op: op.into(),
                        operand: Box::new(operand),
                    },
                    h + 1,
                ));
            }
            Some(Token::Symbol("(")) => {
                let inner = self.expr(0, depth)?;
                self.eat(")");
                return Ok(inner);
            }
            Some(Token::Symbol("[")) => {
                self.pos -= 1;
                let (els, h) = self.list("]", depth)?;
                return Ok((Expr::Array { els }, h + 1));
            }
            Some(Token::Symbol("{")) => {
                let mut entries = IndexMap::new();
                let mut height = 0;
                while self.current().is_some() && self.current() != Some(&Token::Symbol("}")) {
                    let key = self.key();
                    self.eat(":");
                    let (value, h) = self.expr(0, depth)?;
                    height = height.max(h);
                    entries.insert(key, value);
                    self.eat(",");
                }
                self.eat("}");
                return Ok((Expr::Object { entries }, height + 1));
            }
            _ => Expr::Null,
        };
        Ok((expr, 1))
    }

    fn list(&mut self, close: &str, depth: usize) -> Result<(Vec<Expr>, usize), String> {
        self.pos += 1;
        let mut args = Vec::new();
        let mut height = 0;
        while self.current().is_some()
            && !matches!(self.current(), Some(Token::Symbol(s)) if *s == close)
        {
            let (arg, h) = self.expr(0, depth)?;
            height = height.max(h);
            args.push(arg);
            self.eat(",");
        }
        self.eat(close);
        Ok((args, height))
    }
}

fn precedence(op: &str) -> u8 {
    match op {
        "?" => 1,
        "||" => 2,
        "&&" => 3,
        "==" | "!=" => 4,
        ">" | "<" | ">=" | "<=" => 5,
        "+" | "-" => 6,
        "*" | "/" | "%" => 7,
        "." | "[" => 9,
        _ => 0,
    }
}
