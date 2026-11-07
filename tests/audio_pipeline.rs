use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use neuromita::{
    ai_runtime::speech::Transcript,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    audio::{
        device::{AudioCapture, AudioPlayback, DeviceFuture},
        segment::{SegmentConfig, SpeechMode, SpeechSegment, SpeechSegmenter, VoiceGate},
        session::{
            ChatReplyTarget, SpeechConversation, SpeechFuture, SpeechInputHandler,
            SpeechTranscriber,
        },
        vad::{FRAME_SAMPLES, MonoPcm16Frame, VoiceDecision},
    },
    character::CharacterId,
    generation::{
        GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::RequestSource,
    settings::{AppSettings, SettingsSource},
    voice::{AudioArtifact, AudioFormat, VoiceFuture, VoiceOptions, VoiceProvider},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Provider;
impl GenerationProvider for Provider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(
        &self,
        _context: GenerationInput,
        _template: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async { Ok(StructuredResponse::spoken("Answer from Mita")) })
    }
}

struct Voice;
impl VoiceProvider for Voice {
    fn synthesize(&self, _text: Arc<str>, _options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async {
            Ok(AudioArtifact {
                format: AudioFormat::Wave,
                bytes: Arc::from(
                    SpeechSegment {
                        samples: vec![500; 256],
                    }
                    .as_wav()
                    .unwrap(),
                ),
            })
        })
    }
}

struct Capture(Vec<MonoPcm16Frame>);
impl AudioCapture for Capture {
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
        Box::pin(async {
            Ok(if self.0.is_empty() {
                None
            } else {
                Some(self.0.remove(0))
            })
        })
    }
}

struct Gate;
impl VoiceGate for Gate {
    fn inspect(&mut self, frame: &MonoPcm16Frame) -> VoiceDecision {
        VoiceDecision {
            probability: f32::from(frame[0] != 0),
            speech: frame[0] != 0,
        }
    }
    fn reset(&mut self) {}
}

struct Transcriber;
impl SpeechTranscriber for Transcriber {
    fn transcribe<'a>(&'a self, wav: &'a [u8]) -> SpeechFuture<'a, Transcript> {
        Box::pin(async move {
            assert_eq!(&wav[..4], b"RIFF");
            Ok(Transcript {
                text: "Speech from player".into(),
                language: Some("en".into()),
            })
        })
    }
}

struct Playback(Arc<Mutex<Vec<Vec<u8>>>>);
impl AudioPlayback for Playback {
    fn play<'a>(&'a self, audio: AudioArtifact, _stop: CancellationToken) -> DeviceFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push(audio.bytes.to_vec());
            Ok(())
        })
    }
}

#[tokio::test]
async fn microphone_vad_asr_player_message_generation_tts_playback() {
    let limits = RuntimeLimits {
        request_queue: NonZeroUsize::new(8).unwrap(),
        discovery_buffer: NonZeroUsize::new(32).unwrap(),
        character_commands: NonZeroUsize::new(8).unwrap(),
        presenter_updates: NonZeroUsize::new(32).unwrap(),
        retained_finished: NonZeroUsize::new(8).unwrap(),
    };
    let template = GeminiTemplate::google(
        "model-test",
        "not-used",
        GenerationSettings::new(0.5, 64, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let mut config = ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new([Arc::new(Provider) as Arc<dyn GenerationProvider>]).unwrap(),
        limits,
        "127.0.0.1:0".parse().unwrap(),
    );
    config.voice = Arc::new(Voice);
    let mut settings = AppSettings::initial_profile();
    settings.voice.in_launcher = true;
    config.settings.source = SettingsSource::initial(settings);
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let handler = SpeechInputHandler::new(
        Arc::new(Transcriber),
        Arc::new(ChatReplyTarget {
            character: CharacterId::Crazy,
            source: RequestSource::Presenter,
        }) as Arc<dyn SpeechConversation>,
        Arc::new(Playback(Arc::clone(&recorded))),
    );
    let config = SegmentConfig {
        mode: SpeechMode::Automatic,
        pre_roll_frames: 3,
        start_voice_frames: 2,
        end_silence_frames: 3,
        min_segment_frames: 2,
        max_segment_frames: 32,
    };
    let frames = [
        vec![[0; FRAME_SAMPLES]; 4],
        vec![[200; FRAME_SAMPLES]; 5],
        vec![[0; FRAME_SAMPLES]; 4],
    ]
    .concat();
    let (_tx, rx) = mpsc::channel(2);
    handler
        .run(
            Capture(frames),
            SpeechSegmenter::new(Gate, config).unwrap(),
            rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    {
        let audio = recorded.lock().unwrap();
        assert_eq!(audio.len(), 1);
        assert_eq!(&audio[0][..4], b"RIFF");
    }
    let snapshot = app
        .presenter()
        .conversation
        .snapshot(CharacterId::Crazy)
        .await
        .unwrap();
    assert!(
        snapshot
            .messages
            .iter()
            .any(|m| m.content == "Speech from player")
    );
    assert!(
        snapshot
            .messages
            .iter()
            .any(|m| m.content == "Answer from Mita")
    );
    app.shutdown().await.unwrap();
}
