use crate::MAX_NODES;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    Name(String),
    State(String),
    Call(String),
    String(String),
    Number(f64),
    True,
    False,
    Null,
    Symbol(&'static str),
    Newline,
}

pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if tokens.len() >= MAX_NODES {
            return Err("token 数超过安全预算".into());
        }
        if matches!(c, ' ' | '\t' | '\r') {
            i += 1;
            continue;
        }
        if c == '\n' {
            tokens.push(Token::Newline);
            i += 1;
            continue;
        }
        if c == '#' || (c == '/' && chars.get(i + 1) == Some(&'/')) {
            while chars.get(i).is_some_and(|c| *c != '\n') {
                i += 1;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            let mut single = String::new();
            let mut closed = false;
            while let Some(&ch) = chars.get(i) {
                i += 1;
                if ch == c {
                    closed = true;
                    break;
                }
                if ch == '\\' {
                    if let Some(&escaped) = chars.get(i) {
                        i += 1;
                        if c == '\'' {
                            single.push(match escaped {
                                'n' => '\n',
                                't' => '\t',
                                other => other,
                            });
                        }
                    }
                } else if c == '\'' {
                    single.push(ch);
                }
            }
            let value = if c == '\'' {
                single
            } else {
                let mut raw: String = chars[start..i].iter().collect();
                if !closed {
                    raw.push('"');
                }
                serde_json::from_str::<String>(&raw).unwrap_or_else(|_| {
                    let raw: String = chars[start + 1..i.saturating_sub(usize::from(closed))]
                        .iter()
                        .collect();
                    raw
                })
            };
            tokens.push(Token::String(value));
            continue;
        }
        let after_value = tokens.last().is_some_and(|t| {
            matches!(
                t,
                Token::Name(_)
                    | Token::State(_)
                    | Token::Call(_)
                    | Token::String(_)
                    | Token::Number(_)
                    | Token::True
                    | Token::False
                    | Token::Null
                    | Token::Symbol(")" | "]")
            )
        });
        if c.is_ascii_digit()
            || (c == '-' && !after_value && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            i += usize::from(c == '-');
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
            if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                i += 1;
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
            if chars.get(i).is_some_and(|c| matches!(c, 'e' | 'E')) {
                i += 1;
                if chars.get(i).is_some_and(|c| matches!(c, '+' | '-')) {
                    i += 1;
                }
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
            let raw: String = chars[start..i].iter().collect();
            tokens.push(Token::Number(raw.parse().unwrap_or(f64::NAN)));
            continue;
        }
        if alpha(c) || (matches!(c, '$' | '@') && chars.get(i + 1).is_some_and(|c| alpha(*c))) {
            let start = i;
            i += 1;
            while chars
                .get(i)
                .is_some_and(|c| alpha(*c) || c.is_ascii_digit())
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            tokens.push(match word.as_str() {
                "true" => Token::True,
                "false" => Token::False,
                "null" => Token::Null,
                _ if c == '$' => Token::State(word),
                _ if c == '@' => Token::Call(word[1..].to_owned()),
                _ => Token::Name(word),
            });
            continue;
        }
        let symbol = match c {
            '(' => "(",
            ')' => ")",
            '[' => "[",
            ']' => "]",
            '{' => "{",
            '}' => "}",
            ',' => ",",
            ':' => ":",
            '.' => ".",
            '?' => "?",
            '+' => "+",
            '-' => "-",
            '*' => "*",
            '/' => "/",
            '%' => "%",
            '=' | '!' | '>' | '<' if chars.get(i + 1) == Some(&'=') => {
                i += 1;
                match c {
                    '=' => "==",
                    '!' => "!=",
                    '>' => ">=",
                    _ => "<=",
                }
            }
            '=' => "=",
            '!' => "!",
            '>' => ">",
            '<' => "<",
            '&' | '|' => {
                if chars.get(i + 1) == Some(&c) {
                    i += 1;
                }
                if c == '&' {
                    "&&"
                } else {
                    "||"
                }
            }
            _ => {
                i += 1;
                continue;
            }
        };
        tokens.push(Token::Symbol(symbol));
        i += 1;
    }
    Ok(tokens)
}

fn alpha(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
