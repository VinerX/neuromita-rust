use super::{
    runtime::Command,
    view::{
        AppWindow, CharacterPresets, PresetItem, PresetParameterItem, PresetTemplateItem,
        PromptSetItem, SettingsUi,
    },
};
use neuromita::{
    character::CharacterId,
    generation::{PresetConnectionEdit, PresetGenerationEdit, PresetId, PresetRecord},
    presentation::{GuiPresenter, PresetSlot, PresetsPresenter, PresetsView},
    settings::{PersistenceState, PresetSelection, SettingsArea, SettingsSnapshot},
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{rc::Rc, sync::Arc};

const GUI_SETTINGS_AREAS: [SettingsArea; 11] = [
    SettingsArea::Api,
    SettingsArea::Characters,
    SettingsArea::Voice,
    SettingsArea::Language,
    SettingsArea::Asr,
    SettingsArea::AiEngine,
    SettingsArea::Games,
    SettingsArea::Models,
    SettingsArea::Images,
    SettingsArea::Updates,
    SettingsArea::Datasets,
];

fn area_for_section(index: i32) -> Option<SettingsArea> {
    usize::try_from(index)
        .ok()
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| GUI_SETTINGS_AREAS.get(index).copied())
}

pub struct AdministrationSnapshot {
    pub settings: Arc<SettingsSnapshot>,
    pub presets: PresetsView,
    pub prompt_sets: Vec<neuromita::prompts::PromptSetInfo>,
}

pub fn execute(api: &GuiPresenter, command: Command) -> Result<(), String> {
    match command {
        Command::SelectPromptSet(character, name) => {
            let catalog = api
                .prompts
                .as_ref()
                .ok_or("Prompt catalog is not configured")?;
            catalog
                .resolve_set(character, Some(&name))
                .map_err(|e| e.to_string())?;
            api.settings
                .characters
                .for_character(character)
                .prompt_set
                .set(Some(name))
                .map_err(|e| e.to_string())?;
        }
        Command::UpdatePreset(id, record) => api
            .presets
            .update_connection(id, record)
            .map_err(|e| e.to_string())?,
        Command::DeletePreset(id) => api.presets.delete(id).map_err(|e| e.to_string())?,
        Command::ActivePreset(id) => {
            api.presets
                .require_configured(id)
                .map_err(|e| e.to_string())?;
            api.settings
                .presets
                .active
                .set(id)
                .map_err(|e| e.to_string())?;
        }
        Command::AssignAllPresets(character, selection) => {
            if let PresetSelection::Specific(id) = selection {
                api.presets.require(id).map_err(|e| e.to_string())?;
            }
            api.settings
                .characters
                .for_character(character)
                .assignments
                .set(neuromita::settings::OperationPresets {
                    reply: selection,
                    react: selection,
                    initiate: selection,
                })
                .map_err(|e| e.to_string())?;
        }
        Command::AssignPreset(character, operation, selection) => {
            if let PresetSelection::Specific(id) = selection {
                api.presets.require(id).map_err(|e| e.to_string())?;
            }
            let settings = api.settings.characters.for_character(character);
            let target = match operation {
                PresetSlot::Reply => &settings.reply,
                PresetSlot::React => &settings.react,
                PresetSlot::Initiate => &settings.initiate,
            };
            target.set(selection).map_err(|e| e.to_string())?;
        }
        Command::SaveTts(mut value) => {
            let current = api.settings.tts.preferences.get();
            value.character_voice_ids = current.character_voice_ids;
            value.default_voice_id = current.default_voice_id;
            api.settings
                .tts
                .preferences
                .set(value)
                .map_err(|e| e.to_string())?;
        }
        Command::SaveCharacterVoice(character, voice_id) => {
            if voice_id.len() > 512 {
                return Err("Invalid voice ID".into());
            }
            let mut value = api.settings.tts.preferences.get();
            if let Some(id) = character {
                value.character_voice_ids.insert(id, voice_id);
            } else {
                value.default_voice_id = voice_id;
            }
            api.settings
                .tts
                .preferences
                .set(value)
                .map_err(|e| e.to_string())?;
        }
        Command::SelectAsrRecognizer(model_id) => {
            let mut value = api.settings.asr.preferences.get();
            value.recognizer_id = model_id;
            api.settings
                .asr
                .preferences
                .set(value)
                .map_err(|e| e.to_string())?;
        }
        Command::SaveAsr(mut value, language) => {
            let current = api.settings.asr.preferences.get();
            value.recognizer_id = current.recognizer_id;
            value.model_languages = current.model_languages;
            if let Some(language) = language
                && !value.recognizer_id.is_empty()
            {
                value
                    .model_languages
                    .insert(value.recognizer_id.clone(), language);
            }
            api.settings
                .asr
                .preferences
                .set(value)
                .map_err(|e| e.to_string())?;
        }
        Command::VoiceInLauncher(value) => api
            .settings
            .voice
            .in_launcher
            .set(value)
            .map_err(|e| e.to_string())?,
        Command::VoiceInGame(value) => api
            .settings
            .voice
            .in_game
            .set(value)
            .map_err(|e| e.to_string())?,
        Command::SectionVisibility(area, enabled) => api
            .settings
            .interface
            .set_visible(area, enabled)
            .map_err(|e| e.to_string())?,
        Command::HidePrivate(value) => api
            .settings
            .interface
            .hide_private
            .set(value)
            .map_err(|e| e.to_string())?,
        Command::VoiceName(value) => api
            .settings
            .voice
            .voice
            .set(value)
            .map_err(|e| e.to_string())?,
        _ => return Err("Unsupported administration command".into()),
    }
    Ok(())
}

fn id(value: &str) -> Result<PresetId, String> {
    value
        .parse::<u32>()
        .map(PresetId)
        .map_err(|_| "Invalid preset ID".into())
}

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    let settings = window.global::<SettingsUi>();
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_preset_new(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        if ui.get_preset_dirty() {
            ui.set_preset_pending_id("__new__".into());
        } else {
            send(Command::NewPreset);
        }
    });
    let weak = window.as_weak();
    settings.on_preset_template_select(move |id| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<SettingsUi>();
            if id.is_empty() {
                ui.set_preset_template_id("".into());
                ui.set_preset_template_name("".into());
                ui.set_preset_provider_icon("".into());
                ui.set_preset_schema_label("".into());
                ui.set_preset_url_template("".into());
                ui.set_preset_endpoint("".into());
                ui.set_preset_model("".into());
                ui.set_preset_key_env("".into());
                ui.set_preset_temperature("".into());
                ui.set_preset_tokens("".into());
                ui.set_preset_timeout("".into());
            } else if ui
                .get_preset_templates()
                .iter()
                .any(|item| item.id == id && item.available)
            {
                let template = ui.get_preset_templates().iter().find(|item| item.id == id);
                if let Some(template) = template {
                    ui.set_preset_template_id(id);
                    ui.set_preset_template_name(template.name);
                    ui.set_preset_provider_icon(template.icon);
                    ui.set_preset_schema_label(template.format);
                    let endpoint = if ui.get_preset_endpoint().is_empty() {
                        ui.set_preset_endpoint(template.endpoint.clone());
                        template.endpoint
                    } else {
                        ui.get_preset_endpoint()
                    };
                    let request_url_template = PresetsPresenter::request_url_template(
                        ui.get_preset_template_id().as_str(),
                        &endpoint,
                    )
                    .unwrap_or_default();
                    ui.set_preset_url_template(request_url_template.into());
                    match PresetsPresenter::template(ui.get_preset_template_id().as_str()) {
                        Ok(template) => {
                            ui.set_preset_output_token_wire_key(
                                template.output_token_wire_key().into(),
                            );
                            ui.set_preset_temperature_enabled(template.send_temperature);
                            ui.set_preset_tokens_enabled(template.send_max_output_tokens);
                            if ui.get_preset_temperature().is_empty() {
                                ui.set_preset_temperature(template.temperature.to_string().into());
                            }
                            if ui.get_preset_tokens().is_empty() {
                                ui.set_preset_tokens(template.max_output_tokens.to_string().into());
                            }
                            if ui.get_preset_timeout().is_empty() {
                                ui.set_preset_timeout(template.timeout_ms.to_string().into());
                            }
                        }
                        Err(error) => ui.set_settings_error(error.to_string().into()),
                    }
                }
            } else {
                ui.set_settings_error("Этот провайдер пока не поддерживается ядром".into());
            }
        }
    });
    let weak = window.as_weak();
    settings.on_preset_documentation(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let chosen = ui.get_preset_template_id();
        let trusted = PresetsPresenter::providers()
            .into_iter()
            .find(|provider| chosen == provider.id && provider.supported);
        if let Some(provider) =
            trusted.filter(|provider| provider.documentation.starts_with("https://"))
            && let Err(error) = super::binding::open_external_url(provider.documentation)
        {
            ui.set_settings_error(error.into());
        }
    });
    let weak = window.as_weak();
    settings.on_preset_help(move |kind| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let selected = ui.get_preset_template_id();
        let target = PresetsPresenter::providers()
            .into_iter()
            .find(|provider| selected == provider.id && provider.supported)
            .map(|provider| {
                if kind == 1 {
                    provider.models_url
                } else if kind == 2 {
                    provider.credentials_url
                } else {
                    ""
                }
            })
            .filter(|link| link.starts_with("https://"));
        if let Some(url) = target
            && let Err(error) = super::binding::open_external_url(url)
        {
            ui.set_settings_error(error.into());
        }
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    settings.on_preset_save(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        let record = (|| -> Result<PresetConnectionEdit, String> {
            if window.get_preset_edit_id().is_empty() {
                return Err("Choose a preset before editing".into());
            }
            if window.get_preset_name().trim().is_empty() {
                return Err("Preset name is required".into());
            }
            if window.get_preset_template_id().is_empty() {
                return Ok(PresetConnectionEdit {
                    key: None,
                    name: window.get_preset_name().into(),
                    connection: None,
                });
            }
            let template = PresetsPresenter::template(window.get_preset_template_id().as_str())
                .map_err(|e| e.to_string())?;
            Ok(PresetConnectionEdit {
                key: Some(neuromita::generation::SecretKey::new(
                    window.get_preset_key_env().to_string(),
                )),
                name: window.get_preset_name().into(),
                connection: Some(PresetGenerationEdit {
                    protocol: template.protocol,
                    send_temperature: window.get_preset_temperature_enabled(),
                    send_max_output_tokens: window.get_preset_tokens_enabled(),
                    endpoint: window.get_preset_endpoint().into(),
                    model: window.get_preset_model().into(),
                    credential: None,
                    temperature: window
                        .get_preset_temperature()
                        .parse()
                        .map_err(|_| "Invalid temperature")?,
                    max_output_tokens: window
                        .get_preset_tokens()
                        .parse()
                        .map_err(|_| "Invalid output token limit")?,
                    timeout_ms: window
                        .get_preset_timeout()
                        .parse::<u64>()
                        .map_err(|_| "Invalid timeout")?,
                }),
            })
        })();
        match record {
            Ok(record) => match id(&window.get_preset_edit_id()) {
                Ok(id) => send(Command::UpdatePreset(id, record)),
                Err(error) => window.set_settings_error(error.into()),
            },
            Err(error) => window.set_settings_error(error.into()),
        }
    });
    let weak = window.as_weak();
    settings.on_preset_edit(move |selected| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        if selected == window.get_preset_edit_id() {
            return;
        }
        if window.get_preset_dirty() {
            window.set_preset_pending_id(selected);
            return;
        }
        if let Some(record) = window
            .get_settings_presets()
            .iter()
            .find(|r| r.id == selected)
        {
            select_preset(&window, &record);
        } else {
            window.set_settings_error("Preset is no longer available".into());
        }
    });
    let weak = window.as_weak();
    settings.on_preset_confirm_cancel(move || {
        if let Some(component) = weak.upgrade() {
            component
                .global::<SettingsUi>()
                .set_preset_pending_id("".into());
        }
    });
    let weak = window.as_weak();
    settings.on_preset_confirm_save(move || {
        if let Some(component) = weak.upgrade() {
            let ui = component.global::<SettingsUi>();
            ui.set_preset_pending_save(true);
            ui.invoke_preset_save();
        }
    });
    let weak = window.as_weak();
    let send = dispatch.clone();
    settings.on_preset_confirm_discard(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let target = ui.get_preset_pending_id();
        ui.set_preset_pending_id("".into());
        if target == "__new__" {
            send(Command::NewPreset);
        } else if let Some(record) = ui
            .get_settings_presets()
            .iter()
            .find(|row| row.id == target)
        {
            select_preset(&ui, &record);
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_preset_delete(move |value| {
        if let Some(component) = weak.upgrade() {
            let window = component.global::<SettingsUi>();
            match id(&value) {
                Ok(id) => send(Command::DeletePreset(id)),
                Err(error) => window.set_settings_error(error.into()),
            }
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_preset_activate(move |value| {
        if let Some(component) = weak.upgrade() {
            let window = component.global::<SettingsUi>();
            match id(&value) {
                Ok(id) => send(Command::ActivePreset(id)),
                Err(error) => window.set_settings_error(error.into()),
            }
        }
    });
    let send = dispatch.clone();
    settings.on_character_prompt_set(move |character, name| {
        let Some(character) = CharacterId::from_index(character) else {
            return;
        };
        if !name.is_empty() {
            send(Command::SelectPromptSet(character, name.into()));
        }
    });
    let send = dispatch.clone();
    settings.on_prompt_reload(move || send(Command::RefreshPromptCatalog));
    let send = dispatch.clone();
    settings.on_prompt_open_folder(move |index| {
        if let Some(character) = CharacterId::from_index(index) {
            send(Command::OpenPromptFolder(character));
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_character_preset(move |character, operation, value| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let window = component.global::<SettingsUi>();
        let character = match CharacterId::from_index(character) {
            Some(id) => id,
            None => {
                window.set_settings_error("Unknown character".into());
                return;
            }
        };
        let operation = match operation {
            0 => PresetSlot::Reply,
            1 => PresetSlot::React,
            2 => PresetSlot::Initiate,
            _ => {
                window.set_settings_error("Unknown operation".into());
                return;
            }
        };
        let selection = if value == "active" {
            Ok(PresetSelection::Active)
        } else {
            id(&value).map(PresetSelection::Specific)
        };
        match selection {
            Ok(selection) => send(Command::AssignPreset(character, operation, selection)),
            Err(error) => window.set_settings_error(error.into()),
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    settings.on_character_common(move |character, value| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<SettingsUi>();
        let character = match CharacterId::from_index(character) {
            Some(id) => id,
            None => {
                ui.set_settings_error("Unknown character".into());
                return;
            }
        };
        let selection = if value == "active" {
            Ok(PresetSelection::Active)
        } else {
            id(&value).map(PresetSelection::Specific)
        };
        match selection {
            Ok(selection) => send(Command::AssignAllPresets(character, selection)),
            Err(error) => ui.set_settings_error(error.into()),
        }
    });
    let send = dispatch.clone();
    settings.on_section_visibility(move |index, checked| {
        if let Some(area) = area_for_section(index) {
            send(Command::SectionVisibility(area, checked));
        }
    });
    let send = dispatch.clone();
    settings.on_set_hide_private(move |checked| send(Command::HidePrivate(checked)));
    let send = dispatch.clone();
    settings.on_voice_launcher(move |value| send(Command::VoiceInLauncher(value)));
    let send = dispatch.clone();
    settings.on_voice_game(move |value| send(Command::VoiceInGame(value)));
    let send = dispatch.clone();
    settings.on_volume(move |value| {
        if let Ok(value) = u8::try_from(value) {
            send(Command::Volume(value));
        }
    });
    settings.on_voice_name(move |value| dispatch(Command::VoiceName(value.into())));
}

pub fn render(window: &AppWindow, model: &AdministrationSnapshot) {
    let window = window.global::<SettingsUi>();
    let settings = &model.settings;
    window.set_settings_revision(settings.revision.to_string().into());
    window.set_section_visible(ModelRc::new(VecModel::from(
        std::iter::once(true)
            .chain(
                GUI_SETTINGS_AREAS
                    .iter()
                    .map(|area| settings.values.interface.visibility.is_visible(*area)),
            )
            .collect::<Vec<_>>(),
    )));
    window.set_hide_private(settings.values.interface.hide_private);
    window.set_settings_persistence(match settings.persistence {
        PersistenceState::Pending => 0,
        PersistenceState::Saved(_) => 1,
        PersistenceState::Failed { .. } => 2,
    });
    if let PersistenceState::Failed { error, .. } = &settings.persistence {
        window.set_settings_error(error.to_string().into());
    }
    window.set_settings_active(settings.values.presets.active.0.to_string().into());
    window.set_settings_volume(i32::from(settings.values.voice.volume.get()));
    window.set_settings_voice_launcher(settings.values.voice.in_launcher);
    window.set_settings_voice_game(settings.values.voice.in_game);
    window.set_settings_voice_name(settings.values.voice.voice.as_str().into());
    let previous = window.get_character_presets();
    let encode = |value: PresetSelection| match value {
        PresetSelection::Active => "active".into(),
        PresetSelection::Specific(id) => id.0.to_string().into(),
    };
    let rows = CharacterId::ALL
        .into_iter()
        .map(|id| {
            let presets = settings.values.characters.for_character(id).presets;
            let mut row = CharacterPresets {
                reply: encode(presets.reply),
                react: encode(presets.react),
                initiate: encode(presets.initiate),
                split: presets.reply != presets.react || presets.reply != presets.initiate,
            };
            if let Some(old) = previous.row_data(id.index())
                && old.reply == row.reply
                && old.react == row.react
                && old.initiate == row.initiate
            {
                row.split = old.split;
            }
            row
        })
        .collect::<Vec<_>>();
    window.set_character_presets(ModelRc::new(VecModel::from(rows)));
    let prompt_options = model
        .prompt_sets
        .iter()
        .map(|set| PromptSetItem {
            character: set.character.index() as i32,
            name: set.name.as_str().into(),
            author: set.author.as_str().into(),
            version: set.version.as_str().into(),
            description: set.description.as_str().into(),
        })
        .collect::<Vec<_>>();
    window.set_prompt_set_options(ModelRc::new(VecModel::from(prompt_options)));
    let prompt_selections = CharacterId::ALL
        .into_iter()
        .map(|id| {
            let requested = settings
                .values
                .characters
                .for_character(id)
                .prompt_set
                .as_deref();
            if let Some(name) = requested {
                if model
                    .prompt_sets
                    .iter()
                    .any(|set| set.character == id && set.name == name)
                {
                    return name.into();
                }
                return "".into();
            }
            model
                .prompt_sets
                .iter()
                .filter(|set| set.character == id)
                .find(|set| set.name.eq_ignore_ascii_case("Default"))
                .or_else(|| model.prompt_sets.iter().find(|set| set.character == id))
                .map(|set| set.name.as_str())
                .unwrap_or_default()
                .into()
        })
        .collect::<Vec<_>>();
    window.set_character_prompt_sets(ModelRc::new(VecModel::from(prompt_selections)));
    let templates = provider_templates(&model.presets.providers);
    window.set_preset_templates(ModelRc::new(VecModel::from(templates)));
    let parameters = model
        .presets
        .parameters
        .iter()
        .map(|parameter| PresetParameterItem {
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
    window.set_preset_parameters(ModelRc::new(VecModel::from(parameters)));
    let records = model
        .presets
        .records
        .iter()
        .map(|record| PresetItem {
            output_token_wire_key: record.output_token_wire_key.into(),
            template_id: record.template_id.into(),
            temperature_enabled: record.send_temperature,
            tokens_enabled: record.send_max_output_tokens,
            id: record.id.0.to_string().into(),
            name: record.name.as_str().into(),
            model: record.model.as_str().into(),
            provider: record.provider.into(),
            endpoint: record.endpoint.as_str().into(),
            key_env: record
                .api_key
                .as_ref()
                .map(|key| key.expose())
                .unwrap_or_default()
                .into(),
            key_error: record
                .credential_error
                .as_deref()
                .unwrap_or_default()
                .into(),
            temperature: if record.configured {
                record.temperature.to_string().into()
            } else {
                "".into()
            },
            tokens: if record.configured {
                record.max_output_tokens.to_string().into()
            } else {
                "".into()
            },
            timeout: if record.configured {
                record.timeout_ms.to_string().into()
            } else {
                "".into()
            },
            configured: record.configured,
        })
        .collect::<Vec<_>>();
    let selected = window.get_preset_edit_id();
    if !window.get_preset_creating()
        && (selected.is_empty() || !records.iter().any(|record| record.id == selected))
    {
        let active = settings.values.presets.active.0.to_string();
        let first = records
            .iter()
            .find(|record| record.id == active)
            .or_else(|| records.first());
        if let Some(record) = first {
            select_preset(&window, record);
        }
    }
    if records.is_empty() {
        clear_preset(&window);
    } else if let Some(record) = records.iter().find(|r| r.id == window.get_preset_edit_id())
        && record.configured != window.get_preset_configured()
    {
        select_preset(&window, record);
    }
    window.set_settings_presets(ModelRc::new(VecModel::from(records)));
}

fn mark_preset_baseline(ui: &SettingsUi) {
    ui.set_preset_baseline_name(ui.get_preset_name());
    ui.set_preset_baseline_model(ui.get_preset_model());
    ui.set_preset_baseline_endpoint(ui.get_preset_endpoint());
    ui.set_preset_baseline_key_env(ui.get_preset_key_env());
    ui.set_preset_baseline_template(ui.get_preset_template_id());
    ui.set_preset_baseline_temperature_enabled(ui.get_preset_temperature_enabled());
    ui.set_preset_baseline_tokens_enabled(ui.get_preset_tokens_enabled());
    ui.set_preset_baseline_temperature(ui.get_preset_temperature());
    ui.set_preset_baseline_tokens(ui.get_preset_tokens());
    ui.set_preset_baseline_timeout(ui.get_preset_timeout());
}

fn select_preset(ui: &SettingsUi, record: &PresetItem) {
    ui.set_preset_creating(false);
    ui.set_preset_configured(record.configured);
    ui.set_preset_delete_id("".into());
    ui.set_preset_edit_id(record.id.clone());
    ui.set_preset_name(record.name.clone());
    ui.set_preset_model(record.model.clone());
    ui.set_preset_endpoint(record.endpoint.clone());
    ui.set_preset_key_revealed(false);
    ui.set_preset_key_env(record.key_env.clone());
    if !record.key_error.is_empty() {
        ui.set_settings_error(record.key_error.clone());
    }
    ui.set_preset_temperature_enabled(record.temperature_enabled);
    ui.set_preset_tokens_enabled(record.tokens_enabled);
    ui.set_preset_output_token_wire_key(record.output_token_wire_key.clone());
    ui.set_preset_temperature(record.temperature.clone());
    ui.set_preset_tokens(record.tokens.clone());
    ui.set_preset_timeout(record.timeout.clone());
    ui.set_preset_template_id("".into());
    ui.set_preset_template_name("".into());
    ui.set_preset_provider_icon("".into());
    ui.set_preset_schema_label("".into());
    ui.set_preset_url_template("".into());
    if record.configured {
        if let Some(template) = ui
            .get_preset_templates()
            .iter()
            .find(|item| item.id == record.template_id)
        {
            ui.set_preset_template_id(template.id);
            ui.set_preset_template_name(template.name);
            ui.set_preset_provider_icon(record.provider.clone());
            ui.set_preset_schema_label(template.format);
            if let Some(request_url_template) = PresetsPresenter::request_url_template(
                record.template_id.as_str(),
                &record.endpoint,
            ) {
                ui.set_preset_url_template(request_url_template.into());
            }
        } else {
            ui.set_settings_error("Provider metadata is unavailable for this preset".into());
        }
    }
    mark_preset_baseline(ui);
}

pub fn clear_preset(ui: &SettingsUi) {
    ui.set_preset_edit_id("".into());
    ui.set_preset_configured(false);
    ui.set_preset_name("".into());
    ui.set_preset_model("".into());
    ui.set_preset_endpoint("".into());
    ui.set_preset_key_env("".into());
    ui.set_preset_temperature("".into());
    ui.set_preset_tokens("".into());
    ui.set_preset_timeout("".into());
    ui.set_preset_template_id("".into());
    ui.set_preset_template_name("".into());
    ui.set_preset_provider_icon("".into());
    ui.set_preset_schema_label("".into());
    ui.set_preset_url_template("".into());
    ui.set_preset_delete_id("".into());
    mark_preset_baseline(ui);
}

pub fn created(window: &AppWindow, id: PresetId, record: &PresetRecord) {
    let ui = window.global::<SettingsUi>();
    let item = PresetItem {
        key_error: "".into(),
        output_token_wire_key: "".into(),
        template_id: "".into(),
        temperature_enabled: false,
        tokens_enabled: false,
        id: id.0.to_string().into(),
        name: record.name.as_str().into(),
        model: "".into(),
        provider: "".into(),
        endpoint: "".into(),
        key_env: "".into(),
        temperature: "".into(),
        tokens: "".into(),
        timeout: "".into(),
        configured: false,
    };
    select_preset(&ui, &item);
    ui.set_preset_search("".into());
    ui.set_preset_tab(0);
}

pub fn saved(window: &AppWindow, edited: Option<PresetId>) {
    let ui = window.global::<SettingsUi>();
    ui.set_settings_error("".into());
    if edited.is_some_and(|id| ui.get_preset_edit_id() == id.0.to_string()) {
        mark_preset_baseline(&ui);
        if ui.get_preset_pending_save() {
            let target = ui.get_preset_pending_id();
            ui.set_preset_pending_id("".into());
            ui.set_preset_pending_save(false);
            if target == "__new__" {
                ui.invoke_preset_new();
            } else if !target.is_empty() {
                ui.invoke_preset_edit(target);
            }
        }
    }
}

pub(super) fn provider_templates(
    providers: &[neuromita::presentation::ProviderOption],
) -> Vec<PresetTemplateItem> {
    providers
        .iter()
        .map(|provider| PresetTemplateItem {
            id: provider.id.into(),
            name: provider.name.into(),
            icon: provider.icon.into(),
            format: provider.schema_label.into(),
            endpoint: PresetsPresenter::template(provider.id)
                .map(|template| template.endpoint)
                .unwrap_or_default()
                .into(),
            documentation: provider.documentation.into(),
            pricing: provider.pricing.into(),
            badge_kind: provider.badge_kind.into(),
            available: provider.supported,
        })
        .collect::<Vec<_>>()
}
