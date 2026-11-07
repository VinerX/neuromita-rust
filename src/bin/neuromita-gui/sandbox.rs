use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use neuromita::{
    character::CharacterId,
    presentation::ConversationSnapshot,
    request::{ProcessingStage, RequestId, RequestOutcome, RequestState},
    timeline::domain::MessageRole,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{
    runtime::{Command, DesktopRuntime, Update},
    view::{
        AppWindow, CharacterCatalog, CharacterOption, ChatRow, ChatStatus, ConversationUi,
        MemoryUi, SettingsUi,
    },
};

pub fn attach(
    window: &AppWindow,
    directory: PathBuf,
    model: Option<String>,
) -> std::io::Result<DesktopRuntime> {
    let memory_editor =
        std::sync::Arc::new(std::sync::Mutex::new(super::memory::MemoryEditor::default()));
    let editor = memory_editor.clone();
    window.set_chat_model_input(model.clone().unwrap_or_default().into());
    let weak = window.as_weak();
    let runtime = DesktopRuntime::spawn(directory, model, move |update| {
        let editor = editor.clone();
        let _ = weak.upgrade_in_event_loop(move |window| apply(&window, update, &editor));
    })?;
    let weak = window.as_weak();
    let commands = runtime.commands.clone();
    let queue_editor = memory_editor.clone();
    let dispatch = std::rc::Rc::new(move |command| {
        if let Err(error) = commands.try_send(command)
            && let Some(window) = weak.upgrade()
        {
            if let Command::MemoryEdit(edit_id, ..) = error.into_inner() {
                super::memory::failed(
                    &window,
                    &queue_editor,
                    edit_id,
                    "Desktop command queue is full or closed".into(),
                );
            }
            window.set_chat_sending(false);
            window.set_core_connecting(false);
            window.set_chat_error("Desktop command queue is full or closed".into());
            window
                .global::<SettingsUi>()
                .set_settings_error("Desktop command queue is full or closed".into());
        }
    });
    super::settings::bind(window, dispatch.clone());
    super::ai_engine::bind(window, dispatch.clone());
    super::voice::bind(window, dispatch.clone());
    super::asr::bind(window, dispatch.clone());
    super::releases::bind(window, dispatch.clone());
    super::memory::bind(window, dispatch.clone(), &memory_editor);
    let send = dispatch.clone();
    window.global::<ConversationUi>().on_preview(move |kind| {
        let operation = match kind {
            1 => neuromita::presentation::ConversationChange::ClearMessages,
            2 => neuromita::presentation::ConversationChange::ResetContext,
            _ => return,
        };
        send(Command::Preview(operation));
    });
    let send = dispatch.clone();
    window
        .global::<ConversationUi>()
        .on_confirm(move || send(Command::Confirm));
    let send = dispatch.clone();
    window
        .global::<ConversationUi>()
        .on_abort(move || send(Command::Abort));
    let send = dispatch.clone();
    window
        .global::<ConversationUi>()
        .on_trace(move || send(Command::Diagnostics));

    let send = dispatch.clone();
    let weak = window.as_weak();
    window.on_chat_send(move |text| {
        if let Some(window) = weak.upgrade() {
            window.set_chat_sending(true);
        }
        send(Command::Send(text.into()));
    });
    let send = dispatch.clone();
    window.on_chat_select_character(move |id| {
        let Some(character) = CharacterId::from_index(id) else {
            return;
        };
        send(Command::Select(character));
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    window.on_chat_cancel(move |id| match uuid::Uuid::parse_str(&id) {
        Ok(id) => send(Command::Cancel(RequestId(id))),
        Err(error) => {
            if let Some(window) = weak.upgrade() {
                window.set_chat_error(error.to_string().into());
            }
        }
    });
    let send = dispatch.clone();
    window.on_chat_refresh(move || send(Command::Refresh));
    window.on_chat_set_volume(move |value| {
        if let Ok(value) = u8::try_from(value) {
            dispatch(Command::Volume(value));
        }
    });
    let weak = window.as_weak();
    window.on_chat_dismiss_error(move || {
        if let Some(window) = weak.upgrade() {
            window.set_chat_error("".into());
        }
    });
    Ok(runtime)
}

pub(super) fn render_catalog(
    window: &AppWindow,
    catalog: Vec<neuromita::character::CharacterDefinition>,
) {
    let entries = catalog
        .into_iter()
        .enumerate()
        .map(|(index, item)| CharacterOption {
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
        .global::<CharacterCatalog>()
        .set_entries(ModelRc::new(VecModel::from(entries)));
}

fn apply(window: &AppWindow, update: Update, memory_editor: &super::memory::EditorState) {
    match update {
        Update::RecognizedText {
            text,
            auto_send_after,
            merge_with_draft,
        } => {
            let previous = window.get_chat_draft();
            let combined = if previous.trim().is_empty() {
                text
            } else {
                format!(
                    "{}{}{}",
                    previous,
                    if merge_with_draft { " " } else { "\n" },
                    text
                )
            };
            window.set_chat_draft(combined.clone().into());
            if let Some(delay) = auto_send_after {
                if delay.is_zero() {
                    window.invoke_chat_send(combined.into());
                } else {
                    let weak = window.as_weak();
                    slint::Timer::single_shot(delay, move || {
                        if let Some(window) = weak.upgrade()
                            && window.get_chat_draft() == combined
                            && !window.get_chat_sending()
                        {
                            window.invoke_chat_send(combined.into());
                        }
                    });
                }
            }
        }
        Update::SpeechSessionStatus(snapshot) => super::asr::session_status(window, &snapshot),
        Update::MicrophoneTestStatus(snapshot) => {
            let ui = window.global::<super::view::AsrUi>();
            ui.set_testing(snapshot.running);
            ui.set_mic_level(i32::from(snapshot.level));
            ui.set_monitored_device(
                ui.get_device_entries()
                    .iter()
                    .find(|entry| entry.id == snapshot.device.as_str())
                    .map(|entry| entry.caption)
                    .unwrap_or_else(|| snapshot.device.into()),
            );
            if let Some(error) = snapshot.error {
                window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into());
            }
        }
        Update::RuntimeStatus(snapshot) => {
            super::voice::runtime_status(window, &snapshot);
            super::asr::runtime_status(window, &snapshot);
        }
        Update::CharacterCatalog(catalog) => render_catalog(window, catalog),
        Update::SpeechInventory(catalog, devices) => {
            super::voice::inventory(window, &catalog);
            super::asr::inventory(window, &catalog, &devices);
        }
        Update::Releases(snapshot) => super::releases::render(window, &snapshot),
        Update::ReleaseError(error) => window.set_release_error(error.into()),
        Update::Memory(page) => super::memory::render(window, memory_editor, &page),
        Update::MemoryError(error) => window.global::<MemoryUi>().set_error(error.into()),
        Update::MemorySaved(edit_id) => super::memory::saved(window, memory_editor, edit_id),
        Update::MemoryEditError(edit_id, error) => {
            super::memory::failed(window, memory_editor, edit_id, error)
        }
        Update::Confirmation(preview) => {
            let ui = window.global::<ConversationUi>();
            match preview {
                Some((operation, count)) => {
                    ui.set_count(i32::try_from(count).unwrap_or(i32::MAX));
                    ui.set_confirmation(match operation {
                        neuromita::presentation::ConversationChange::ClearMessages => 1,
                        neuromita::presentation::ConversationChange::ResetContext => 2,
                    });
                }
                None => ui.set_confirmation(0),
            }
        }
        Update::Diagnostics(value) => {
            window.global::<ConversationUi>().set_history(false);
            window
                .global::<ConversationUi>()
                .set_diagnostics(value.into());
        }
        Update::Hardware(snapshot) => super::ai_engine::render(window, snapshot),
        Update::Connections(snapshot) => {
            window.set_chat_clients(i32::try_from(snapshot.connected_clients).unwrap_or(i32::MAX));
            window.set_chat_server_running(snapshot.running);
        }
        Update::Administration(snapshot) => {
            super::settings::render(window, &snapshot);
            super::voice::render(window, &snapshot.settings.values.speech.tts);
            super::asr::render(window, &snapshot.settings.values.speech.asr);
        }
        Update::PresetCreated(id, record) => super::settings::created(window, id, &record),
        Update::AdminError(error) => {
            window.global::<SettingsUi>().set_preset_pending_save(false);
            window
                .global::<SettingsUi>()
                .set_settings_error(error.into());
        }
        Update::AdminSaved => super::settings::saved(window, None),
        Update::PresetSaved(id) => super::settings::saved(window, Some(id)),
        Update::Stopped => {
            let stopped = neuromita::ai_runtime::runtime_status::RuntimeSnapshot::default();
            super::voice::runtime_status(window, &stopped);
            super::asr::runtime_status(window, &stopped);
            super::asr::session_status(window, &Default::default());
            window.set_core_connected(false);
            window.set_core_connecting(false);
            window.set_chat_sending(false);
        }
        Update::Connecting => {
            window.set_core_connecting(true);
            window.set_chat_error("".into());
        }
        Update::Connected(address) => {
            window.set_core_connecting(false);
            window.set_core_connected(true);
            window.set_chat_address(address.into());
        }
        Update::Snapshot(snapshot) => render(window, &snapshot),
        Update::Sent(text) => {
            window.set_chat_sending(false);
            window.set_chat_error("".into());
            if window.get_chat_draft() == text {
                window.set_chat_draft("".into());
            }
        }
        Update::Error(error) => {
            if !window.get_core_connected() {
                window
                    .global::<SettingsUi>()
                    .set_settings_error(error.as_str().into());
            }
            window.set_core_connecting(false);
            window.set_chat_sending(false);
            window.set_chat_error(error.into());
        }
    }
}

fn render(window: &AppWindow, snapshot: &ConversationSnapshot) {
    window.set_chat_character(snapshot.character.index() as i32);
    window.set_chat_generation_ready(snapshot.model.is_ok());
    match &snapshot.model {
        Ok(model) => {
            window.set_chat_model(match &snapshot.preset_name {
                Some(name) => format!("{name} ({model})").into(),
                None => model.as_str().into(),
            });
            window.set_chat_generation_error("".into());
        }
        Err(error) => {
            window.set_chat_model("".into());
            window.set_chat_generation_error(error.to_string().into());
        }
    }
    window.set_chat_preset(format!("#{}", snapshot.preset.0).into());
    window.set_chat_anchor(
        format!(
            "branch: {}\nthrough: {}",
            snapshot.at.branch_id, snapshot.at.through
        )
        .into(),
    );
    window.set_chat_volume(i32::from(snapshot.settings.values.voice.volume.get()));
    window.set_chat_voice(snapshot.settings.values.voice.in_launcher);
    window.set_chat_attitude(format!("{:.1}", snapshot.state.attitude).into());
    window.set_chat_boredom(format!("{:.1}", snapshot.state.boredom).into());
    window.set_chat_stress(format!("{:.1}", snapshot.state.stress).into());
    window.set_chat_accepted(snapshot.state.accepted_replies.to_string().into());
    window.set_chat_active(
        snapshot
            .requests
            .requests
            .iter()
            .filter(|r| !r.snapshot.state.is_terminal())
            .count() as i32,
    );
    window.set_chat_messages(ModelRc::new(VecModel::from(rows(snapshot))));
    if let Some(error) = &snapshot.requests.discovery_error {
        window.set_chat_error(error.to_string().into());
    }
}

fn rows(snapshot: &ConversationSnapshot) -> Vec<ChatRow> {
    let committed: HashSet<_> = snapshot
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .filter_map(|m| m.request_id)
        .collect();
    let requests = snapshot
        .requests
        .requests
        .iter()
        .filter(|row| row.metadata.character_id == snapshot.character)
        .collect::<Vec<_>>();
    let accepted = requests
        .iter()
        .filter_map(|request| match &request.snapshot.state {
            RequestState::Finished(
                RequestOutcome::Success(response)
                | RequestOutcome::VoiceGenerationFailed { response, .. },
            ) => Some((response.event_id, *request)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut rows = snapshot
        .messages
        .iter()
        .rev()
        .take(200)
        .rev()
        .map(|message| {
            let mut row = ChatRow {
                text: message.content.clone().into(),
                user: message.speaker.as_deref() == Some("player")
                    || (message.speaker.is_none() && message.role == MessageRole::User),
                speaker: message.speaker.as_deref().unwrap_or_default().into(),
                character_index: message
                    .speaker
                    .as_deref()
                    .and_then(CharacterId::parse)
                    .map(|id| id.index() as i32)
                    .unwrap_or(-1),
                sequence: format!("#{}", message.sequence).into(),
                status: ChatStatus::History,
                ..Default::default()
            };
            if let Some(request) = accepted.get(&message.event_id) {
                row.status = ChatStatus::Success;
                if let RequestState::Finished(RequestOutcome::VoiceGenerationFailed {
                    error, ..
                }) = &request.snapshot.state
                {
                    row.status = ChatStatus::VoiceError;
                    row.detail = error.to_string().into();
                }
            }
            row
        })
        .collect::<Vec<_>>();
    for request in requests {
        if committed.contains(&request.metadata.id) {
            continue;
        }
        let mut row = ChatRow {
            request_id: request.metadata.id.to_string().into(),
            character_index: request.metadata.character_id.index() as i32,
            can_cancel: request.snapshot.can_cancel(),
            ..Default::default()
        };
        row.text = request
            .snapshot
            .provisional_text()
            .unwrap_or_default()
            .into();
        row.status = match &request.snapshot.state {
            RequestState::Pending => ChatStatus::Pending,
            RequestState::Processing(ProcessingStage::GeneratingText) => ChatStatus::Text,
            RequestState::Processing(ProcessingStage::TextReady { .. }) => ChatStatus::TextReady,
            RequestState::Processing(ProcessingStage::GeneratingVoice { .. }) => ChatStatus::Voice,
            RequestState::Processing(ProcessingStage::Accepting { .. }) => ChatStatus::Accepting,
            RequestState::Cancelled => ChatStatus::Cancelled,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(error)) => {
                row.detail = error.to_string().into();
                ChatStatus::TextError
            }
            RequestState::Finished(RequestOutcome::InternalError(error)) => {
                row.detail = error.to_string().into();
                row.text = error.partial_text.as_deref().unwrap_or_default().into();
                ChatStatus::InternalError
            }
            RequestState::Finished(
                RequestOutcome::Success(_) | RequestOutcome::VoiceGenerationFailed { .. },
            ) => continue,
        };
        if let Some(error) = &request.observation_error {
            row.detail = error.to_string().into();
        }
        rows.push(row);
    }
    rows
}
