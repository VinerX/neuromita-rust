use std::collections::{BTreeMap, BTreeSet};

use super::LocalizationError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalizedText {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) arguments: BTreeMap<String, Argument>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Argument {
    Literal(String),
    Text(Box<LocalizedText>),
}

impl From<String> for Argument {
    fn from(value: String) -> Self {
        Self::Literal(value)
    }
}
impl From<&str> for Argument {
    fn from(value: &str) -> Self {
        Self::Literal(value.into())
    }
}
impl From<LocalizedText> for Argument {
    fn from(value: LocalizedText) -> Self {
        Self::Text(Box::new(value))
    }
}

/// Declares readable source text and a stable ID without selecting a language.
pub fn i18n(id: &str, source: &str) -> LocalizedText {
    LocalizedText {
        id: id.into(),
        source: source.into(),
        arguments: BTreeMap::new(),
    }
}

impl LocalizedText {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn arg(mut self, name: &str, value: impl Into<Argument>) -> Self {
        self.arguments.insert(name.into(), value.into());
        self
    }
}

pub(crate) fn placeholders(template: &str) -> Result<BTreeSet<String>, LocalizationError> {
    let mut names = BTreeSet::new();
    format_template(template, |name| {
        names.insert(name.into());
        Ok(String::new())
    })?;
    Ok(names)
}

pub(crate) fn format_template(
    template: &str,
    mut argument: impl FnMut(&str) -> Result<String, LocalizationError>,
) -> Result<String, LocalizationError> {
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                        _ => return Err(LocalizationError::Template(template.into())),
                    }
                }
                if name.is_empty() {
                    return Err(LocalizationError::Template(template.into()));
                }
                output.push_str(&argument(&name)?);
            }
            '}' => return Err(LocalizationError::Template(template.into())),
            c => output.push(c),
        }
    }
    Ok(output)
}
