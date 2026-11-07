use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationProvider, PresetId, ResponseGenerator, gemini::GeminiProvider,
        python_import::PythonPresetImport,
    },
    request::{RequestOutcome, RequestState},
};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires explicit local preset/template paths and preset ID; makes one paid/provider request"]
async fn live_presenter_to_gemini() {
    let path =
        std::env::var("NEUROMITA_LIVE_GEMINI_PRESET_FILE").expect("explicit fixture path required");
    let id = std::env::var("NEUROMITA_LIVE_GEMINI_PRESET_ID")
        .expect("explicit fixture preset ID required");
    let template_path = std::env::var("NEUROMITA_LIVE_GEMINI_TEMPLATE_FILE")
        .expect("explicit template file required");
    let plan = PythonPresetImport::parse(
        &std::fs::read(path).expect("fixture cannot be read"),
        &std::fs::read(template_path).expect("templates cannot be read"),
    )
    .expect("import failed");
    let preset_id = PresetId(id.parse().expect("numeric preset ID required"));
    let model = match &plan
        .records
        .get(&preset_id)
        .expect("requested preset was not imported")
        .definition
    {
        neuromita::generation::PresetDefinition::Gemini(config) => config.model.clone(),
        _ => panic!("explicit Gemini preset required"),
    };
    let directory = std::env::temp_dir().join(format!("live-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("presets.json"),
        plan.catalog_bytes().unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.join("preset-credentials.json"),
        plan.credential_bytes().unwrap(),
    )
    .unwrap();
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let limits = RuntimeLimits {
        request_queue: NonZeroUsize::new(2).unwrap(),
        discovery_buffer: NonZeroUsize::new(8).unwrap(),
        character_commands: NonZeroUsize::new(8).unwrap(),
        presenter_updates: NonZeroUsize::new(8).unwrap(),
        retained_finished: NonZeroUsize::new(4).unwrap(),
    };
    let config = ApplicationConfig::persistent_unconfigured(
        ResponseGenerator::new([provider]).unwrap(),
        limits,
        "127.0.0.1:0".parse().unwrap(),
        &directory,
    )
    .unwrap();
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.presenter()
        .settings
        .presets
        .active
        .set(preset_id)
        .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(
            CharacterId::Crazy,
            "Проверка соединения. Ответь одним словом: готово".into(),
        )
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(100), request.wait()).await;
    app.shutdown().await.unwrap();
    let snapshot = result.expect("Gemini request timed out").unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    match &snapshot.state {
        RequestState::Finished(RequestOutcome::Success(response)) => {
            assert!(!response.text.trim().is_empty());
            println!(
                "Gemini live check succeeded: model={model}, request={}, answer={}",
                request.id(),
                response.text
            );
        }
        state => panic!("Gemini live request did not succeed: {state:?}"),
    }
}
