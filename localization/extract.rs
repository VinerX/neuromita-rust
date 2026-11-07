use std::{collections::BTreeMap, fs, path::Path};

#[path = "id.rs"]
mod id;
use id::valid_id;

pub fn extract_sources(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut messages = BTreeMap::new();
    let roots: Vec<String> = serde_json::from_str(
        &fs::read_to_string(root.join("localization/sources.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    for source in roots {
        visit(&root.join(source), &mut messages)?;
    }
    Ok(messages)
}

fn visit(path: &Path, messages: &mut BTreeMap<String, String>) -> Result<(), String> {
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
            visit(&entry.map_err(|e| e.to_string())?.path(), messages)?;
        }
    } else if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "slint")
    ) {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        extract_text(&text, messages).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn extract_text(text: &str, messages: &mut BTreeMap<String, String>) -> Result<(), String> {
    let tokens = tokens(text)?;
    for window in tokens.windows(6) {
        if let [
            Token::Ident(name),
            Token::Punct('('),
            Token::String(id),
            Token::Punct(','),
            Token::String(source),
            Token::Punct(')' | ','),
        ] = window
            && name == "i18n"
        {
            if !valid_id(id) {
                return Err(format!("invalid localization ID {id:?}"));
            }
            if source.is_empty() {
                return Err(format!("empty source for {id}"));
            }
            if let Some(previous) = messages.insert(id.clone(), source.clone())
                && previous != *source
            {
                return Err(format!(
                    "conflicting sources for {id}: {previous:?} / {source:?}"
                ));
            }
        }
    }
    Ok(())
}

enum Token {
    Ident(String),
    String(String),
    Punct(char),
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos..].starts_with(b"//") {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
        } else if bytes[pos..].starts_with(b"/*") {
            pos += 2;
            let mut depth = 1;
            while pos < bytes.len() && depth != 0 {
                if bytes[pos..].starts_with(b"/*") {
                    depth += 1;
                    pos += 2;
                } else if bytes[pos..].starts_with(b"*/") {
                    depth -= 1;
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            if depth != 0 {
                return Err("unterminated block comment".into());
            }
        } else if bytes[pos] == b'r' && raw_string_end(bytes, pos).is_some() {
            let (start, end, next) = raw_string_end(bytes, pos).unwrap();
            result.push(Token::String(text[start..end].into()));
            pos = next;
        } else if bytes[pos] == b'"' {
            let start = pos;
            pos += 1;
            while pos < bytes.len() {
                if bytes[pos] == b'\\' {
                    pos += 2;
                } else if bytes[pos] == b'"' {
                    break;
                } else {
                    pos += 1;
                }
            }
            if pos >= bytes.len() {
                return Err("unterminated string literal".into());
            }
            pos += 1;
            let value = decode_string(&text[start..pos])?;
            result.push(Token::String(value));
        } else if bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'_' {
            let start = pos;
            while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                pos += 1;
            }
            result.push(Token::Ident(text[start..pos].into()));
        } else {
            if !bytes[pos].is_ascii_whitespace() {
                result.push(Token::Punct(bytes[pos] as char));
            }
            pos += 1;
        }
    }
    Ok(result)
}

fn raw_string_end(bytes: &[u8], pos: usize) -> Option<(usize, usize, usize)> {
    let mut quote = pos + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - pos - 1;
    let start = quote + 1;
    for end in start..bytes.len() {
        if bytes[end] == b'"'
            && bytes
                .get(end + 1..end + 1 + hashes)
                .is_some_and(|s| s.iter().all(|b| *b == b'#'))
        {
            return Some((start, end, end + 1 + hashes));
        }
    }
    None
}

fn decode_string(value: &str) -> Result<String, String> {
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            normalized.push(character);
            continue;
        }
        let escaped = chars.next().ok_or("unfinished string escape")?;
        match escaped {
            'u' if chars.peek() == Some(&'{') => {
                chars.next();
                let mut digits = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) if c.is_ascii_hexdigit() => digits.push(c),
                        Some('_') => {}
                        _ => return Err("invalid Unicode escape".into()),
                    }
                }
                let code = u32::from_str_radix(&digits, 16).map_err(|e| e.to_string())?;
                let character = char::from_u32(code).ok_or("invalid Unicode escape")?;
                let json =
                    serde_json::to_string(&character.to_string()).map_err(|e| e.to_string())?;
                normalized.push_str(&json[1..json.len() - 1]);
            }
            '0' => normalized.push_str("\\u0000"),
            '\'' => normalized.push('\''),
            '\n' => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            c => {
                normalized.push('\\');
                normalized.push(c);
            }
        }
    }
    serde_json::from_str(&normalized).map_err(|e| e.to_string())
}
