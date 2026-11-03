# Issue 60: dependency spike wavekat/ORT (2026-11-03)

Проверка дельты из `issue-60-vad-dependencies.md` в отдельном worktree
`codex/spike-vad-deps` (ветка от `7ee0b63c`). Производственные `Cargo.toml`,
`Cargo.lock` и общий vendor не изменялись.

## Резолв

Дельта применена как предложено (`wavekat-vad =0.1.17`, `ort =2.0.0-rc.12`,
фичи `vad-wavekat`/`vad-silero`). Cargo.lock вырос на **30 пакетов**.

Ключевые версии: `wavekat-vad 0.1.17`, `ort 2.0.0-rc.12`, `ort-sys 2.0.0-rc.12`,
`ndarray 0.17.2`, `rubato 0.16.2`, `ureq 3.5.1`, существующий `earshot 1.2.2`.
Resolved `rust-version` не поднимает требование выше 1.89.

## Подтверждённая проблема feature unification

Наш прямой `ort` с `default-features = false` не отключает транзитивные default
фичи. Фактический граф резолва:

```
ort      api-17..api-24, copy-dylibs, default, download-binaries, load-dynamic,
         ndarray, preload-dylibs, std, tls-native, tracing
ort-sys  api-17..api-24, copy-dylibs, disable-linking, download-binaries,
         std, tls-native
```

То есть `download-binaries` и `copy-dylibs` остаются активными даже при
`default-features = false` у нашей зависимости: волкат подключает `ort` со своими
defaults. Предупреждение внешнего ревью подтверждено по факту, а не по чтению кода.

## Что всё же не скачалось

После сборки `--features vad-silero` ни в `~\.cargo`, ни в target-spike нет ни одного
`onnxruntime*.dll`. Ранний выход ort-sys при `disable-linking` действительно
предотвращает загрузку бинаря ORT. Это доказано отсутствием файла, но не доказывает
поведение при полностью заблокированной сети.

## Модель Silero

`wavekat-vad 0.1.17` в `build.rs`:

- `SILERO_MODEL_PATH` — локальный файл; при отсутствии файла build script паникует и
  ничего не скачивает;
- иначе скачивает модель с
  `https://github.com/snakers4/silero-vad/raw/v6.2.1/src/silero_vad/data/silero_vad.onnx`.

Факты прогонов:

1. Без `SILERO_MODEL_PATH` сборка скачала `silero_vad.onnx` в `OUT_DIR`
   (2 327 524 байта) — неявная загрузка реальна.
2. SHA-256 этого файла — `1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3`,
   то есть ровно наш закреплённый хэш.
3. С `SILERO_MODEL_PATH` на локальную копию сборка `--offline` прошла за 9.4 с без
   загрузки.
4. С `SILERO_MODEL_PATH` на несуществующий файл сборка падает с
   `SILERO_MODEL_PATH points to non-existent file` и ничего не скачивает.

**Провенанс уточнён.** Файл совпал с URL тега v6.2.1, но это не противоречие:
в snakers4/silero-vad модель не менялась между тегами. Git blob ONNX-файла одинаков
в v6.2.1 (commit `7e30209a`) и v6.2.3 (commit `5cd79456`):
`80c5592ef1f4c9ede3e357bbd02eb863358a6a9d`, размер 2 327 524 байта. Поэтому
закрепление на commit `5cd7945` и SHA-256 `1a153a22…8788e3` остаётся корректным.

## Сборка

- `cargo check --features vad-wavekat` — PASS, 62 с; ort в граф не попадает.
- `cargo check --features vad-silero` — PASS, 27 с (все 30 новых пакетов).
- `cargo check --features vad-silero --offline` с локальным ассетом — PASS.

## Что не доказано

- Сборка при реально отключённой сети: `cargo --offline` блокирует registry, но не
  build scripts. Аргумент сейчас — чтение `build.rs` плюс отсутствие скачанного DLL.
- Размер итогового бинаря и vendor-дельты в производственном репозитории.
- Поведение с DLL, которую поставляет сам проект: для спайка взят официальный
  ONNX Runtime 1.24.2, но он не входит в дистрибутив.

## Реальный inference (2026-11-03)

`examples/silero_spike.rs` прогоняет проверенную модель через боевой конвейер
`VerifiedSileroAsset → SileroScores → ScoreFrames → ScoreGate → SpeechSegmenter`.

- DLL: официальный ONNX Runtime 1.24.2 (Windows x64), файл 14 148 680 байт,
  версия 1.24.20260218.6.058787c, инициализация через `ort::init_from(<abs path>)`.
  DLL из Mozilla Firefox 1.30 дала `STATUS_ACCESS_VIOLATION` — чужие сборки не годятся.
- Модель: закреплённый файл, 2 327 524 байта, хэш проверен перед загрузкой.
- Клип: 51 200 сэмплов 16 кГц (1 с тишины, 1.2 с синтетического тона, 1 с тишины).
- Результат: 38 «речевых» и 62 «тихих» окон 512 сэмплов; вероятность в речи
  0.002–0.752, в тишине 0.001–0.009; 100 окон за 26.9 мс в debug-сборке;
  сегментатор опубликовал 1 сегмент длиной 11 776 сэмплов.

Это доказывает работоспособность модели, ORT и интеграции с `ScoreGate` на
синтетическом сигнале. Качество на настоящей речи не измерено: тон — не речь,
поэтому 0.752 не является оценкой качества VAD.

## Принятое решение (внешнее ревью)

- Earshot остаётся основным backend, прямой путь не меняется.
- Silero — через волкат 0.1.17; собственную реализацию Silero поверх `ort` не пишем.
  Причина: волкат уже держит LSTM-состояние и контекст модели, а `download-binaries`
  пока не создал практической проблемы благодаря `disable-linking`.
- Зависимости не переносятся в production до реального inference с локальным DLL:
  `cargo check` доказывает компиляцию, но не работоспособность модели.
- Для production нужен fail-closed вариант `build.rs`: без `SILERO_MODEL_PATH`
  сборка должна падать, а не скачивать модель. Сейчас это держится на том, что мы
  всегда задаём переменную.

## Следующий шаг

Regenerated vendor, локальный CPU DLL ORT, реальный Silero inference через
`ScoreGate`, тесты с реальными временными границами, затем корпус и A/B.
