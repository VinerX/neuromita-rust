use std::{cell::RefCell, error::Error, path::PathBuf, rc::Rc, time::Duration};

use neuromita::localization::{FallbackPolicy, LocaleId, Localizer, i18n};
use neuromita::presentation::PresetsPresenter;
use slint::ComponentHandle;

mod view {
    slint::include_modules!();
}

fn main() -> Result<(), Box<dyn Error>> {
    let destination = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: gui_snapshot <new-output.ppm>")?,
    );
    if destination.exists() {
        return Err("snapshot destination already exists".into());
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg".into())
        .select()?;
    let window = view::AppWindow::new()?;
    let catalog = Localizer::bundled(FallbackPolicy::EnglishThenSource)?;
    let translations = window.global::<view::I18n>();
    translations.on_lookup(move |code, _, id, source| {
        catalog
            .resolve(
                &LocaleId::new(&code).expect("fixture locale"),
                &i18n(&id, &source),
            )
            .unwrap_or_else(|_| source.to_string())
            .into()
    });
    translations.set_locale("ru".into());
    window.set_page(view::PageId::Settings);
    window.set_core_connected(true);
    window.set_version(format!("v{}", env!("CARGO_PKG_VERSION")).into());
    let general = std::env::args().nth(2).as_deref() == Some("general");
    window
        .global::<view::SettingsUi>()
        .set_section(if general { 0 } else { 2 });
    let characters = neuromita::character::character_catalog()
        .enumerate()
        .map(|(index, item)| view::CharacterOption {
            index: index as i32,
            id: item.id.as_str().into(),
            name: item.name.source().into(),
            name_key: item.name.id().into(),
            conversation_name: item.conversation_name.source().into(),
            conversation_key: item.conversation_name.id().into(),
            initial: item.id.as_str().chars().next().unwrap().to_string().into(),
        })
        .collect::<Vec<_>>();
    window
        .global::<view::CharacterCatalog>()
        .set_entries(slint::ModelRc::new(slint::VecModel::from(characters)));
    let mode = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "characters".into());
    let settings = window.global::<view::SettingsUi>();
    settings.set_section(if general {
        0
    } else if mode.starts_with("api") {
        1
    } else if mode == "ai-engine" {
        6
    } else {
        2
    });
    if mode.starts_with("api") && mode != "api-empty" {
        let catalog = neuromita::generation::PresetCatalog::new(Default::default());
        let definition = neuromita::generation::GeminiPresetDefinition {
            options: Default::default(),
            transforms: Vec::new(),
            endpoint: neuromita::generation::gemini::GeminiTemplate::GOOGLE_ENDPOINT.into(),
            model: "fixture-model".into(),
            credential: "SNAPSHOT_UNUSED_KEY".into(),
            temperature: 0.7,
            max_output_tokens: 128,
            timeout_ms: 60_000,
        };
        let unconfigured = mode == "api-new";
        let name = if unconfigured {
            "Пользовательский API 1"
        } else {
            "QA preset"
        };
        let record = neuromita::generation::PresetRecord {
            name: name.into(),
            definition: if unconfigured {
                neuromita::generation::PresetDefinition::Unconfigured
            } else {
                neuromita::generation::PresetDefinition::Gemini(definition.clone())
            },
        };
        let id = catalog.create(record)?;
        let row = view::PresetItem {
            key_error: "".into(),
            output_token_wire_key: "maxOutputTokens".into(),
            template_id: "gemini".into(),
            temperature_enabled: true,
            tokens_enabled: true,
            id: id.0.to_string().into(),
            name: name.into(),
            model: if unconfigured {
                "".into()
            } else {
                definition.model.clone().into()
            },
            provider: if unconfigured {
                "".into()
            } else {
                "google".into()
            },
            configured: !unconfigured,
            endpoint: if unconfigured {
                "".into()
            } else {
                definition.endpoint.clone().into()
            },
            key_env: if unconfigured {
                "".into()
            } else {
                definition.credential.to_string().into()
            },
            temperature: if unconfigured {
                "".into()
            } else {
                definition.temperature.to_string().into()
            },
            tokens: if unconfigured {
                "".into()
            } else {
                definition.max_output_tokens.to_string().into()
            },
            timeout: if unconfigured {
                "".into()
            } else {
                definition.timeout_ms.to_string().into()
            },
        };
        settings.set_settings_presets(slint::ModelRc::new(slint::VecModel::from(vec![row])));
        if !unconfigured {
            settings.set_settings_active(id.0.to_string().into());
        }
        settings.set_preset_edit_id(id.0.to_string().into());
        settings.set_preset_configured(!unconfigured);
        settings.set_preset_name(name.into());
        settings.set_preset_model(if unconfigured {
            "".into()
        } else {
            definition.model.into()
        });
        settings.set_preset_endpoint(if unconfigured {
            "".into()
        } else {
            definition.endpoint.into()
        });
        settings.set_preset_key_env(if unconfigured {
            "".into()
        } else {
            definition.credential.to_string().into()
        });
        settings.set_preset_temperature(if unconfigured {
            "".into()
        } else {
            definition.temperature.to_string().into()
        });
        settings.set_preset_tokens(if unconfigured {
            "".into()
        } else {
            definition.max_output_tokens.to_string().into()
        });
        settings.set_preset_timeout(if unconfigured {
            "".into()
        } else {
            definition.timeout_ms.to_string().into()
        });
        settings.set_preset_provider_icon(if unconfigured {
            "".into()
        } else {
            "google".into()
        });
        settings.set_preset_template_id(if unconfigured {
            "".into()
        } else {
            "gemini".into()
        });
        settings.set_preset_template_name(if unconfigured {
            "".into()
        } else {
            "Google AI Studio".into()
        });
        if !unconfigured {
            settings.set_preset_url_template(
                PresetsPresenter::request_url_template(
                    "gemini",
                    neuromita::generation::gemini::GeminiTemplate::GOOGLE_ENDPOINT,
                )
                .unwrap()
                .into(),
            );
        }
        let templates = PresetsPresenter::providers()
            .into_iter()
            .map(|provider| view::PresetTemplateItem {
                id: provider.id.into(),
                name: provider.name.into(),
                icon: provider.icon.into(),
                format: provider.schema_label.into(),
                documentation: provider.documentation.into(),
                pricing: provider.pricing.into(),
                badge_kind: provider.badge_kind.into(),
                endpoint: if provider.id == "gemini" {
                    neuromita::generation::gemini::GeminiTemplate::GOOGLE_ENDPOINT.into()
                } else {
                    "".into()
                },
                available: provider.supported,
            })
            .collect::<Vec<_>>();
        settings.set_preset_templates(slint::ModelRc::new(slint::VecModel::from(templates)));
        let parameters = PresetsPresenter::parameters()
            .into_iter()
            .map(|parameter| view::PresetParameterItem {
                id: parameter.id.into(),
                wire_key: parameter.wire_key.into(),
                title: parameter.title.source().into(),
                title_key: parameter.title.id().into(),

                description: parameter.description.source().into(),
                description_key: parameter.description.id().into(),

                editable: parameter.editable,
                sent: parameter.sent,
            })
            .collect::<Vec<_>>();
        settings.set_preset_parameters(slint::ModelRc::new(slint::VecModel::from(parameters)));
        settings.set_preset_tab(if mode == "api-generation" { 1 } else { 0 });
    }
    window.show()?;
    window.window().set_size(slint::PhysicalSize::new(
        1560,
        if mode == "ai-engine" { 958 } else { 918 },
    ));
    if mode == "api-dropdown" {
        let weak = window.as_weak();
        slint::Timer::single_shot(Duration::from_millis(250), move || {
            if let Some(window) = weak.upgrade() {
                let position = slint::LogicalPosition {
                    x: 1350.0,
                    y: 430.0,
                };
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
            }
        });
    }

    let result = Rc::new(RefCell::new(None));
    let completed = result.clone();
    let weak = window.as_weak();
    slint::Timer::single_shot(Duration::from_millis(500), move || {
        let outcome = (|| -> Result<(), Box<dyn Error>> {
            let window = weak.upgrade().ok_or("snapshot window closed")?;
            let image = window.window().take_snapshot()?;
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for pixel in image.as_bytes().as_chunks::<4>().0 {
                ppm.extend_from_slice(&pixel[..3]);
            }
            use std::io::Write;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?
                .write_all(&ppm)?;
            window.hide()?;
            Ok(())
        })();
        *completed.borrow_mut() = Some(outcome.map_err(|error| error.to_string()));
        let _ = slint::quit_event_loop();
    });
    slint::run_event_loop()?;
    result
        .borrow_mut()
        .take()
        .ok_or("snapshot callback did not run")?
        .map_err(Into::into)
}
