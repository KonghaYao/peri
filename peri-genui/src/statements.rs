use crate::lexer::Token;

pub(crate) fn strip_comments(input: &str) -> String {
    let mut quote = None;
    input
        .split('\n')
        .map(|line| {
            let mut chars = line.char_indices().peekable();
            while let Some((i, c)) = chars.next() {
                if let Some(q) = quote {
                    if c == '\\' {
                        chars.next();
                    } else if c == q {
                        quote = None;
                    }
                } else if matches!(c, '\'' | '"') {
                    quote = Some(c);
                } else if c == '#' || (c == '/' && chars.peek().is_some_and(|(_, c)| *c == '/')) {
                    return line[..i].trim_end();
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn auto_close(input: &str) -> (String, bool) {
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escape = false;
    for c in input.chars() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' && quote.is_some() {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => stack.push(')'),
            '[' => stack.push(']'),
            '{' => stack.push('}'),
            ')' | ']' | '}' if stack.last() == Some(&c) => {
                stack.pop();
            }
            _ => {}
        }
    }
    let incomplete = quote.is_some() || !stack.is_empty();
    let mut out = input.to_owned();
    if let Some(q) = quote {
        if escape {
            out.push('\\');
        }
        out.push(q);
    }
    out.extend(stack.into_iter().rev());
    (out, incomplete)
}

pub(crate) fn split(tokens: &[Token]) -> Vec<(String, bool, Vec<Token>)> {
    let mut statements = Vec::new();
    let mut pos = 0;
    while pos < tokens.len() {
        if tokens.get(pos) == Some(&Token::Newline) {
            pos += 1;
            continue;
        }
        let id = match tokens.get(pos) {
            Some(Token::Name(id) | Token::State(id))
                if tokens.get(pos + 1) == Some(&Token::Symbol("=")) =>
            {
                Some(id.clone())
            }
            _ => None,
        };
        let Some(id) = id else {
            while tokens.get(pos).is_some_and(|t| *t != Token::Newline) {
                pos += 1;
            }
            continue;
        };
        let state = matches!(tokens.get(pos), Some(Token::State(_)));
        pos += 2;
        let mut expr = Vec::new();
        let mut depth: usize = 0;
        let mut ternary: usize = 0;
        while let Some(token) = tokens.get(pos) {
            if *token == Token::Newline && depth == 0 && ternary == 0 {
                let next = tokens[pos + 1..].iter().find(|t| **t != Token::Newline);
                if next != Some(&Token::Symbol("?")) {
                    break;
                }
            }
            match token {
                Token::Newline => {
                    pos += 1;
                    continue;
                }
                Token::Symbol("(" | "[" | "{") => depth += 1,
                Token::Symbol(")" | "]" | "}") => depth = depth.saturating_sub(1),
                Token::Symbol("?") if depth == 0 => ternary += 1,
                Token::Symbol(":") if depth == 0 => ternary = ternary.saturating_sub(1),
                _ => {}
            }
            expr.push(token.clone());
            pos += 1;
        }
        if !expr.is_empty() {
            statements.push((id, state, expr));
        }
    }
    statements
}
