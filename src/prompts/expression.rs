use std::collections::BTreeMap;

use serde_json::{Number, Value};

use super::PromptError;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Name(String),
    Literal(Value),
    Operator(String),
    Left,
    Right,
    Comma,
    End,
}

fn lex(input: &str) -> Result<Vec<Token>, PromptError> {
    let mut out = Vec::new();
    let mut it = input.char_indices().peekable();
    while let Some((pos, ch)) = it.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut end = pos + ch.len_utf8();
            while let Some(&(idx, next)) = it.peek() {
                if !next.is_ascii_alphanumeric() && next != '_' {
                    break;
                }
                it.next();
                end = idx + next.len_utf8();
            }
            let text = &input[pos..end];
            out.push(match text {
                "True" | "true" => Token::Literal(Value::Bool(true)),
                "False" | "false" => Token::Literal(Value::Bool(false)),
                "None" | "none" => Token::Literal(Value::Null),
                "and" | "AND" | "or" | "OR" | "not" | "NOT" => Token::Operator(text.to_lowercase()),
                _ => Token::Name(text.to_owned()),
            });
            continue;
        }
        if ch.is_ascii_digit() {
            let mut end = pos + 1;
            while let Some(&(idx, next)) = it.peek() {
                if !next.is_ascii_digit() && next != '.' {
                    break;
                }
                it.next();
                end = idx + next.len_utf8();
            }
            let value = serde_json::from_str(&input[pos..end])
                .map_err(|_| PromptError::Dsl(format!("invalid number: {}", &input[pos..end])))?;
            out.push(Token::Literal(value));
            continue;
        }
        if ch == '\'' || ch == '"' {
            let mut ahead = it.clone();
            let multiline = ahead.next().is_some_and(|(_, c)| c == ch)
                && ahead.next().is_some_and(|(_, c)| c == ch);
            if multiline {
                it.next();
                it.next();
            }
            let mut escaped = false;
            let mut value = String::new();
            let mut closed = false;
            while let Some((_, next)) = it.next() {
                if escaped {
                    value.push(match next {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                } else if next == ch {
                    if !multiline {
                        closed = true;
                        break;
                    }
                    let mut ahead = it.clone();
                    if ahead.next().is_some_and(|(_, c)| c == ch)
                        && ahead.next().is_some_and(|(_, c)| c == ch)
                    {
                        it.next();
                        it.next();
                        closed = true;
                        break;
                    }
                    value.push(ch);
                } else {
                    value.push(next);
                }
            }
            if !closed {
                return Err(PromptError::Dsl("unterminated string literal".into()));
            }
            out.push(Token::Literal(Value::String(value)));
            continue;
        }
        match ch {
            '(' => out.push(Token::Left),
            ')' => out.push(Token::Right),
            ',' => out.push(Token::Comma),
            '+' | '-' | '*' | '/' | '%' | '<' | '>' | '=' | '!' => {
                let mut text = ch.to_string();
                if let Some(&(_, '=')) = it.peek()
                    && matches!(ch, '<' | '>' | '=' | '!')
                {
                    it.next();
                    text.push('=');
                }
                out.push(Token::Operator(text));
            }
            _ => {
                return Err(PromptError::Dsl(format!(
                    "invalid expression token at {pos}: {ch}"
                )));
            }
        }
    }
    out.push(Token::End);
    Ok(out)
}

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => {
            if *value {
                "True".into()
            } else {
                "False".into()
            }
        }
        _ => value.to_string(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

pub(super) fn evaluate(expr: &str, vars: &BTreeMap<String, Value>) -> Result<Value, PromptError> {
    let trimmed = expr.trim();
    if trimmed.starts_with("f\"") || trimmed.starts_with("f'") {
        return f_string(&trimmed[1..], vars).map(Value::String);
    }
    let tokens = lex(trimmed)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        vars,
    };
    let result = parser.parse(0)?;
    if parser.current() != &Token::End {
        return Err(PromptError::Dsl(format!("unconsumed expression: {expr}")));
    }
    Ok(result)
}

fn f_string(quoted: &str, vars: &BTreeMap<String, Value>) -> Result<String, PromptError> {
    let literal = match lex(quoted)?.first() {
        Some(Token::Literal(Value::String(value))) => value.clone(),
        _ => return Err(PromptError::Dsl("invalid f-string".into())),
    };
    let mut result = String::new();
    let mut chars = literal.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            result.push(ch);
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            result.push('{');
            continue;
        }
        let mut expr = String::new();
        let mut finished = false;
        for next in chars.by_ref() {
            if next == '}' {
                finished = true;
                break;
            }
            expr.push(next);
        }
        if !finished {
            return Err(PromptError::Dsl("unterminated f-string variable".into()));
        }
        result.push_str(&text(&evaluate(&expr, vars)?));
    }
    Ok(result.replace("}}", "}"))
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    vars: &'a BTreeMap<String, Value>,
}
impl Parser<'_> {
    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn next(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }
    fn parse(&mut self, min_bp: u8) -> Result<Value, PromptError> {
        let mut left =
            match self.next() {
                Token::Literal(value) => value,
                Token::Name(name) => {
                    if self.current() == &Token::Left {
                        self.next();
                        let mut args = Vec::new();
                        if self.current() != &Token::Right {
                            loop {
                                args.push(self.parse(0)?);
                                if self.current() != &Token::Comma {
                                    break;
                                }
                                self.next();
                            }
                        }
                        if self.next() != Token::Right {
                            return Err(PromptError::Dsl("function call missing ')'".into()));
                        }
                        call(&name, args)?
                    } else {
                        self.vars.get(&name).cloned().unwrap_or(Value::Null)
                    }
                }
                Token::Left => {
                    let value = self.parse(0)?;
                    if self.next() != Token::Right {
                        return Err(PromptError::Dsl("missing ')'".into()));
                    }
                    value
                }
                Token::Operator(op) if op == "not" || op == "-" || op == "+" => {
                    let value = self.parse(11)?;
                    match op.as_str() {
                        "not" => Value::Bool(!truthy(&value)),
                        "+" => value,
                        _ => number(-value.as_f64().ok_or_else(|| {
                            PromptError::Dsl("unary '-' requires number".into())
                        })?)?,
                    }
                }
                other => {
                    return Err(PromptError::Dsl(format!(
                        "expected expression, found {other:?}"
                    )));
                }
            };
        while let Token::Operator(op) = self.current() {
            let (left_bp, right_bp) = match op.as_str() {
                "or" => (1, 2),
                "and" => (3, 4),
                "==" | "!=" | "<" | ">" | "<=" | ">=" => (5, 6),
                "+" | "-" => (7, 8),
                "*" | "/" | "%" => (9, 10),
                _ => return Err(PromptError::Dsl(format!("unknown operator: {op}"))),
            };
            if left_bp < min_bp {
                break;
            }
            let op = op.clone();
            self.next();
            let right = self.parse(right_bp)?;
            left = binary(&op, left, right)?;
        }
        Ok(left)
    }
}
fn number(value: f64) -> Result<Value, PromptError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| PromptError::Dsl("non-finite numeric result".into()))
}
fn binary(op: &str, left: Value, right: Value) -> Result<Value, PromptError> {
    if op == "and" {
        return Ok(Value::Bool(truthy(&left) && truthy(&right)));
    }
    if op == "or" {
        return Ok(Value::Bool(truthy(&left) || truthy(&right)));
    }
    match op {
        "==" => return Ok(Value::Bool(left == right)),
        "!=" => return Ok(Value::Bool(left != right)),
        _ => {}
    }
    if op == "+" && (left.is_string() || right.is_string()) {
        return Ok(Value::String(format!("{}{}", text(&left), text(&right))));
    }
    let a = left
        .as_f64()
        .ok_or_else(|| PromptError::Dsl(format!("{op} requires numeric arguments")))?;
    let b = right
        .as_f64()
        .ok_or_else(|| PromptError::Dsl(format!("{op} requires numeric arguments")))?;
    Ok(match op {
        "<" => Value::Bool(a < b),
        ">" => Value::Bool(a > b),
        "<=" => Value::Bool(a <= b),
        ">=" => Value::Bool(a >= b),
        "+" => number(a + b)?,
        "-" => number(a - b)?,
        "*" => number(a * b)?,
        "/" => {
            if b == 0.0 {
                return Err(PromptError::Dsl("division by zero".into()));
            }
            number(a / b)?
        }
        "%" => {
            if b == 0.0 {
                return Err(PromptError::Dsl("modulo by zero".into()));
            }
            number(a % b)?
        }
        _ => return Err(PromptError::Dsl(format!("invalid operator: {op}"))),
    })
}
fn call(name: &str, args: Vec<Value>) -> Result<Value, PromptError> {
    match (name, args.as_slice()) {
        ("str", [v]) => Ok(Value::String(text(v))),
        ("int", [v]) => v
            .as_i64()
            .or_else(|| text(v).parse().ok())
            .map(|v| Value::Number(v.into()))
            .ok_or_else(|| PromptError::Dsl("int() conversion failed".into())),
        ("float", [v]) => v
            .as_f64()
            .or_else(|| text(v).parse().ok())
            .ok_or_else(|| PromptError::Dsl("float() conversion failed".into()))
            .and_then(number),
        ("len", [Value::String(v)]) => Ok(Value::Number(v.chars().count().into())),
        ("len", [Value::Array(v)]) => Ok(Value::Number(v.len().into())),
        ("abs", [v]) => number(
            v.as_f64()
                .ok_or_else(|| PromptError::Dsl("abs() expects number".into()))?
                .abs(),
        ),
        ("round", [v]) => number(
            v.as_f64()
                .ok_or_else(|| PromptError::Dsl("round() expects number".into()))?
                .round(),
        ),
        ("round", [v, digits]) => {
            let digits = digits
                .as_i64()
                .ok_or_else(|| PromptError::Dsl("round() digits must be integer".into()))?;
            let factor = 10_f64.powi(
                i32::try_from(digits)
                    .map_err(|_| PromptError::Dsl("round() digits out of range".into()))?,
            );
            number(
                (v.as_f64()
                    .ok_or_else(|| PromptError::Dsl("round() expects number".into()))?
                    * factor)
                    .round()
                    / factor,
            )
        }
        ("min", [a, b]) => binary("<", a.clone(), b.clone()).map(|v| {
            if v == Value::Bool(true) {
                a.clone()
            } else {
                b.clone()
            }
        }),
        ("max", [a, b]) => binary(">", a.clone(), b.clone()).map(|v| {
            if v == Value::Bool(true) {
                a.clone()
            } else {
                b.clone()
            }
        }),
        _ => Err(PromptError::Dsl(format!(
            "unsupported function or arguments: {name}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evaluates_python_like_basics() {
        let vars = BTreeMap::from([
            ("attitude".into(), Value::from(34)),
            ("name".into(), Value::from("Mita")),
        ]);
        assert_eq!(
            evaluate("attitude < 40 AND name == 'Mita'", &vars).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate("'hello ' + name", &vars).unwrap(),
            Value::String("hello Mita".into())
        );
        assert_eq!(
            evaluate("f'Hey {name}: {attitude}'", &vars).unwrap(),
            Value::String("Hey Mita: 34".into())
        );
        assert!(evaluate("__import__('os')", &vars).is_err());
        assert_eq!(
            evaluate("undefined == None", &vars).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            evaluate("f\"\"\"Hi {name}\nBye\"\"\"", &vars).unwrap(),
            Value::String("Hi Mita\nBye".into())
        );
    }
}
