use std::rc::Rc;

use neuromita::{
    ai_runtime::{
        AiService,
        runtime_status::{RuntimePhase, RuntimeSnapshot},
        speech_catalog::SpeechInventory,
        speech_languages::asr_language_capability,
    },
    audio::{
        controller::{SpeechSessionPhase, SpeechSessionSnapshot},
        input_catalog::AudioInputDevice,
    },
    settings::AsrPreferences,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{
    runtime::Command,
    view::{AppWindow, AsrUi, AudioDeviceEntry, SettingsUi},
};

fn model_id(label: &str) -> String {
    label.split_once(" · ").map_or(label, |(id, _)| id).into()
}

fn millis(value: &str, field: &str) -> Result<u32, String> {
    let number: f64 = value
        .trim()
        .replace(',', ".")
        .parse()
        .map_err(|_| format!("Некорректное значение: {field}"))?;
    if !number.is_finite() || !(0.0..=600.0).contains(&number) {
        return Err(format!("Значение {field} вне диапазона"));
    }
    Ok((number * 1000.0).round() as u32)
}

fn language_id(name: &str) -> String {
    name.rsplit_once(" (")
        .and_then(|(_, suffix)| suffix.strip_suffix(')'))
        .unwrap_or(name)
        .to_owned()
}

fn language_label(plugin_id: &str, language: &str) -> String {
    asr_language_capability(plugin_id)
        .and_then(|capability| {
            capability
                .choices
                .iter()
                .find(|choice| choice.id == language)
        })
        .map_or_else(|| language.to_owned(), |choice| choice.label.clone())
}

fn mode_id(name: &str) -> String {
    match name {
        "Рация — включать кнопкой" => "radio",
        "Слушать постоянно" => "continuous",
        "Нажать для разговора" => "push_to_talk",
        other => other,
    }
    .into()
}
fn mode_name(id: &str) -> String {
    match id {
        "radio" => "Рация — включать кнопкой",
        "continuous" => "Слушать постоянно",
        "push_to_talk" => "Нажать для разговора",
        other => other,
    }
    .into()
}

fn read(window: &AppWindow) -> Result<(AsrPreferences, Option<String>), String> {
    let ui = window.global::<AsrUi>();
    let model = model_id(&ui.get_recognizer());
    let options = AsrPreferences {
        recognizer_id: model,
        device_id: ui.get_device().into(),
        audio_backend: ui.get_audio_backend().into(),
        instant_send: ui.get_instant_send(),
        delayed_send: ui.get_delayed_send(),
        merge_chat: ui.get_merge_chat(),
        pause_while_speaking: ui.get_pause_while_speaking(),
        send_delay_ms: millis(&ui.get_send_delay(), "Пауза отправки")?,
        input_mode: mode_id(&ui.get_input_mode()),
        model_languages: Default::default(),
        chunk_size: ui
            .get_chunk_size()
            .parse()
            .map_err(|_| "Некорректный размер чанка")?,
        vad_threshold_milli: {
            let value = millis(&ui.get_vad_threshold(), "Порог VAD")?;
            value.try_into().map_err(|_| "Некорректный порог VAD")?
        },
        silence_ms: millis(&ui.get_silence_timeout(), "Порог тишины")?,
        pre_buffer_ms: millis(&ui.get_pre_buffer(), "Предбуфер")?,
        max_speech_ms: millis(&ui.get_max_speech(), "Максимальная речь")?,
        min_speech_ms: millis(&ui.get_min_speech(), "Минимальная речь")?,
    };
    Ok((
        options,
        (ui.get_recognition_available() && !ui.get_language().is_empty())
            .then(|| language_id(&ui.get_language())),
    ))
}

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>) {
    let ui = window.global::<AsrUi>();
    let weak_device = window.as_weak();
    ui.on_select_device(move |caption| {
        if let Some(window) = weak_device.upgrade() {
            let ui = window.global::<AsrUi>();
            if let Some(entry) = ui
                .get_device_entries()
                .iter()
                .find(|entry| entry.caption == caption)
            {
                ui.set_device(entry.id);
                ui.set_device_caption(entry.caption);
                ui.set_audio_backend(entry.backend);
                ui.invoke_persist();
            }
        }
    });
    let weak_numbers = window.as_weak();
    ui.on_step_number(move |value, minimum, maximum, step, decimals, up| {
        let parsed = value
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite());
        if let Some(number) = parsed {
            let adjusted = (number
                + if up {
                    f64::from(step)
                } else {
                    -f64::from(step)
                })
            .clamp(f64::from(minimum), f64::from(maximum));
            format!("{:.*}", decimals.clamp(0, 6) as usize, adjusted).into()
        } else {
            if let Some(window) = weak_numbers.upgrade() {
                window
                    .global::<super::view::SettingsUi>()
                    .set_settings_error("Invalid numeric value".into());
            }
            value
        }
    });
    ui.set_draft_enabled(true);
    ui.set_runtime_ready(false);
    ui.set_active(false);
    ui.set_status("—".into());
    ui.set_languages(ModelRc::new(VecModel::from(
        Vec::<slint::SharedString>::new(),
    )));
    ui.set_input_modes(ModelRc::new(VecModel::from(vec![
        "Рация — включать кнопкой".into(),
        "Слушать постоянно".into(),
        "Нажать для разговора".into(),
    ])));
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_persist(move || {
        if let Some(window) = weak.upgrade() {
            match read(&window) {
                Ok((settings, language)) => send(Command::SaveAsr(settings, language)),
                Err(error) => window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into()),
            }
        }
    });
    let send = dispatch.clone();
    let selection_window = window.as_weak();
    ui.on_recognizer_changed(move |value| {
        if let Some(window) = selection_window.upgrade() {
            let ui = window.global::<AsrUi>();
            ui.set_recognition_available(false);
            ui.set_language("".into());
        }
        send(Command::SelectAsrRecognizer(model_id(&value)));
    });
    let send = dispatch.clone();
    ui.on_refresh_models(move || send(Command::RefreshSpeechInventory));
    let refresh = dispatch.clone();
    ui.on_refresh_devices(move || refresh(Command::RefreshSpeechInventory));
    let send = dispatch.clone();
    ui.on_session_toggle(move |enabled| send(Command::SpeechSessionEnabled(enabled)));
    let send = dispatch.clone();
    let weak = window.as_weak();
    ui.on_test_microphone(move |enabled| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<AsrUi>();
            send(Command::MicrophoneTest(
                enabled,
                ui.get_audio_backend().into(),
                ui.get_device().into(),
            ));
        }
    });
    ui.on_push_to_talk(move |pressed| dispatch(Command::SpeechPushToTalk(pressed)));
}

pub fn render(window: &AppWindow, settings: &AsrPreferences) {
    let ui = window.global::<AsrUi>();
    let chosen = ui
        .get_recognizers()
        .iter()
        .find(|entry| model_id(entry) == settings.recognizer_id);
    let available = chosen.is_some();
    ui.set_recognition_available(available);
    ui.set_recognizer(chosen.unwrap_or_else(|| "".into()));
    ui.set_device(settings.device_id.clone().into());
    ui.set_device_caption(
        ui.get_device_entries()
            .iter()
            .find(|entry| entry.id == settings.device_id.as_str())
            .map(|entry| entry.caption)
            .unwrap_or_else(|| settings.device_id.clone().into()),
    );
    ui.set_audio_backend(settings.audio_backend.clone().into());
    ui.set_instant_send(settings.instant_send);
    ui.set_delayed_send(settings.delayed_send);
    ui.set_merge_chat(settings.merge_chat);
    ui.set_pause_while_speaking(settings.pause_while_speaking);
    ui.set_send_delay(format!("{:.2}", settings.send_delay_ms as f64 / 1000.0).into());
    ui.set_input_mode(mode_name(&settings.input_mode).into());
    let capability = if available {
        asr_language_capability(&settings.recognizer_id)
    } else {
        None
    };
    ui.set_languages(ModelRc::new(VecModel::from(
        capability
            .map(|capability| {
                capability
                    .choices
                    .iter()
                    .map(|choice| slint::SharedString::from(choice.label.as_str()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    )));
    let default_language = capability.map_or("", |capability| capability.default.as_str());
    let language = settings
        .model_languages
        .get(&settings.recognizer_id)
        .map(String::as_str)
        .unwrap_or(default_language);
    ui.set_language(if available {
        language_label(&settings.recognizer_id, language).into()
    } else {
        "".into()
    });
    ui.set_language_hint(
        capability
            .map_or("", |capability| capability.hint.as_str())
            .into(),
    );
    ui.set_sample_rate("16000".into());
    ui.set_chunk_size(settings.chunk_size.to_string().into());
    ui.set_vad_threshold(format!("{:.2}", f64::from(settings.vad_threshold_milli) / 1000.0).into());
    ui.set_silence_timeout(format!("{:.2}", settings.silence_ms as f64 / 1000.0).into());
    ui.set_pre_buffer(format!("{:.2}", settings.pre_buffer_ms as f64 / 1000.0).into());
    ui.set_max_speech(format!("{:.2}", settings.max_speech_ms as f64 / 1000.0).into());
    ui.set_min_speech(format!("{:.2}", settings.min_speech_ms as f64 / 1000.0).into());
}

pub fn inventory(window: &AppWindow, inventory: &SpeechInventory, devices: &[AudioInputDevice]) {
    let ui = window.global::<AsrUi>();
    ui.set_recognizers(ModelRc::new(VecModel::from(
        inventory
            .asr
            .iter()
            .filter(|entry| entry.package_detected && entry.verified_assets)
            .map(|entry| {
                format!(
                    "{} · {} ({})",
                    entry.id,
                    entry.name,
                    entry.installation_label()
                )
                .into()
            })
            .collect::<Vec<_>>(),
    )));
    if !ui
        .get_recognizers()
        .iter()
        .any(|entry| entry == ui.get_recognizer())
    {
        ui.set_recognizer("".into());
    }
    ui.set_devices(ModelRc::new(VecModel::from(
        devices
            .iter()
            .map(|device| format!("{} ({})", device.name, device.backend).into())
            .collect::<Vec<_>>(),
    )));
    ui.set_device_entries(ModelRc::new(VecModel::from(
        devices
            .iter()
            .map(|device| AudioDeviceEntry {
                id: device.id.clone().into(),
                caption: format!("{} ({})", device.name, device.backend).into(),
                backend: device.backend.clone().into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_device_caption(
        ui.get_device_entries()
            .iter()
            .find(|entry| entry.id == ui.get_device())
            .map(|entry| entry.caption)
            .unwrap_or_else(|| ui.get_device()),
    );
    let mut backends = devices
        .iter()
        .map(|device| device.backend.as_str())
        .collect::<Vec<_>>();
    backends.sort_unstable();
    backends.dedup();
    ui.set_audio_backends(ModelRc::new(VecModel::from(
        backends.into_iter().map(Into::into).collect::<Vec<_>>(),
    )));
    ui.set_monitored_device(if devices.is_empty() {
        "Доступные устройства не обнаружены".into()
    } else {
        "Выберите устройство ввода".into()
    });
    ui.set_draft_enabled(true);
}

pub fn preview(window: &AppWindow) {
    window.global::<SettingsUi>().set_section(5);
    let ui = window.global::<AsrUi>();
    ui.set_draft_enabled(true);
    ui.set_runtime_ready(false);
}

pub fn runtime_status(window: &AppWindow, snapshot: &RuntimeSnapshot) {
    let service = snapshot.service(AiService::Asr);
    let ui = window.global::<AsrUi>();
    ui.set_worker_failed(service.phase == RuntimePhase::Failed);
    ui.set_worker_idle(service.phase == RuntimePhase::Stopped);
    ui.set_runtime_ready(service.phase == RuntimePhase::ModelReady);
    ui.set_worker_status(super::speech_status::describe(&service).into());
    display_status(&ui);
}

pub fn session_status(window: &AppWindow, snapshot: &SpeechSessionSnapshot) {
    let ui = window.global::<AsrUi>();
    ui.set_session_failed(snapshot.phase == SpeechSessionPhase::Failed);
    ui.set_session_idle(snapshot.phase == SpeechSessionPhase::Stopped);
    ui.set_active(snapshot.phase == SpeechSessionPhase::Listening);
    let message = match snapshot.phase {
        SpeechSessionPhase::Stopped => "Микрофон остановлен".to_owned(),
        SpeechSessionPhase::Starting => "Запуск микрофона".to_owned(),
        SpeechSessionPhase::Listening => "Микрофон слушает".to_owned(),
        SpeechSessionPhase::Stopping => "Остановка микрофона".to_owned(),
        SpeechSessionPhase::Failed => format!(
            "Ошибка микрофона: {}",
            snapshot
                .last_error
                .as_deref()
                .unwrap_or("неизвестная ошибка")
        ),
    };
    ui.set_session_status(message.into());
    display_status(&ui);
}

fn display_status(ui: &AsrUi) {
    ui.set_status(if ui.get_worker_idle() && ui.get_session_idle() {
        "—".into()
    } else {
        format!("{} · {}", ui.get_session_status(), ui.get_worker_status()).into()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asr_languages_follow_original_model_schemas() {
        assert_eq!(
            asr_language_capability("google").unwrap().choices.len(),
            102
        );
        assert_eq!(
            asr_language_capability("whisper").unwrap().choices.len(),
            101
        );
        assert_eq!(language_id(&language_label("google", "en-US")), "en-US");
        assert_eq!(language_id(&language_label("whisper", "auto")), "auto");
    }

    #[test]
    fn seconds_are_converted_to_milliseconds() {
        assert_eq!(millis("0,50", "VAD").unwrap(), 500);
        assert!(millis("NaN", "VAD").is_err());
        assert!(millis("-1", "VAD").is_err());
    }
}
