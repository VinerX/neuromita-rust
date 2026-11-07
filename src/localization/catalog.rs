use std::{collections::BTreeMap, fmt, fs, path::Path, sync::Arc};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};

use super::{
    Argument, LocalizationError, LocalizedText,
    message::{format_template, placeholders},
};

include!(concat!(env!("OUT_DIR"), "/localization-bundled.rs"));

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocaleId(String);

impl LocaleId {
    pub fn new(code: &str) -> Result<Self, LocalizationError> {
        let code = code.to_ascii_lowercase().replace('_', "-");
        if code.is_empty()
            || code.len() > 35
            || !code.split('-').all(|part| {
                !part.is_empty()
                    && part.len() <= 8
                    && part.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            || !code
                .split('-')
                .next()
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_lowercase())
        {
            return Err(LocalizationError::InvalidLocale(code));
        }
        Ok(Self(code))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn russian() -> Self {
        Self("ru".into())
    }
    pub fn english() -> Self {
        Self("en".into())
    }
}

impl fmt::Display for LocaleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FallbackPolicy {
    Source,
    EnglishThenSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageInfo {
    pub locale: LocaleId,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
struct Catalog {
    name: Option<String>,
    legacy: BTreeMap<String, String>,
    entries: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    source: String,
    translation: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CatalogValue {
    Text(String),
    Entry(Entry),
}

#[derive(Clone, Debug)]
pub struct Localizer {
    sources: BTreeMap<String, String>,
    bundled: BTreeMap<LocaleId, Arc<Catalog>>,
    catalogs: BTreeMap<LocaleId, Arc<Catalog>>,
    fallback: FallbackPolicy,
    revision: u64,
}

impl Localizer {
    pub fn bundled(fallback: FallbackPolicy) -> Result<Self, LocalizationError> {
        let sources = serde_json::from_str(include_str!(concat!(
            env!("OUT_DIR"),
            "/localization-messages.json"
        )))
        .map_err(|e| LocalizationError::Catalog(e.to_string()))?;
        let mut localizer = Self::from_sources(sources, fallback)?;
        for (locale, json) in BUNDLED_LEGACY.iter().chain(BUNDLED_OVERRIDES) {
            localizer.merge_json(LocaleId::new(locale)?, json)?;
        }
        localizer.validate()?;
        localizer.bundled = localizer.catalogs.clone();
        Ok(localizer)
    }

    pub fn from_sources(
        sources: BTreeMap<String, String>,
        fallback: FallbackPolicy,
    ) -> Result<Self, LocalizationError> {
        for (id, source) in &sources {
            if !super::id::valid_id(id) || source.is_empty() {
                return Err(LocalizationError::Catalog(format!(
                    "invalid declaration {id}"
                )));
            }
            placeholders(source)?;
        }
        let catalogs = BTreeMap::from([(LocaleId::russian(), Arc::new(Catalog::default()))]);
        Ok(Self {
            sources,
            bundled: catalogs.clone(),
            catalogs,
            fallback,
            revision: 0,
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn contains_locale(&self, locale: &LocaleId) -> bool {
        self.catalogs.contains_key(locale)
    }

    pub fn languages(&self) -> Vec<LanguageInfo> {
        self.catalogs
            .iter()
            .map(|(locale, catalog)| LanguageInfo {
                locale: locale.clone(),
                name: catalog
                    .name
                    .clone()
                    .unwrap_or_else(|| display_name(locale.as_str()).into()),
            })
            .collect()
    }

    pub fn resolve(
        &self,
        locale: &LocaleId,
        text: &LocalizedText,
    ) -> Result<String, LocalizationError> {
        if !self.contains_locale(locale) {
            return Err(LocalizationError::UnknownLocale(locale.to_string()));
        }
        self.check_source(&text.id, &text.source)?;
        let required = placeholders(&text.source)?;
        if required != text.arguments.keys().cloned().collect() {
            return Err(LocalizationError::Arguments(format!(
                "{} requires {required:?}",
                text.id
            )));
        }
        let template = self.template(locale, &text.id, &text.source);
        if placeholders(template)? != required {
            return Err(LocalizationError::Arguments(format!(
                "translation parameters differ for {} / {locale}",
                text.id
            )));
        }
        format_template(template, |name| match &text.arguments[name] {
            Argument::Literal(value) => Ok(value.clone()),
            Argument::Text(nested) => self.resolve(locale, nested),
        })
    }

    /// Replaces external overlays atomically; removed files stop overriding bundled data.
    pub fn reload_directory(&mut self, directory: &Path) -> Result<(), LocalizationError> {
        let mut candidate = self.clone();
        candidate.catalogs = candidate.bundled.clone();
        let mut paths = fs::read_dir(directory)
            .map_err(|e| LocalizationError::Catalog(format!("{}: {e}", directory.display())))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| LocalizationError::Catalog(e.to_string()))?;
        paths.sort();
        let mut seen = std::collections::BTreeSet::new();
        for path in paths {
            if !path.is_file() || path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| LocalizationError::Catalog("invalid catalog filename".into()))?;
            if name.starts_with('_') {
                continue;
            }
            let locale = LocaleId::new(name)?;
            if !seen.insert(locale.clone()) {
                return Err(LocalizationError::Catalog(format!(
                    "duplicate locale {locale}"
                )));
            }
            let json = fs::read_to_string(&path)
                .map_err(|e| LocalizationError::Catalog(format!("{}: {e}", path.display())))?;
            candidate.merge_json(locale, &json)?;
        }
        candidate.validate()?;
        candidate.revision += 1;
        *self = candidate;
        Ok(())
    }

    /// Adds an in-memory overlay using the same validation as external JSON files.
    pub fn with_catalog(&self, locale: LocaleId, json: &str) -> Result<Self, LocalizationError> {
        let mut candidate = self.clone();
        candidate.merge_json(locale, json)?;
        candidate.validate()?;
        candidate.revision += 1;
        Ok(candidate)
    }

    fn check_source(&self, id: &str, source: &str) -> Result<(), LocalizationError> {
        match self.sources.get(id) {
            None => Err(LocalizationError::UnknownId(id.into())),
            Some(expected) if expected != source => {
                Err(LocalizationError::SourceMismatch(id.into()))
            }
            Some(_) => Ok(()),
        }
    }

    fn merge_json(&mut self, locale: LocaleId, json: &str) -> Result<(), LocalizationError> {
        let StrictMap(values) = serde_json::from_str(json)
            .map_err(|e| LocalizationError::Catalog(format!("{locale}: {e}")))?;
        let catalog = Arc::make_mut(self.catalogs.entry(locale.clone()).or_default());
        for (key, value) in values {
            if key == "_name" {
                let CatalogValue::Text(name) = value else {
                    return Err(LocalizationError::Catalog(format!(
                        "invalid _name in {locale}"
                    )));
                };
                if name.is_empty() {
                    return Err(LocalizationError::Catalog(format!(
                        "empty _name in {locale}"
                    )));
                }
                catalog.name = Some(name);
            } else if key.starts_with('_') {
                return Err(LocalizationError::Catalog(format!(
                    "unknown metadata {key} in {locale}"
                )));
            } else {
                match value {
                    CatalogValue::Text(translation) if !translation.is_empty() => {
                        catalog.legacy.insert(key, translation);
                    }
                    CatalogValue::Text(_) => {}
                    CatalogValue::Entry(entry) => {
                        catalog.entries.insert(key, entry);
                    }
                }
            }
        }
        Ok(())
    }

    fn template<'a>(&'a self, locale: &LocaleId, id: &str, source: &'a str) -> &'a str {
        let lookup = |locale: &LocaleId| {
            self.catalogs.get(locale).and_then(|c| {
                c.entries
                    .get(id)
                    .filter(|entry| !entry.translation.is_empty())
                    .map(|e| e.translation.as_str())
                    .or_else(|| c.legacy.get(source).map(String::as_str))
            })
        };
        lookup(locale)
            .or_else(|| match self.fallback {
                FallbackPolicy::EnglishThenSource
                    if locale.as_str() != "ru" && locale.as_str() != "en" =>
                {
                    lookup(&LocaleId::english())
                }
                _ => None,
            })
            .unwrap_or(source)
    }

    fn validate(&self) -> Result<(), LocalizationError> {
        for (locale, catalog) in &self.catalogs {
            for (id, entry) in &catalog.entries {
                self.check_source(id, &entry.source)?;
            }
            for (id, source) in &self.sources {
                if placeholders(self.template(locale, id, source))? != placeholders(source)? {
                    return Err(LocalizationError::Arguments(format!(
                        "translation parameters differ for {id} / {locale}"
                    )));
                }
            }
        }
        Ok(())
    }
}

struct StrictMap(BTreeMap<String, CatalogValue>);

impl<'de> Deserialize<'de> for StrictMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor;
        impl<'de> Visitor<'de> for MapVisitor {
            type Value = StrictMap;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a catalog with unique keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, CatalogValue>()? {
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {key}")));
                    }
                }
                Ok(StrictMap(values))
            }
        }
        deserializer.deserialize_map(MapVisitor)
    }
}

fn display_name(code: &str) -> &str {
    match code {
        "ru" => "Русский",
        "en" => "English",
        "de" => "Deutsch",
        "el" => "Ελληνικά",
        "es" => "Español",
        "fr" => "Français",
        "it" => "Italiano",
        "ja" => "日本語",
        "ko" => "한국어",
        "nl" => "Nederlands",
        "pl" => "Polski",
        "pt" => "Português",
        "ro" => "Română",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "zh" => "简体中文",
        other => other,
    }
}
