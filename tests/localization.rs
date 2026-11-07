use std::{collections::BTreeMap, fs, path::PathBuf};

use neuromita::localization::{
    FallbackPolicy, LocaleId, LocalizationError, Localizer, i18n, source_template_json,
};

#[test]
fn generated_template_is_a_valid_untranslated_catalog() {
    let localizer = Localizer::bundled(FallbackPolicy::EnglishThenSource).unwrap();
    let template = source_template_json().unwrap();
    let other = localizer
        .with_catalog(LocaleId::new("xx").unwrap(), &template)
        .unwrap();
    let welcome = i18n("launcher.home.welcome", "Добро пожаловать!");
    assert_eq!(
        other
            .resolve(&LocaleId::new("xx").unwrap(), &welcome)
            .unwrap(),
        "Welcome!"
    );
}

#[path = "../localization/extract.rs"]
mod extract;

fn catalog(sources: &[(&str, &str)]) -> Localizer {
    Localizer::from_sources(
        sources
            .iter()
            .map(|(id, text)| ((*id).into(), (*text).into()))
            .collect(),
        FallbackPolicy::EnglishThenSource,
    )
    .unwrap()
}

#[test]
fn imported_catalogs_support_all_bundled_languages_and_context_override() {
    let localizer = Localizer::bundled(FallbackPolicy::EnglishThenSource).unwrap();
    assert_eq!(localizer.languages().len(), 16);
    let more = i18n("launcher.actions.more", "Дополнительно");
    assert_eq!(
        localizer.resolve(&LocaleId::english(), &more).unwrap(),
        "More"
    );
    assert_eq!(
        localizer.resolve(&LocaleId::russian(), &more).unwrap(),
        "Дополнительно"
    );
    let welcome = i18n("launcher.home.welcome", "Добро пожаловать!");
    for language in localizer.languages() {
        assert!(
            !localizer
                .resolve(&language.locale, &welcome)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        localizer.resolve(&LocaleId::english(), &welcome).unwrap(),
        "Welcome!"
    );
}

#[test]
fn every_current_declaration_has_a_non_source_english_translation() {
    let localizer = Localizer::bundled(FallbackPolicy::EnglishThenSource).unwrap();
    let sources = extract::extract_sources(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert!(sources.len() >= 40);
    for (id, source) in sources {
        if source.contains('{')
            || !source.chars().any(char::is_alphabetic)
            || matches!(source.as_str(), "Unity" | "NEW")
        {
            continue;
        }
        let translated = localizer
            .resolve(&LocaleId::english(), &i18n(&id, &source))
            .unwrap();
        assert_ne!(translated, source, "missing English translation for {id}");
    }
}

#[test]
fn descriptors_and_nested_arguments_are_resolved_again_after_language_changes() {
    let localizer = catalog(&[("notice", "Действие: {action}"), ("action", "Проверка")])
        .with_catalog(
            LocaleId::english(),
            r#"{"Действие: {action}":"Action: {action}","Проверка":"Verify"}"#,
        )
        .unwrap();
    let notice = i18n("notice", "Действие: {action}").arg("action", i18n("action", "Проверка"));
    assert_eq!(
        localizer.resolve(&LocaleId::russian(), &notice).unwrap(),
        "Действие: Проверка"
    );
    assert_eq!(
        localizer.resolve(&LocaleId::english(), &notice).unwrap(),
        "Action: Verify"
    );
    assert_eq!(
        localizer.resolve(&LocaleId::russian(), &notice).unwrap(),
        "Действие: Проверка"
    );
}

#[test]
fn missing_translation_uses_the_explicit_policy_without_hiding_unknown_ids() {
    let localizer = catalog(&[("welcome", "Привет"), ("source", "Исходник")])
        .with_catalog(LocaleId::english(), r#"{"Привет":"Hello"}"#)
        .unwrap()
        .with_catalog(LocaleId::new("fr").unwrap(), r#"{"_name":"Français"}"#)
        .unwrap();
    let french = LocaleId::new("fr").unwrap();
    assert_eq!(
        localizer
            .resolve(&french, &i18n("welcome", "Привет"))
            .unwrap(),
        "Hello"
    );
    assert_eq!(
        localizer
            .resolve(&french, &i18n("source", "Исходник"))
            .unwrap(),
        "Исходник"
    );
    assert!(matches!(
        localizer.resolve(&french, &i18n("typo", "Привет")),
        Err(LocalizationError::UnknownId(_))
    ));
    assert!(matches!(
        localizer.resolve(&french, &i18n("welcome", "Иной текст")),
        Err(LocalizationError::SourceMismatch(_))
    ));
    assert!(matches!(
        localizer.resolve(&LocaleId::new("xx").unwrap(), &i18n("welcome", "Привет")),
        Err(LocalizationError::UnknownLocale(_))
    ));
}

#[test]
fn stable_ids_disambiguate_identical_source_text() {
    let localizer = catalog(&[
        ("menu.more", "Дополнительно"),
        ("settings.more", "Дополнительно"),
    ])
    .with_catalog(
        LocaleId::english(),
        r#"{
            "Дополнительно":"Custom params",
            "menu.more":{"source":"Дополнительно","translation":"More"}
        }"#,
    )
    .unwrap();
    assert_eq!(
        localizer
            .resolve(&LocaleId::english(), &i18n("menu.more", "Дополнительно"))
            .unwrap(),
        "More"
    );
    assert_eq!(
        localizer
            .resolve(
                &LocaleId::english(),
                &i18n("settings.more", "Дополнительно")
            )
            .unwrap(),
        "Custom params"
    );
    assert!(
        localizer
            .with_catalog(
                LocaleId::english(),
                r#"{"menu.more":{"source":"Старый текст","translation":"More"}}"#
            )
            .is_err()
    );
}

#[test]
fn catalog_and_parameter_errors_are_rejected() {
    let localizer = catalog(&[("counter", "Число: {n}")]);
    for bad in [
        r#"{"Число: {n}":"One","Число: {n}":"Two"}"#,
        r#"{"Число: {n}":"Count: {missing}"}"#,
        r#"{"counter":{"source":"Число: {n}","translation":"Count: {n}","extra":true}}"#,
        r#"{"unknown":{"source":"Missing","translation":"Text"}}"#,
        r#"{"counter":{"source":"Число: {n}","source":"Число: {n}","translation":"Count: {n}"}}"#,
    ] {
        assert!(
            localizer.with_catalog(LocaleId::english(), bad).is_err(),
            "{bad}"
        );
    }
    assert!(
        localizer
            .resolve(&LocaleId::russian(), &i18n("counter", "Число: {n}"))
            .is_err()
    );
    assert!(
        localizer
            .resolve(
                &LocaleId::russian(),
                &i18n("counter", "Число: {n}")
                    .arg("n", "1")
                    .arg("extra", "2")
            )
            .is_err()
    );
    let text = i18n("counter", "Число: {n}").arg("n", "{literal}");
    assert_eq!(
        localizer.resolve(&LocaleId::russian(), &text).unwrap(),
        "Число: {literal}"
    );
}

#[test]
fn external_reload_is_atomic_and_removes_deleted_overlays() {
    let directory = TempDirectory::new();
    let file = directory.0.join("en.json");
    let mut localizer = catalog(&[("welcome", "Привет")]);
    fs::write(
        &file,
        r#"{"welcome":{"source":"Привет","translation":"Hello"}}"#,
    )
    .unwrap();
    localizer.reload_directory(&directory.0).unwrap();
    let welcome = i18n("welcome", "Привет");
    assert_eq!(
        localizer.resolve(&LocaleId::english(), &welcome).unwrap(),
        "Hello"
    );
    let revision = localizer.revision();
    fs::write(
        &file,
        r#"{"welcome":{"source":"Привет","translation":"Hi"}}"#,
    )
    .unwrap();
    fs::write(directory.0.join("fr.json"), "broken json").unwrap();
    assert!(localizer.reload_directory(&directory.0).is_err());
    assert_eq!(localizer.revision(), revision);
    assert_eq!(
        localizer.resolve(&LocaleId::english(), &welcome).unwrap(),
        "Hello"
    );
    fs::remove_file(directory.0.join("fr.json")).unwrap();
    localizer.reload_directory(&directory.0).unwrap();
    assert_eq!(
        localizer.resolve(&LocaleId::english(), &welcome).unwrap(),
        "Hi"
    );
    fs::remove_file(&file).unwrap();
    localizer.reload_directory(&directory.0).unwrap();
    assert!(!localizer.contains_locale(&LocaleId::english()));
}

#[test]
fn extraction_skips_comments_and_preserves_unicode_raw_strings_and_conflict_errors() {
    let mut messages = BTreeMap::new();
    extract::extract_text(
        r###"
        // i18n("ignored", "Ignored")
        /* outer /* i18n("ignored", "Ignored") */ */
        let a = i18n("menu.more", "Дополнительно");
        text: I18n.i18n("menu.more", "Дополнительно");
        let b = i18n("message.raw", r#"Текст "в кавычках""#);
        let c = i18n("message.unicode", "\u{0410}");
        let d = i18n("message.trailing", "Запятая",);
        let e = i18n("message.escape", "\\u{{0410}}");
    "###,
        &mut messages,
    )
    .unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages["message.escape"], "\\u{{0410}}");
    assert_eq!(messages["message.unicode"], "А");
    assert_eq!(messages["message.raw"], "Текст \"в кавычках\"");
    assert!(extract::extract_text(r#"i18n("menu.more", "Другое")"#, &mut messages).is_err());
    assert!(extract::extract_text(r#"i18n("invalid..id", "Текст")"#, &mut messages).is_err());
}

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("neuromita-localization-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        for name in ["en.json", "fr.json"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
