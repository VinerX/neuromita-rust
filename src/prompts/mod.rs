mod dsl;
mod expression;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;
use thiserror::Error;

use crate::character::CharacterId;

#[derive(Debug, Error)]
pub enum PromptError {
    #[error("prompt filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("prompt metadata: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("invalid prompt selection: {0}")]
    InvalidSelection(String),
    #[error("prompt set is missing: {0}")]
    MissingSet(String),
    #[error("prompt DSL: {0}")]
    Dsl(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSetInfo {
    pub character: CharacterId,
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptInitializationDeclaration {
    Memory {
        priority: String,
        content: String,
        entities: Vec<String>,
        rag_only: bool,
    },
    Island {
        kind: String,
        content: String,
    },
    Relation {
        subject: String,
        relation: String,
        object: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPrompt {
    pub set_name: String,
    pub stable: Vec<String>,
    pub context: Vec<String>,
    /// Declarative initialization operations; rendering never writes memory.
    pub initialization: Vec<PromptInitializationDeclaration>,
}

impl RenderedPrompt {
    pub fn system_text(&self) -> String {
        self.stable
            .iter()
            .chain(&self.context)
            .filter(|x| !x.trim().is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[derive(Debug, Clone)]
pub struct PromptCatalog {
    root: PathBuf,
}

impl PromptCatalog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn validate_set_name(name: &str) -> Result<(), PromptError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.starts_with('.')
            || name.contains('/')
            || name.contains('\\')
            || name.chars().any(char::is_control)
        {
            return Err(PromptError::InvalidSelection(name.to_owned()));
        }
        Ok(())
    }

    pub fn list(&self, character: CharacterId) -> Result<Vec<PromptSetInfo>, PromptError> {
        let directory = self.root.join(character.to_string());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut sets = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if Self::validate_set_name(&name).is_err() || name == "System" || name == "__pycache__"
            {
                continue;
            }
            if !entry.file_type()?.is_dir() || !entry.path().join("main_template.txt").is_file() {
                continue;
            }
            let mut author = String::new();
            let mut version = String::new();
            let mut description = String::new();
            for file in ["info.json", "config.json"] {
                let path = entry.path().join(file);
                if path.is_file() {
                    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
                    let metadata = value.get("metadata").unwrap_or(&value);
                    if author.is_empty() {
                        author = metadata
                            .get("author")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                    }
                    if version.is_empty() {
                        version = metadata
                            .get("version")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                    }
                    if description.is_empty() {
                        description = metadata
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into();
                    }
                }
            }
            sets.push(PromptSetInfo {
                character,
                name,
                author,
                version,
                description,
            });
        }
        sets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(sets)
    }

    pub fn resolve_set(
        &self,
        character: CharacterId,
        selected: Option<&str>,
    ) -> Result<String, PromptError> {
        let options = self.list(character)?;
        let name = if let Some(name) = selected {
            Self::validate_set_name(name)?;
            name.to_owned()
        } else {
            options
                .iter()
                .find(|set| set.name.eq_ignore_ascii_case("Default"))
                .or_else(|| options.first())
                .ok_or_else(|| PromptError::MissingSet(character.to_string()))?
                .name
                .clone()
        };
        if !options.iter().any(|set| set.name == name) {
            return Err(PromptError::MissingSet(format!("{character}/{name}")));
        }
        Ok(name)
    }

    pub fn render(
        &self,
        character: CharacterId,
        selected: Option<&str>,
        context_variables: BTreeMap<String, Value>,
        template: &str,
    ) -> Result<RenderedPrompt, PromptError> {
        let set_name = self.resolve_set(character, selected)?;
        let dir = self.root.join(character.to_string()).join(&set_name);
        let config_path = dir.join("config.json");
        let mut vars = BTreeMap::new();
        if config_path.is_file() {
            let value: Value = serde_json::from_slice(&fs::read(config_path)?)?;
            let config = value
                .as_object()
                .ok_or_else(|| PromptError::Dsl("config.json must be an object".into()))?;
            let defaults = config
                .get("variables")
                .and_then(Value::as_object)
                .unwrap_or(config);
            for (key, value) in defaults {
                if key != "metadata"
                    && !matches!(key.as_str(), "author" | "version" | "description")
                {
                    vars.insert(key.clone(), value.clone());
                }
            }
        }
        vars.extend(context_variables);
        vars.insert("PROMPT_SET_NAME".into(), Value::String(set_name.clone()));
        let mut runtime = dsl::Interpreter::new(&self.root, &dir, vars)?;
        let rendered = runtime.render_main(template)?;
        Ok(RenderedPrompt {
            set_name,
            stable: rendered.0,
            context: rendered.1,
            initialization: runtime.declarations(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::CharacterId;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "neuromita-prompt-test-{}-{unique}",
            std::process::id()
        ));
        let set = root.join("Crazy/Default");
        fs::create_dir_all(set.join("Main")).unwrap();
        fs::write(set.join("config.json"), r#"{"attitude":60,"player_name":"Игрок","metadata":{"author":"A","version":"2","description":"Example"}}"#).unwrap();
        fs::write(
            set.join("main_template.txt"),
            "[<Main/identity.txt>]\n[<@ Main/state.script>]\n",
        )
        .unwrap();
        fs::write(set.join("Main/identity.txt"), "Привет [{player_name}].").unwrap();
        fs::write(
            set.join("Main/state.script"),
            "IF attitude < 40 THEN\n RETURN 'Злюсь'\nELSE\n RETURN 'Спокойна'\nENDIF\n",
        )
        .unwrap();
        root
    }
    #[test]
    fn list_select_and_render_are_isolated_by_request() {
        let dir = root();
        let catalog = PromptCatalog::new(&dir);
        let options = catalog.list(CharacterId::Crazy).unwrap();
        assert_eq!(options[0].author, "A");
        let first = catalog
            .render(
                CharacterId::Crazy,
                None,
                BTreeMap::new(),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(first.stable, vec!["Привет Игрок."]);
        assert_eq!(first.context, vec!["Спокойна"]);
        let second = catalog
            .render(
                CharacterId::Crazy,
                Some("Default"),
                BTreeMap::from([("attitude".into(), Value::from(10))]),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(second.context, vec!["Злюсь"]);
        assert!(
            catalog
                .render(
                    CharacterId::Crazy,
                    Some("../Default"),
                    BTreeMap::new(),
                    "main_template.txt"
                )
                .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn captures_initialization_without_mutating_timeline() {
        let root = root();
        let scripts = root.join("Crazy/Default");
        fs::write(scripts.join("main_template.txt"), "[<Main/init.script>]\n").unwrap();
        fs::write(scripts.join("Main/init.script"), "SEED_MEMORY High | Initial setting ENTITIES Mita, Player\nLINK_ENTITIES Mita -> knows -> Player\nSEED_RAG_MEMORY Normal | Project facts\nRETURN f\"\"\"hello\nworld\"\"\"\n").unwrap();
        let rendered = PromptCatalog::new(&root)
            .render(
                CharacterId::Crazy,
                None,
                BTreeMap::new(),
                "main_template.txt",
            )
            .unwrap();
        assert_eq!(rendered.stable, vec!["hello\nworld"]);
        assert_eq!(rendered.initialization.len(), 3);
        assert_eq!(
            rendered.initialization[0],
            PromptInitializationDeclaration::Memory {
                priority: "High".into(),
                content: "Initial setting".into(),
                entities: vec!["Mita".into(), "Player".into()],
                rag_only: false,
            }
        );
        assert!(
            matches!(&rendered.initialization[1], PromptInitializationDeclaration::Relation { relation, .. } if relation == "knows")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
