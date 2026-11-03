# #60 Native VAD: запрос внешнего архитектурного ревью

Эта ветка содержит только документ для внешней консультации. Кода, зависимостей и
ассетов здесь нет. Ветка реализации: `codex/issue-60-native-vad-current`
(HEAD `7ee0b63c`, первый проверенный инкремент `8d453de9`).

Репозиторий: https://github.com/Atm4x/neuromita-rust
Issue: https://github.com/Atm4x/neuromita-rust/issues/60
База: `upstream/main` `15710db73adea787e9ddb23a3ea2550ded96d260` (2026-11-03).
Платформа: Windows 10/11, стабильный Rust 1.89, Cargo offline + vendored sources.

Нужен разбор по существу: где наша схема неверна, что упростить, какой минимальный
набор доказательств честно закрывает issue, и какие риски мы не видим.

## Что требует issue #60

Сравнить реальные wavekat Earshot и опциональный Silero на одном ограниченном корпусе
записей, закрепить единственного Rust-владельца сегментации и входного гейта. Python
остаётся только ASR-провайдером, не вторым сегментатором. Без второго AI-супервизора,
универсального RPC-реестра, неявной установки моделей и записи с микрофона без явного
действия пользователя.

## Что уже реализовано и проверено

Первый инкремент `8d453de9` (8 файлов) трогает только домен, без Cargo/vendor изменений:

- `src/audio/session.rs` — stream/generation tickets, utterance UUID, immutable
  `SpeechRoute`. Reset и новое нажатие PTT инвалидируют активную и очередь сегментов.
  Устаревшая работа проверяется перед ASR, после ASR и на синхронном admission.
  `ChatReplyTarget` привязан к исходному `Arc<ChatApiState>`, поэтому старый Core не
  может писать в перезапущенный. Drop после паники в admission не паникует повторно.
- `src/audio/segment.rs` — onset-кадры сохраняются при коротком или нулевом pre-roll;
  `start_voice_frames > max_segment_frames` отклоняется; непрерывная речь на границе
  максимальной длины не теряет и не дублирует кадры.
- `src/audio/score.rs` — `VadScoreProvider`, `DirectEarshotScores`, сборка кадров
  256/512, sample offsets, честный хвост без padding. Ошибка провайдера/вероятности/
  offset переводит поток в poisoned + `ResetRequired`; частичный score-вектор не
  отдаётся. `VerifiedSileroAsset` проверяет размер и SHA-256.

Существующее до нас: `src/audio/vad.rs` с прямым Earshot 1.2.2 (PCM16 mono 16 kHz,
256 samples), `VoiceGate`/prebuffer/hangover/мин-макс длина в `segment.rs`,
FFmpeg/ffplay capture adapter в `device.rs`, `examples/vad_smoke.rs` (тишина/синус,
не качество речи), `tests/audio_pipeline.rs` (fake capture/gate/transcriber/playback).

Проверки: default suite 438 passed / 0 failed / 2 ignored; no-default 377 / 0 / 2;
focused audio 21 + pipeline 1; Clippy `-D warnings` для обоих наборов; fmt и
`git diff --check` чистые. Всё `--locked --offline` с vendor override.

Не проверено и не заявляется: волкат, Silero inference, реальные RU/EN записи,
микрофон, hotplug, GUI, GPU.

## Предлагаемая дельта зависимостей (ещё не применена)

```toml
[features]
# существующий default остаётся ["sqlite-system"]
vad-wavekat = ["dep:wavekat-vad"]
vad-silero = ["vad-wavekat", "wavekat-vad/silero", "dep:ort"]

[dependencies]
wavekat-vad = { version = "=0.1.17", optional = true, default-features = false, features = ["earshot"] }
ort = { version = "=2.0.0-rc.12", optional = true, default-features = false, features = ["std", "ndarray", "load-dynamic", "api-24"] }
```

Факты по волкат 0.1.17: Apache-2.0, checksum
`2294cef58ceef4ff323bb282ea23be7f5154077419ec453bc12427a6e0c79c73`, архив 91 522
байта, без declared rust-version. Неопциональные зависимости: rubato ^0.16 и thiserror
^2; Earshot ^1.2.1 (в нашем lock уже 1.2.2). Для Silero волкат дополнительно тянет
ndarray ^0.17, ort ^2.0.0-rc.12 (со своими default features) и build-time ureq ^3.

Проблема, которую мы решаем: upstream `build.rs` волката по умолчанию скачивает модель
Silero (v6.2.1) и бинарь ORT. Мы хотим offline/vendored сборку без неявных загрузок:
`load-dynamic` включает `disable-linking`, из-за чего build.rs ort-sys выходит до
разрешения/скачивания бинаря; модель форсируется через `SILERO_MODEL_PATH` на
проверенный локальный файл.

Закреплённый ассет: snakers4/silero-vad commit
`5cd7945676eb32225748052e2e6a0580e4686a08`, `src/silero_vad/data/silero_vad.onnx`,
v6.2.3, MIT, 2 327 524 байта, SHA-256
`1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3`.
Constructor принимает уже проверенные байты; `SileroVad::new` и любые default-пути не
используются. ORT инициализируется из абсолютного локального DLL; CPU only.

## План A/B

- Речь: google/fleurs, CC BY 4.0, 6 коротких RU + 6 EN клипов; транскрипты есть.
- Шум: ESC-10, CC BY 3.0, 6 записей; отсутствие речи подтверждается прослушиванием.
- Каждый фикстур: стабильный id, RU/EN/noise, URL и ревизия/клип, автор/лицензия,
  исходные SHA-256 и размер, команда конвертации, PCM16 mono 16 kHz WAV SHA-256/размер,
  роль calibration vs held-out.
- Границы речи размечаются вручную в целых sample offsets с метаданными аннотатора;
  транскрипт границей не считается. Без разметки метрики границ явно подавляются.
- Порог замораживается до прогона held-out; на отчётной выборке не подбирается.
- Метрики: frame precision/recall/F1, boundary loss, false-positive duration в шуме,
  endpoint latency, p50/p95 времени обработки. Апстрим-бенчмарки не выдаются за наши.

## Известные незакрытые места

- EOF drain без дедлайна; rebind/hotplug устройства; speaking lease вместо локального
  playback suppression; partial PCM до гейта (существующая проблема, не наша регрессия);
  паритет с Python gate/session, включая radio/PTT/permission.
- Реальная микрофонная приёмка отложена до конца и требует присутствия пользователя.
- Threading admission: reset не отменяет admission, уже прошедший до захвата
  синхронного лока; это осознанное ограничение.

## Вопросы

1. **Нужен ли волкат вообще.** У нас уже есть прямой Earshot и собственный слой
   кадров 256/512. Даёт ли `wavekat-vad` реальную ценность, кроме унификации с Silero,
   или разумнее взять Earshot напрямую и подключить `ort` + Silero своим кодом на уже
   существующем фрейминге? Что бы вы выбрали и почему?
2. **Offline + ORT.** Корректен ли расчёт, что `load-dynamic` + `disable-linking` +
   `SILERO_MODEL_PATH` даёт vendored сборку без скачиваний? Известны ли подводные камни
   ort 2.0.0-rc.12 на Windows, с `cargo vendor`, feature unification и Rust 1.89?
   Как правильно провалидировать это до коммита vendor (минимальный эксперимент)?
3. **Feature matrix.** Правильно ли `vad-silero` подразумевает `vad-wavekat`, и какой
   набор CI-конфигураций вы считаете обязательным (default, no-default, wavekat,
   silero), чтобы не раздувать время сборки?
4. **Достаточность корпуса.** 12 речевых клипов + 6 шумов — это вообще достаточно,
   чтобы закрыть issue и что-то утверждать про качество? Если нет, какой минимальный
   размер выборки и протокол вы бы потребовали? Как корректно отчитываться при таком N?
5. **Порог.** Фиксированные фабричные пороги или калибровка на устройстве? Как
   отчитываться о порогах и неопределённости, чтобы это не выглядело подгонкой?
6. **Границы issue.** Какие из незакрытых пунктов (drain deadline, hotplug, speaking
   lease, partial PCM, Python parity, реальный микрофон) обязательны для закрытия #60,
   а какие — законные follow-up? В каком порядке их делать?
7. **Критика дизайна.** Есть ли скрытые баги в stream generation tickets, once-only
   admission, `ResetRequired` poison, 256/512 фрейминге и проверке хэша ассета?
   Что выглядит хрупким при перезапуске Core, смене устройства и остановке процесса?
8. **Минимальный честный результат.** Опишите, какой набор артефактов и метрик вы бы
   приняли как «issue #60 закрыт», не переоценивая доказательства.
