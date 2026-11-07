use super::*;

mod probe {
    slint::slint! {
        import { I18n } from "../../../ui/i18n.slint";
        import { CollapsibleSettingsSection } from "../../../ui/components/collapsible-section.slint";
        import { SettingsTheme } from "../../../ui/settings/theme.slint";
        import { SettingsScrollView } from "../../../ui/settings/scroll-view.slint";
        import { SettingsToggleRow, SettingsNumberRow, SettingsButtonRow } from "../../../ui/components/settings-kit.slint";
        import { CharacterVoiceTabs } from "../../../ui/components/character-voice-tabs.slint";
        import { CharacterCatalog, CharacterOption } from "../../../ui/characters/catalog.slint";
        export { CharacterCatalog, CharacterOption }
        export { I18n }
        export component VoiceTabsProbe inherits Window {
            preferred-width: 600px; preferred-height: 234px;
            background: SettingsTheme.canvas;
            CharacterVoiceTabs { selected: 0; }
        }
        export component ControlsProbe inherits Window {
            preferred-width: 400px; preferred-height: 140px;
            out property <length> row-top: row.y;
            VerticalLayout { padding: 0px; spacing: 0px;
                CollapsibleSettingsSection { title: "Title"; subtitle: "Subtitle"; glyph: "\u{f1de}"; }
                row := SettingsToggleRow { title: "Toggle"; height: 60px; checked: true; }
                Rectangle { vertical-stretch: 1; }
            }
        }
        export component TranslationProbe inherits Window {
            out property <string> welcome: I18n.i18n("launcher.home.welcome", "Добро пожаловать!");
            out property <string> more: I18n.i18n("launcher.actions.more", "Дополнительно");
        }
        export component RowSizingProbe inherits Window {
            preferred-width: 500px; preferred-height: 200px;
            out property <length> first-height: first.height;
            out property <length> second-top: second.y;
            out property <length> second-bottom: second.y + second.height;
            out property <length> button-top: button.y;
            VerticalLayout { padding: 0px; spacing: 10px; alignment: start;
                first := SettingsNumberRow { title: "Minimum rounds"; value: 1; }
                second := SettingsNumberRow { title: "Maximum turns"; value: 24; }
                button := SettingsButtonRow { title: "Launch game"; button-text: "Launch"; }
            }
        }
        export component AccordionProbe inherits Window {
            background: SettingsTheme.panel;
            preferred-width: 500px;
            preferred-height: 600px;
            in-out property <bool> expanded;
            out property <length> section-height: section.height;
            out property <length> next-top: next.y;
            VerticalLayout {
                padding: 0px; spacing: 12px;
                section := CollapsibleSettingsSection {
                    title: "Geometry probe";
                    expanded: root.expanded;
                    Rectangle { height: 300px; }
                }
                next := Rectangle { height: 50px; }
                Rectangle { vertical-stretch: 1; }
            }
        }
        export component OverflowProbe inherits Window {
            preferred-width: 500px; preferred-height: 260px;
            in-out property <int> expanded-count: 3;
            in-out property <length> payload-height: 300px;
            in-out property <length> offset <=> scroll.content-y;
            out property <length> first-height: first.height;
            out property <length> second-height: second.height;
            out property <length> third-height: third.height;
            out property <length> last-bottom: last.y + last.height;
            out property <length> content-height: sections.preferred-height;
            out property <bool> scrollable: scroll.scrollable;
            out property <length> viewport-width: scroll.visible-width;
            out property <length> viewport-left: sections.x;
            scroll := SettingsScrollView {
                width: parent.width; height: parent.height;
                content-height: sections.preferred-height;
                sections := VerticalLayout {
                    width: scroll.visible-width; padding: 0px; spacing: 12px; alignment: start;
                    first := CollapsibleSettingsSection { expanded: root.expanded-count > 0; Rectangle { height: root.payload-height; } }
                    second := CollapsibleSettingsSection { expanded: root.expanded-count > 1; Rectangle { height: 200px; } }
                    third := CollapsibleSettingsSection { expanded: root.expanded-count > 2; Rectangle { height: 300px; } }
                    last := Rectangle { height: 40px; }
                }
            }
        }
    }
}

struct TestPlatform;
impl slint::platform::Platform for TestPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
    }
}

#[test]
fn existing_slint_bindings_retranslate_on_locale_change_and_catalog_revision() {
    slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
    let controls = probe::ControlsProbe::new().unwrap();
    controls.show().unwrap();
    controls
        .window()
        .set_size(slint::PhysicalSize::new(400, 140));
    let image = controls.window().take_snapshot().unwrap();
    let green_rows = image
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, pixel)| {
            pixel[1] > pixel[0].saturating_add(40) && pixel[1] > pixel[2].saturating_add(30)
        })
        .map(|(index, _)| (index / 400) as f32)
        .collect::<Vec<_>>();
    assert!(!green_rows.is_empty());
    assert!(green_rows[0] > controls.get_row_top() + 17.0);
    assert!(*green_rows.last().unwrap() < controls.get_row_top() + 43.0);
    controls.hide().unwrap();
    let voice = probe::VoiceTabsProbe::new().unwrap();
    voice
        .global::<probe::CharacterCatalog>()
        .set_entries(ModelRc::new(VecModel::from(
            neuromita::character::character_catalog()
                .enumerate()
                .map(|(index, item)| probe::CharacterOption {
                    index: index as i32,
                    id: item.id.as_str().into(),
                    name: item.name.source().into(),
                    name_key: item.name.id().into(),
                    conversation_name: item.conversation_name.source().into(),
                    conversation_key: item.conversation_name.id().into(),
                    initial: item.id.as_str().chars().next().unwrap().to_string().into(),
                })
                .collect::<Vec<_>>(),
        )));
    voice.show().unwrap();
    voice.window().set_size(slint::PhysicalSize::new(600, 234));
    let surface = voice.window().take_snapshot().unwrap();
    let pixel = |x: usize, y: usize| {
        let offset = (y * 600 + x) * 4;
        &surface.as_bytes()[offset..offset + 3]
    };
    assert_eq!(
        pixel(248, 107),
        &[10, 10, 24],
        "inactive tabs must leave a gap before the editor surface"
    );
    let arrow_rows = (27..109)
        .filter(|y| (570..596).any(|x| pixel(x, *y).iter().all(|channel| *channel > 200)))
        .collect::<Vec<_>>();
    assert!(
        !arrow_rows.is_empty(),
        "enabled right scroll arrow must be visible"
    );
    let arrow_center = (arrow_rows[0] + arrow_rows[arrow_rows.len() - 1]) / 2;
    assert!(
        (60..80).contains(&arrow_center),
        "scroll arrows must be vertically centered in the tab strip: {arrow_center}"
    );
    voice.hide().unwrap();
    let rows = probe::RowSizingProbe::new().unwrap();
    rows.show().unwrap();
    rows.window().set_size(slint::PhysicalSize::new(500, 200));
    rows.window().take_snapshot().unwrap();
    assert!(rows.get_first_height() >= 40.0);
    assert!(rows.get_second_top() >= rows.get_first_height() + 9.0);
    assert!(rows.get_button_top() >= rows.get_second_bottom() + 9.0);
    rows.hide().unwrap();
    let accordion = probe::AccordionProbe::new().unwrap();
    accordion.show().unwrap();
    accordion
        .window()
        .set_size(slint::PhysicalSize::new(500, 600));
    accordion.window().take_snapshot().unwrap();
    let closed = (accordion.get_section_height(), accordion.get_next_top());
    assert!((closed.0 - 50.0).abs() < 1.0);
    accordion.set_expanded(true);
    let expanded = accordion.window().take_snapshot().unwrap();
    assert!(accordion.get_next_top() > closed.1 + 300.0);
    let pixel = |x: usize, y: usize| {
        let offset = (y * expanded.width() as usize + x) * 4;
        &expanded.as_bytes()[offset..offset + 3]
    };
    assert_ne!(
        pixel(250, 20),
        pixel(250, 100),
        "expanded body must retain its QSS background layer"
    );
    assert_ne!(
        pixel(0, 100),
        pixel(250, 100),
        "content must not paint over the section border"
    );
    accordion.set_expanded(false);
    accordion.window().take_snapshot().unwrap();
    assert_eq!(
        (accordion.get_section_height(), accordion.get_next_top()),
        closed
    );
    accordion.hide().unwrap();
    let overflow = probe::OverflowProbe::new().unwrap();
    overflow.show().unwrap();
    overflow
        .window()
        .set_size(slint::PhysicalSize::new(500, 260));
    overflow.window().take_snapshot().unwrap();
    assert!(overflow.get_scrollable());
    assert_eq!(overflow.get_viewport_left(), 0.0);
    let viewport_width = overflow.get_viewport_width();
    assert!(viewport_width < 500.0 && viewport_width > 480.0);
    assert!(overflow.get_first_height() > 350.0);
    assert!(overflow.get_second_height() > 250.0);
    assert!(overflow.get_third_height() > 350.0);
    let first_height = overflow.get_first_height();
    overflow.set_expanded_count(1);
    overflow.window().take_snapshot().unwrap();
    assert_eq!(overflow.get_first_height(), first_height);
    overflow.set_payload_height(500.0);
    overflow.set_expanded_count(3);
    overflow.window().take_snapshot().unwrap();
    assert!(overflow.get_first_height() > first_height + 199.0);
    overflow.set_offset(260.0 - overflow.get_content_height());
    overflow.window().take_snapshot().unwrap();
    assert!((overflow.get_last_bottom() + overflow.get_offset() - 260.0).abs() < 1.0);
    overflow.set_expanded_count(0);
    overflow.window().take_snapshot().unwrap();
    assert!(!overflow.get_scrollable());
    assert_eq!(overflow.get_viewport_width(), viewport_width);
    assert_eq!(overflow.get_offset(), 0.0);
    overflow.hide().unwrap();
    let catalog = Rc::new(RefCell::new(
        Localizer::bundled(FallbackPolicy::EnglishThenSource).unwrap(),
    ));
    let window = probe::TranslationProbe::new().unwrap();
    let bridge = window.global::<probe::I18n>();
    bridge.on_lookup(translation_lookup(catalog.clone()));
    bridge.set_locale("ru".into());
    assert_eq!(window.get_welcome(), "Добро пожаловать!");
    bridge.set_locale("en".into());
    assert_eq!(window.get_welcome(), "Welcome!");
    assert_eq!(window.get_more(), "More");
    bridge.set_locale("de".into());
    assert_ne!(window.get_welcome(), "Welcome!");
    bridge.set_locale("en".into());
    let candidate = catalog
        .borrow()
        .with_catalog(
            LocaleId::english(),
            r#"{
        "launcher.home.welcome":{"source":"Добро пожаловать!","translation":"Welcome again!"}
    }"#,
        )
        .unwrap();
    *catalog.borrow_mut() = candidate;
    bridge.set_revision(1);
    assert_eq!(window.get_welcome(), "Welcome again!");
    bridge.set_locale("ru".into());
    assert_eq!(window.get_welcome(), "Добро пожаловать!");
    sandbox_render_and_bindings(catalog);
}

fn sandbox_render_and_bindings(catalog: Rc<RefCell<Localizer>>) {
    use super::super::view::{ChatRow, ChatStatus, ReleaseEntry};
    let window = AppWindow::new().unwrap();
    window
        .global::<I18n>()
        .on_lookup(translation_lookup(catalog));
    window.global::<I18n>().set_locale("ru".into());
    super::super::sandbox::render_catalog(
        &window,
        neuromita::character::character_catalog().collect(),
    );
    let catalog = window
        .global::<super::super::view::CharacterCatalog>()
        .get_entries();
    assert_eq!(
        catalog.row_count(),
        neuromita::character::CharacterId::ALL.len()
    );
    for id in neuromita::character::CharacterId::ALL {
        let row = catalog.row_data(id.index()).unwrap();
        assert_eq!(row.id, id.as_str());
        assert_eq!(row.index, id.index() as i32);
    }
    assert_eq!(catalog.row_data(0).unwrap().name, "Безумная Мита");
    window.set_page(PageId::Sandbox);
    window.set_version("v0.1.0".into());
    window.set_core_connected(true);
    window.set_chat_generation_ready(true);
    window.set_chat_model("Gemini · test fixture".into());
    window.set_chat_preset("#0".into());
    window.set_chat_attitude("60.0".into());
    window.set_chat_boredom("10.0".into());
    window.set_chat_stress("5.0".into());
    window.set_chat_active(1);
    window.set_chat_messages(ModelRc::new(VecModel::from(vec![
        ChatRow { text: "Ты тут?".into(), user: true, sequence: "#1".into(), ..Default::default() },
        ChatRow { text: "Конечно я тут! Куда же я от тебя денусь? Рада снова тебя видеть. Чем был занят?".into(), status: ChatStatus::Success, sequence: "#2".into(), ..Default::default() },
        ChatRow { text: "Как твои дела?".into(), user: true, sequence: "#3".into(), ..Default::default() },
        ChatRow { text: "Со мной всё в порядке. Сейчас закончу озвучивать ответ, а пока можешь его прочитать.".into(), status: ChatStatus::Voice, can_cancel: true, request_id: uuid::Uuid::new_v4().to_string().into(), ..Default::default() },
    ])));
    let sent = Rc::new(RefCell::new(String::new()));
    let sent_callback = sent.clone();
    window.on_chat_send(move |text| *sent_callback.borrow_mut() = text.to_string());
    window.invoke_chat_send("typed command".into());
    assert_eq!(&*sent.borrow(), "typed command");
    window.set_chat_draft("draft survives navigation".into());
    window.set_page(PageId::Home);
    window.set_page(PageId::Sandbox);
    assert_eq!(window.get_chat_draft(), "draft survives navigation");
    window.set_chat_draft("".into());
    window.show().unwrap();
    for (width, height, name) in [(1560, 918, "sandbox-ru"), (1200, 760, "sandbox-compact")] {
        window
            .window()
            .set_size(slint::PhysicalSize::new(width, height));
        let image = window.window().take_snapshot().unwrap();
        assert_eq!((image.width(), image.height()), (width, height));
        if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
            for rgba in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(directory.join(format!("{name}.ppm")), ppm).unwrap();
        }
    }
    window
        .window()
        .set_size(slint::PhysicalSize::new(1560, 918));
    window.window().take_snapshot().unwrap();
    for (x, name) in [
        (1260.0, "sandbox-state"),
        (1365.0, "sandbox-debug"),
        (1500.0, "sandbox-collapsed"),
    ] {
        let position = slint::LogicalPosition { x, y: 111.0 };
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        let image = window.window().take_snapshot().unwrap();
        if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for rgba in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(PathBuf::from(directory).join(format!("{name}.ppm")), ppm).unwrap();
        }
    }
    window.set_page(PageId::Home);
    window.window().take_snapshot().unwrap();
    window.set_release_releases(ModelRc::new(VecModel::from(vec![
        ReleaseEntry {
            id: "2026.11.01".into(),
            title: "NeuroMita v2026.11.01".into(),
            date: "2026-11-01".into(),
            summary: "Тестовый интеграционный релиз / Integration test release.".into(),
            details: "Релиз включает обновления интерфейса и основные исправления.".into(),
            url: "https://github.com/Atm4x/NeuroMita/releases/tag/v2026.11.01".into(),
            prerelease: false,
        },
        ReleaseEntry {
            id: "2026.10.22".into(),
            title: "NeuroMita v2026.10.22".into(),
            date: "2026-10-22".into(),
            summary: "Тестовая сборка / Test build.".into(),
            details: String::new().into(),
            url: "https://github.com/Atm4x/NeuroMita/releases/tag/v2026.10.22".into(),
            prerelease: false,
        },
    ])));
    for (page, filename) in [
        (PageId::Releases, "releases-ru.ppm"),
        (PageId::About, "about-ru.ppm"),
    ] {
        window.set_page(page);
        let image = window.window().take_snapshot().unwrap();
        assert_eq!((image.width(), image.height()), (1560, 918));
        if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for rgba in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&rgba[..3]);
            }
            std::fs::write(directory.join(filename), ppm).unwrap();
        }
    }
    window.set_page(PageId::Sandbox);
    window.set_core_connected(false);
    window.set_chat_error("Core is not connected".into());
    let image = window.window().take_snapshot().unwrap();
    if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
        let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
        for rgba in image.as_bytes().as_chunks::<4>().0 {
            ppm.extend_from_slice(&rgba[..3]);
        }
        std::fs::write(
            PathBuf::from(directory).join("sandbox-disconnected-error.ppm"),
            ppm,
        )
        .unwrap();
    }
    window.global::<I18n>().set_locale("en".into());
    window.window().take_snapshot().unwrap();
    settings_render_and_bindings(&window);
    memory_render_and_contract(&window);
    window.hide().unwrap();
}

fn settings_render_and_bindings(window: &AppWindow) {
    use super::super::view::{PresetItem, SettingsUi};
    let settings = window.global::<SettingsUi>();
    settings.set_preset_templates(ModelRc::new(VecModel::from(
        super::super::settings::provider_templates(
            &neuromita::presentation::PresetsPresenter::providers(),
        ),
    )));
    settings.set_settings_presets(ModelRc::new(VecModel::from(vec![PresetItem {
        key_error: "".into(),
        output_token_wire_key: "maxOutputTokens".into(),
        template_id: "gemini".into(),
        temperature_enabled: true,
        tokens_enabled: true,
        provider: "google".into(),
        configured: true,
        id: "7".into(),
        name: "Main".into(),
        model: "fixture-model".into(),
        endpoint: "http://127.0.0.1:12345/v1beta/".into(),
        key_env: "env:FIXTURE_KEY".into(),
        temperature: "0.7".into(),
        tokens: "3000".into(),
        timeout: "60".into(),
    }])));
    settings.set_settings_active("7".into());
    settings.set_settings_revision("12".into());
    settings.set_settings_persistence(1);
    settings.set_settings_voice_launcher(true);
    settings.set_settings_volume(65);
    settings.set_character_presets(ModelRc::new(VecModel::from(
        neuromita::character::CharacterId::ALL
            .map(|_| super::super::view::CharacterPresets {
                reply: "active".into(),
                react: "7".into(),
                initiate: "active".into(),
                split: true,
            })
            .to_vec(),
    )));
    let commands = Rc::new(RefCell::new(Vec::new()));
    let sent = commands.clone();
    super::super::settings::bind(
        window,
        Rc::new(move |command| sent.borrow_mut().push(command)),
    );
    settings.invoke_preset_edit("7".into());
    assert_eq!(settings.get_preset_model(), "fixture-model");
    assert_eq!(
        settings.get_preset_request_url(),
        "http://127.0.0.1:12345/v1beta/models/fixture-model:generateContent"
    );
    settings.set_preset_model("changed-model".into());
    assert_eq!(
        settings.get_preset_request_url(),
        "http://127.0.0.1:12345/v1beta/models/changed-model:generateContent"
    );
    assert_eq!(
        settings.get_preset_endpoint(),
        "http://127.0.0.1:12345/v1beta/"
    );
    settings.set_preset_model("fixture-model".into());
    settings.invoke_preset_save();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::UpdatePreset(
            neuromita::generation::PresetId(7),
            _
        ))
    ));
    settings.set_preset_temperature("invalid".into());
    let count = commands.borrow().len();
    settings.invoke_preset_save();
    assert_eq!(commands.borrow().len(), count);
    assert!(!settings.get_settings_error().is_empty());
    settings.set_settings_error("".into());
    settings.set_preset_temperature("0.7".into());
    settings.invoke_preset_edit("7".into());
    super::super::settings::saved(window, None);
    assert_eq!(settings.get_preset_edit_id(), "7");
    settings.invoke_preset_new();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::NewPreset)
    ));
    let before = commands.borrow().len();
    settings.set_preset_model("modified-before-switch".into());
    assert!(settings.get_preset_dirty());
    settings.invoke_preset_new();
    assert_eq!(commands.borrow().len(), before);
    assert_eq!(settings.get_preset_pending_id(), "__new__");
    settings.invoke_preset_confirm_cancel();
    assert!(settings.get_preset_pending_id().is_empty());
    settings.invoke_preset_new();
    settings.invoke_preset_confirm_discard();
    assert!(matches!(
        commands.borrow().last(),
        Some(super::super::runtime::Command::NewPreset)
    ));

    window.set_page(PageId::Settings);
    window.set_core_connected(true);
    window.global::<I18n>().set_locale("ru".into());
    for (width, height) in [(1560, 918), (1200, 760)] {
        window
            .window()
            .set_size(slint::PhysicalSize::new(width, height));
        for section in [0, 4, 7, 8, 9] {
            settings.set_section(section);
            let image = window.window().take_snapshot().unwrap();
            if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                for rgba in image.as_bytes().as_chunks::<4>().0 {
                    ppm.extend_from_slice(&rgba[..3]);
                }
                std::fs::write(
                    PathBuf::from(directory).join(format!("settings-{section}-{width}.ppm")),
                    ppm,
                )
                .unwrap();
            }
        }
    }
    window.global::<I18n>().set_locale("en".into());
    window.window().take_snapshot().unwrap();
}

fn memory_render_and_contract(window: &AppWindow) {
    use super::super::{memory, runtime::Command, view::MemoryUi};
    use neuromita::{
        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        character::CharacterId,
        generation::{
            GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
        },
        memory::{MemoryCommand, MemoryPriority},
    };
    use std::{
        num::NonZeroUsize,
        sync::{Arc, Mutex},
        time::Duration,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let generator = ResponseGenerator::new([
            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
        ])
        .unwrap();
        let template = GeminiTemplate::google(
            "fixture",
            "fixture-key",
            GenerationSettings::new(0.7, 100, Duration::from_secs(1)).unwrap(),
        )
        .unwrap();
        let limit = NonZeroUsize::new(16).unwrap();
        let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
            GenerationPreset::new(ProviderTemplate::Gemini(template)),
            generator,
            RuntimeLimits {
                request_queue: limit,
                discovery_buffer: limit,
                character_commands: limit,
                presenter_updates: limit,
                retained_finished: limit,
            },
            "127.0.0.1:0".parse().unwrap(),
        ))
        .await
        .unwrap();
        super::super::settings::execute(
            app.presenter(),
            super::super::runtime::Command::SelectAsrRecognizer("whisper".into()),
        )
        .unwrap();
        let mut absent_dropdown = app.presenter().settings.asr.preferences.get();
        absent_dropdown.recognizer_id.clear();
        super::super::settings::execute(
            app.presenter(),
            super::super::runtime::Command::SaveAsr(absent_dropdown, None),
        )
        .unwrap();
        assert_eq!(
            app.presenter().settings.asr.preferences.get().recognizer_id,
            "whisper"
        );
        let initial_revision = app.presenter().settings.snapshot().unwrap().revision;
        super::super::settings::execute(
            app.presenter(),
            Command::AssignAllPresets(
                CharacterId::Crazy,
                neuromita::settings::PresetSelection::Specific(neuromita::generation::PresetId(0)),
            ),
        )
        .unwrap();
        let after = app.presenter().settings.snapshot().unwrap();
        assert_eq!(after.revision, initial_revision + 1);
        let assignments = after.values.characters.crazy.presets;
        assert_eq!(assignments.reply, assignments.react);
        assert_eq!(assignments.reply, assignments.initiate);
        assert!(
            super::super::settings::execute(
                app.presenter(),
                Command::AssignAllPresets(
                    CharacterId::Crazy,
                    neuromita::settings::PresetSelection::Specific(
                        neuromita::generation::PresetId(999)
                    )
                )
            )
            .is_err()
        );
        assert_eq!(
            app.presenter().settings.snapshot().unwrap().revision,
            after.revision
        );
        let characters = &app.presenter().characters;
        let initial = characters.memories(CharacterId::Crazy).await.unwrap();
        characters
            .edit_memories(
                CharacterId::Crazy,
                initial,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Игрок любит зелёный чай.".into(),
                }],
            )
            .await
            .unwrap();
        let state = Arc::new(Mutex::new(memory::MemoryEditor::default()));
        let commands = Rc::new(RefCell::new(Vec::new()));
        let send = commands.clone();
        memory::bind(
            window,
            Rc::new(move |command| send.borrow_mut().push(command)),
            &state,
        );
        let page = characters
            .list_memories(CharacterId::Crazy, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        let ui = window.global::<MemoryUi>();
        ui.set_open(true);
        window
            .global::<super::super::view::SettingsUi>()
            .set_section(2);
        window
            .global::<super::super::view::SettingsUi>()
            .set_character_prompt_expanded(false);
        ui.invoke_edit(page.records()[0].id.0.to_string().into());
        ui.set_content("Изменённый текст".into());
        let snapshot = characters.memories(CharacterId::Crazy).await.unwrap();
        characters
            .edit_memories(
                CharacterId::Crazy,
                snapshot,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Новое воспоминание от внешнего клиента.".into(),
                }],
            )
            .await
            .unwrap();
        let page = characters
            .list_memories(CharacterId::Crazy, Default::default())
            .await
            .unwrap();
        memory::render(window, &state, &page);
        assert_eq!(ui.get_content(), "Изменённый текст");
        ui.invoke_apply(0);
        let command = commands.borrow_mut().pop().unwrap();
        let Command::MemoryEdit(edit_id, expected, edits) = command else {
            panic!("expected typed memory edit")
        };
        assert!(
            characters
                .edit_memories(expected.character(), expected, edits)
                .await
                .is_err()
        );
        ui.invoke_edit(page.records()[1].id.0.to_string().into());
        ui.set_content("new draft after save".into());
        memory::saved(window, &state, edit_id);
        assert_eq!(ui.get_content(), "new draft after save");
        assert!(ui.get_editing());
        memory::failed(window, &state, edit_id, "old error".into());
        assert!(ui.get_error().is_empty());
        for (width, height) in [(1560, 918), (1200, 760)] {
            window
                .window()
                .set_size(slint::PhysicalSize::new(width, height));
            let image = window.window().take_snapshot().unwrap();
            if let Some(directory) = std::env::var_os("NEUROMITA_GUI_CAPTURE_DIR") {
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                for rgba in image.as_bytes().as_chunks::<4>().0 {
                    ppm.extend_from_slice(&rgba[..3]);
                }
                std::fs::write(
                    PathBuf::from(directory).join(format!("memory-{width}.ppm")),
                    ppm,
                )
                .unwrap();
            }
        }
        ui.set_open(false);
        app.shutdown().await.unwrap();
    });
}
