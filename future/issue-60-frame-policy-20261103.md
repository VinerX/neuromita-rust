# Issue 60: policy durations in samples and wide score providers

Второй инкремент после `8d453de9`. Закрывает пункт внешнего ревью «256 против 512
samples»: политика сегментации больше не выражается в кадрах Earshot.

## Что изменено

- `SegmentConfig` хранит `pre_roll_samples`, `start_voice_samples`,
  `end_silence_samples`, `min_segment_samples`, `max_segment_samples` вместо
  `*_frames`. Один и тот же конфиг означает одно и то же время при любом размере
  кадра провайдера. Предел сегмента вынесен в `MAX_SEGMENT_SAMPLES` (10 с при 16 кГц).
- `VoiceGate::inspect` теперь возвращает `Result<VoiceDecision, ScoreError>`, а
  `SpeechSegmenter::process` — `Result<Option<SpeechSegment>, SegmentError>`.
  Ошибка провайдера доходит до сессии как `SegmentError::Vad`, а не теряется в
  безошибочном `VoiceDecision`.
- Добавлен `ScoreGate<P: VadScoreProvider>` — мост между провайдером на 256/512
  сэмплов и сеткой захвата 256 сэмплов. Каждый скор покрывает свой участок сэмплов,
  поэтому длительности остаются в реальном времени.

## Осознанное ограничение

Провайдер с окном 512 сэмплов не может оценить первый кадр окна до прихода второго.
Поэтому `ScoreGate` отдаёт последний завершённый скор, и решение отстаёт не более чем
на один кадр захвата (16 мс). Это видимая часть endpoint latency, а не удвоение
политики: 3 кадра onset остаются 48 мс, 28 кадров тишины — 448 мс.

Задержка учтена в самом сегментаторе: `VoiceGate::decision_lag_frames` сообщает,
сколько кадров провайдер отдаёт решение позже, и ровно на столько расширяется
буфер pre-roll. Без этого при `pre_roll_samples = 0` терялся первый речевой кадр,
попавший в первую половину 512-сэмплового окна. Это закрыто тестом
`wide_provider_onset_survives_disabled_pre_roll`.

Ошибка провайдера больше не выходит из `run()` через `?`: она превращается в
`break Err(...)` основного цикла, поэтому worker отменяется и сессия проходит
штатный путь завершения. Тест:
`provider_failure_ends_the_session_through_the_normal_shutdown_path`.

## Проверки

- `cargo test --lib audio::` — 24 passed, 0 failed (включая новые
  `wide_score_provider_keeps_policy_durations_in_real_time`,
  `wide_provider_onset_survives_disabled_pre_roll` и
  `provider_failure_ends_the_session_through_the_normal_shutdown_path`).
- `cargo test --test audio_pipeline` — 1 passed.
- Полный default suite: **443 passed, 0 failed, 2 ignored** (46 test-бинарников).
- `cargo clippy --all-targets -- -D warnings` — PASS.
- `cargo fmt --all -- --check` — PASS.

Все запуски: `--locked --offline`, vendor override
`C:/Development/NeuromitaRust/.worktrees/issue-51-current/vendor`.

## Что не сделано

Реальные провайдеры wavekat Earshot/Silero ещё не подключены: это отдельный
dependency-инкремент (см. `issue-60-vad-dependencies.md`). A/B на записях, корпус,
микрофон и hotplug остаются открытыми.
