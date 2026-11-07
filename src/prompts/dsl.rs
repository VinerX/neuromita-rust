use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::{
    PromptError, PromptInitializationDeclaration,
    expression::{evaluate, text},
};

const MAX_DEPTH: usize = 10;
const MAX_FILE_SIZE: u64 = 1_000_000;

pub(super) struct Interpreter {
    root: PathBuf,
    base: PathBuf,
    vars: BTreeMap<String, Value>,
    static_infos: Vec<String>,
    context_infos: Vec<String>,
    recursion: Vec<PathBuf>,
    declarations: Vec<PromptInitializationDeclaration>,
}

#[derive(Default)]
struct Branch {
    parent_active: bool,
    taken: bool,
    active: bool,
}

impl Interpreter {
    pub fn new(
        root: &Path,
        base: &Path,
        vars: BTreeMap<String, Value>,
    ) -> Result<Self, PromptError> {
        Ok(Self {
            root: root.canonicalize()?,
            base: base.canonicalize()?,
            vars,
            static_infos: Vec::new(),
            context_infos: Vec::new(),
            recursion: Vec::new(),
            declarations: Vec::new(),
        })
    }

    pub fn declarations(&self) -> Vec<PromptInitializationDeclaration> {
        self.declarations.clone()
    }

    fn resolve(&self, file: &str, parent: Option<&Path>) -> Result<PathBuf, PromptError> {
        if file.is_empty() || Path::new(file).is_absolute() || file.contains('\0') {
            return Err(PromptError::Dsl(format!("invalid template path: {file}")));
        }
        let base = if file.starts_with("_CommonPrompts/") || file.starts_with("_CommonScripts/") {
            &self.root
        } else if file.starts_with("./") || file.starts_with("../") {
            parent.unwrap_or(&self.base)
        } else {
            &self.base
        };
        let source = base.join(file);
        let result = source.canonicalize().map_err(|error| {
            PromptError::Dsl(format!("could not resolve {}: {error}", source.display()))
        })?;
        if !result.starts_with(&self.root) || !result.is_file() {
            return Err(PromptError::Dsl(format!(
                "template escaped prompt root: {file}"
            )));
        }
        Ok(result)
    }

    fn read(&mut self, path: &Path) -> Result<String, PromptError> {
        if self.recursion.len() >= MAX_DEPTH {
            return Err(PromptError::Dsl("maximum DSL include depth reached".into()));
        }
        if self.recursion.iter().any(|item| item == path) {
            return Err(PromptError::Dsl(format!(
                "recursive template include: {}",
                path.display()
            )));
        }
        if fs::metadata(path)?.len() > MAX_FILE_SIZE {
            return Err(PromptError::Dsl("prompt file too large".into()));
        }
        self.recursion.push(path.to_owned());
        Ok(fs::read_to_string(path)?)
    }

    fn file(&mut self, name: &str, parent: Option<&Path>) -> Result<String, PromptError> {
        let path = self.resolve(name, parent)?;
        let content = self.read(&path)?;
        let result = match path.extension().and_then(|ext| ext.to_str()) {
            Some("txt") => self.expand(&content, path.parent().unwrap(), true),
            Some("script") | Some("system") => self.script(&content, &path),
            _ => Err(PromptError::Dsl(format!("unsupported prompt file: {name}"))),
        };
        self.recursion.pop();
        result
    }

    pub fn render_main(
        &mut self,
        template: &str,
    ) -> Result<(Vec<String>, Vec<String>), PromptError> {
        let path = self.resolve(template, None)?;
        let content = self.read(&path)?;
        let result = (|| {
            let mut blocks = Vec::new();
            for line in content.lines() {
                let value = line.trim();
                if let Some((name, raw)) = value.split_once('=')
                    && matches!(
                        name.trim().to_ascii_lowercase().as_str(),
                        "support_intents" | "behavior_state"
                    )
                {
                    let feature = name.trim().to_ascii_lowercase();
                    let setting = raw.trim().split("//").next().unwrap_or_default().trim();
                    let value = match setting.to_ascii_lowercase().as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => Value::String(setting.to_ascii_lowercase()),
                    };
                    self.vars.insert(feature, value);
                }
            }
            self.vars
                .entry("support_intents".into())
                .or_insert(Value::Bool(true));
            let non_comments = content
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let mut rest = non_comments.as_str();
            while let Some(start) = rest.find("[<") {
                rest = &rest[start + 2..];
                let end = rest
                    .find(">]")
                    .ok_or_else(|| PromptError::Dsl("unfinished template include".into()))?;
                let body = rest[..end].trim();
                let is_volatile = body.starts_with('@');
                let name = body.trim_start_matches('@').trim();
                let value = self.file(name, path.parent())?;
                if !value.trim().is_empty() {
                    if is_volatile {
                        self.context_infos.push(value);
                    } else {
                        blocks.push(value);
                    }
                }
                rest = &rest[end + 2..];
            }
            blocks.append(&mut self.static_infos);
            Ok((blocks, std::mem::take(&mut self.context_infos)))
        })();
        self.recursion.pop();
        result
    }

    fn expand(
        &mut self,
        input: &str,
        parent: &Path,
        text_template: bool,
    ) -> Result<String, PromptError> {
        let mut value = input.to_owned();
        let mut offset = 0;
        while let Some(start_rel) = value[offset..].find("[<") {
            let start = offset + start_rel;
            let end = value[start..]
                .find(">]")
                .ok_or_else(|| PromptError::Dsl("unclosed include".into()))?
                + start;
            let include = value[start + 2..end].trim();
            let is_volatile = include.starts_with('@');
            let name = include.trim_start_matches('@').trim();
            let base = if text_template && name.starts_with("../") {
                self.base.clone()
            } else {
                parent.to_owned()
            };
            let replacement = self.file(name, Some(&base))?;
            let replacement_len = replacement.len();
            if is_volatile {
                if !replacement.trim().is_empty() {
                    self.context_infos.push(replacement);
                }
                value.replace_range(start..end + 2, "");
            } else {
                value.replace_range(start..end + 2, &replacement);
            }
            offset = start + if is_volatile { 0 } else { replacement_len };
            if offset >= value.len() {
                break;
            }
        }
        let mut offset = 0;
        while let Some(start_rel) = value[offset..].find("[{") {
            let start = offset + start_rel;
            let Some(end_rel) = value[start + 2..].find("}]") else {
                return Err(PromptError::Dsl("unclosed variable".into()));
            };
            let end = start + 2 + end_rel;
            let name = value[start + 2..end].trim();
            let replacement = self
                .vars
                .get(name)
                .filter(|v| !v.is_null())
                .map(text)
                .unwrap_or_default();
            value.replace_range(start..end + 2, &replacement);
            offset = start + replacement.len();
            if offset >= value.len() {
                break;
            }
        }
        Ok(value.trim_end().to_owned())
    }

    fn evaluate(&mut self, input: &str, parent: &Path) -> Result<Value, PromptError> {
        let mut source = input.trim().to_owned();
        let mut cursor = 0;
        let mut replacements = 0;
        while let Some(found) = source[cursor..].find("LOAD ") {
            let start = cursor + found;
            if start > 0
                && source[..start]
                    .chars()
                    .last()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                cursor = start + "LOAD ".len();
                continue;
            }
            let remaining = &source[start + "LOAD ".len()..];
            let Some((tag, file)) = remaining.split_once(" FROM ") else {
                cursor = start + "LOAD ".len();
                continue;
            };
            if !tag
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                cursor = start + "LOAD ".len();
                continue;
            }
            let file = file.trim_start();
            let Some(quote) = file.chars().next().filter(|ch| *ch == '\'' || *ch == '"') else {
                return Err(PromptError::Dsl(
                    "inline LOAD FROM expects quoted file".into(),
                ));
            };
            let tail = &file[1..];
            let end = tail
                .find(quote)
                .ok_or_else(|| PromptError::Dsl("unterminated inline LOAD FROM path".into()))?;
            let literal = serde_json::to_string(&self.tagged(tag.trim(), &tail[..end], parent)?)?;
            let consumed = "LOAD ".len() + remaining.len() - file.len() + end + 2;
            source.replace_range(start..start + consumed, &literal);
            cursor = start + literal.len();
            replacements += 1;
            if replacements > 64 {
                return Err(PromptError::Dsl("too many inline LOAD expressions".into()));
            }
        }
        evaluate(&source, &self.vars)
    }

    fn argument(&mut self, arg: &str, parent: &Path) -> Result<String, PromptError> {
        let arg = arg.trim();
        let upper = arg.to_ascii_uppercase();
        if upper.starts_with("LOAD_REL ")
            || upper.starts_with("LOADREL ")
            || upper.starts_with("LOAD ")
        {
            let name = arg
                .split_once(' ')
                .unwrap()
                .1
                .trim()
                .trim_matches(['\'', '"']);
            if let Some((tag, file)) = name.split_once(" FROM ") {
                return self.tagged(tag.trim(), file.trim().trim_matches(['\'', '"']), parent);
            }
            return self.file(name, Some(parent));
        }
        Ok(text(&self.evaluate(arg, parent)?))
    }

    fn tagged(&mut self, tag: &str, file: &str, parent: &Path) -> Result<String, PromptError> {
        let path = self.resolve(file, Some(parent))?;
        let raw = fs::read_to_string(&path)?;
        let start = format!("[#{}]", tag.to_ascii_uppercase());
        let end = format!("[/{}]", tag.to_ascii_uppercase());
        let upper = raw.to_ascii_uppercase();
        let begin = upper
            .find(&start)
            .ok_or_else(|| PromptError::Dsl(format!("tag {tag} missing")))?
            + start.len();
        let stop = upper[begin..]
            .find(&end)
            .ok_or_else(|| PromptError::Dsl(format!("closing tag {tag} missing")))?
            + begin;
        self.expand(raw[begin..stop].trim(), path.parent().unwrap(), true)
    }

    fn script(&mut self, input: &str, path: &Path) -> Result<String, PromptError> {
        let mut branches: Vec<Branch> = Vec::new();
        let parent = path
            .parent()
            .ok_or_else(|| PromptError::Dsl("script has no parent".into()))?;
        for (line_idx, line) in logical_lines(input)?.into_iter().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let clean = line.split(" //").next().unwrap_or_default().trim();
            let (command, arg) = clean.split_once(char::is_whitespace).unwrap_or((clean, ""));
            let command = command.to_ascii_uppercase();
            let active = branches.last().is_none_or(|branch| branch.active);
            let condition =
                |arg: &str, vars: &BTreeMap<String, Value>| -> Result<bool, PromptError> {
                    let arg = arg.trim().strip_suffix(" THEN").unwrap_or(arg.trim());
                    let value = evaluate(arg, vars)?;
                    Ok(match value {
                        Value::Null => false,
                        Value::Bool(x) => x,
                        Value::Number(ref x) => x.as_f64().unwrap_or_default() != 0.0,
                        Value::String(ref x) => !x.is_empty(),
                        _ => true,
                    })
                };
            let result = (|| -> Result<Option<String>, PromptError> {
                match command.as_str() {
                    "IF" => {
                        let taken = active && condition(arg, &self.vars)?;
                        branches.push(Branch {
                            parent_active: active,
                            taken,
                            active: taken,
                        });
                    }
                    "ELSEIF" => {
                        let branch = branches
                            .last_mut()
                            .ok_or_else(|| PromptError::Dsl("ELSEIF without IF".into()))?;
                        branch.active =
                            branch.parent_active && !branch.taken && condition(arg, &self.vars)?;
                        branch.taken |= branch.active;
                    }
                    "ELSE" => {
                        let branch = branches
                            .last_mut()
                            .ok_or_else(|| PromptError::Dsl("ELSE without IF".into()))?;
                        branch.active = branch.parent_active && !branch.taken;
                        branch.taken = true;
                    }
                    "ENDIF" => {
                        branches
                            .pop()
                            .ok_or_else(|| PromptError::Dsl("ENDIF without IF".into()))?;
                    }
                    _ if !active => {}
                    "SET" => {
                        let arg = arg.trim().strip_prefix("LOCAL ").unwrap_or(arg.trim());
                        let (name, expr) = arg
                            .split_once('=')
                            .ok_or_else(|| PromptError::Dsl("SET requires '='".into()))?;
                        let name = name.trim();
                        if name.is_empty()
                            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        {
                            return Err(PromptError::Dsl("invalid SET name".into()));
                        }
                        let value = self.evaluate(expr, parent)?;
                        self.vars.insert(name.into(), value);
                    }
                    "RETURN" => return Ok(Some(self.argument(arg, parent)?)),
                    "RUN" => {
                        self.file(arg.trim().trim_matches(['\'', '"']), Some(parent))?;
                    }
                    "ADD_SYSTEM_INFO" => {
                        let content = self.argument(arg, parent)?;
                        if !content.trim().is_empty() {
                            self.static_infos.push(content);
                        }
                    }
                    "ADD_CONTEXT_INFO" => {
                        let content = self.argument(arg, parent)?;
                        if !content.trim().is_empty() {
                            self.context_infos.push(content);
                        }
                    }
                    "LOG" => {}
                    "SEED_MEMORY" | "SEED_RAG_MEMORY" => {
                        let (priority, remaining) = arg.split_once('|').ok_or_else(|| {
                            PromptError::Dsl(format!("{command} requires priority | content"))
                        })?;
                        let upper = remaining.to_ascii_uppercase();
                        let (content, entities) = if let Some(index) = upper.find(" ENTITIES ") {
                            (
                                &remaining[..index],
                                remaining[index + " ENTITIES ".len()..]
                                    .split(',')
                                    .map(|part| part.trim().to_owned())
                                    .filter(|part| !part.is_empty())
                                    .collect(),
                            )
                        } else {
                            (remaining, Vec::new())
                        };
                        let content = content.trim().to_owned();
                        if !content.is_empty() {
                            self.declarations
                                .push(PromptInitializationDeclaration::Memory {
                                    priority: priority.trim().into(),
                                    content,
                                    entities,
                                    rag_only: command == "SEED_RAG_MEMORY",
                                });
                        }
                    }
                    "SEED_ISLAND" => {
                        let (kind, content) = arg.split_once('|').ok_or_else(|| {
                            PromptError::Dsl("SEED_ISLAND requires type | content".into())
                        })?;
                        if !content.trim().is_empty() {
                            self.declarations
                                .push(PromptInitializationDeclaration::Island {
                                    kind: kind.trim().into(),
                                    content: content.trim().into(),
                                });
                        }
                    }
                    "LINK_ENTITIES" => {
                        let parts = arg.split("->").map(str::trim).collect::<Vec<_>>();
                        if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
                            return Err(PromptError::Dsl(
                                "LINK_ENTITIES requires subject -> relation -> object".into(),
                            ));
                        }
                        self.declarations
                            .push(PromptInitializationDeclaration::Relation {
                                subject: parts[0].into(),
                                relation: parts[1].into(),
                                object: parts[2].into(),
                            });
                    }
                    _ => return Err(PromptError::Dsl(format!("unknown DSL command {command}"))),
                }
                Ok(None)
            })();
            match result {
                Ok(Some(text)) => return self.expand(&text, parent, false),
                Ok(None) => {}
                Err(err) => {
                    return Err(PromptError::Dsl(format!(
                        "{}:{}: {err}",
                        path.display(),
                        line_idx + 1
                    )));
                }
            }
        }
        if !branches.is_empty() {
            return Err(PromptError::Dsl(format!(
                "unterminated IF in {}",
                path.display()
            )));
        }
        Ok(String::new())
    }
}

fn logical_lines(input: &str) -> Result<Vec<String>, PromptError> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut multiline = false;
    let mut pos = 0;
    while pos < input.len() {
        if input[pos..].starts_with("\"\"\"") {
            multiline = !multiline;
            current.push_str("\"\"\"");
            pos += 3;
        } else {
            let ch = input[pos..].chars().next().unwrap();
            pos += ch.len_utf8();
            if ch == '\n' && !multiline {
                lines.push(std::mem::take(&mut current));
            } else {
                current.push(ch);
            }
        }
    }
    if multiline {
        return Err(PromptError::Dsl("unterminated multiline literal".into()));
    }
    if !current.is_empty() {
        lines.push(current);
    }
    Ok(lines)
}
