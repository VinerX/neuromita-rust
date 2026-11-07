# #51: точные границы Python runtime support

Этот документ фиксирует ограниченный Python-срез. Владение приложением, Core
startup, CAS/stale protection и результаты Rust-проверок описываются отдельно в
[hardware-runtime.md](hardware-runtime.md). Четыре адаптера ниже не означают
поддержку всех builtin plugins или приёмку всего #51.

## Production plugins с явным runtime

| Plugin id | Backend | Device | Precision | Подтверждение перед readiness |
|---|---|---|---|---|
| `gigaam` | `torch_cpu`, `torch_cuda` | `cpu`, видимый Torch `cuda:N` | `fp32` | Реальные параметры и буферы загруженной модели; единое размещение и dtype |
| `omnivoice` | `torch_cpu`, `torch_cuda` | `cpu`, видимый Torch `cuda:N` | `fp32` | Параметры и буферы основной модели и audio tokenizer; наличие TTS methods и tokenizers |
| `gigaam_onnx` | `onnx_cpu` | `cpu` | `fp32` | Полный набор CTC/RNNT sessions, исключительно CPU provider, session I/O types и dtype выбранных ONNX artifacts |
| `google` | `remote_api` | `remote` | `fp32` как wire placeholder | Загруженный локальный SpeechRecognition adapter с Google ASR interface |

`gigaam` сохраняет базовые настройки `model`, `language` и выбор asset revision.
Для CUDA модель загружается внутри контекста выбранного ordinal, затем явно
перемещается на выбранный device в fp32. Явный режим `inference_precision=fp32`
в bundled GigaAM отключает внутренний CUDA fp16 autocast в forward; внешний infer
также отключает autocast для preprocessing, forward и decoding. Без явного
режима legacy CUDA forward сохраняет исходный fp16 autocast. Потеря режима
после explicit activation приводит к ошибке, а не переходу к legacy execution.
Гарантия этого среза — fp32 model tensors и отключённый autocast в указанном
пути infer. Глобальная политика CUDA TF32, внутренние dtype произвольных
сторонних operators и численная эквивалентность CPU/CUDA этим не проверяются.
Игнорирование device/dtype, отсутствие
параметров и смешанное размещение приводят к отказу.

`omnivoice` использует выбранную локальную revision и требует локальную папку
`audio_tokenizer`. Явная загрузка задаёт `local_files_only=True`, `load_asr=False`
и fp32. Дополнительный загруженный ASR pipeline не поддерживается. Конфликт
settings device с runtime отклоняется. Legacy initialize остаётся в базовом
VoicePlugin и не получает новую гарантию runtime.

`gigaam_onnx` не объявляет GPU/DML: существующий загрузчик добавляет CPU fallback.
Явный CPU runtime проверяет каждую сессию и отключает Python-level fallback.
ONNX graph inspection отвергает другие floating precisions, custom functions,
custom operator domains и неразбираемые tensor/type constructs. Такая модель
получает явную ошибку, а не расширенное предположение о поддержке.

`google` при initialize проверяет только локальный adapter, без сетевого запроса.
Это не доказательство доступности удалённого сервиса. Remote не создаёт локальную
hardware device record, а локальный `execute` для него отклоняется. Rust-владелец
должен пропускать локальные inventory/execution checks для remote и требовать
plugin acknowledgement.

## Optional SDK contract и совместимость

Wire profile сохраняет поля `{backend, device, precision}`. Для явного выбора
plugin реализует все три метода:

```python
runtime_capabilities()  # [{backend, devices: [...], precisions: [...]}]
initialize_runtime(settings, runtime)  # result с model_ready=True
runtime_status()  # фактические {backend, device, precision} загруженного runtime
```

Host проверяет объявленную комбинацию, затем фактический профиль после загрузки.
Только точное совпадение и `model_ready is True` позволяют опубликовать active
service slot. `initialize.result.runtime` копируется из подтверждения plugin.
Ошибка инициализации/подтверждения вызывает unload без публикации active slot.
Это проверка при активации, не непрерывный мониторинг состояния модели.

Без runtime сохраняется прежний вызов `initialize(settings)`. Наличие legacy
plugin в discovery и доступный hardware backend сами по себе не дают explicit
runtime support. Каждый активируемый с runtime plugin должен подтвердить профиль.

Silero/RVC и остальные builtin plugins без этих hooks отклоняют явный runtime.
Объявленный hardware inventory для ONNX CUDA/DirectML или CTranslate2 не меняет
эту границу. Все четыре адаптера выше отклоняют fp16.

CTranslate2 execution probe в SDK предназначен для Translator и требует
`model_path` и source `tokens`. Он не является пробой Whisper ASR. Вызов только
с profile либо неподдержанная модель завершаются ошибкой; автоматической подмены
на Translator, CPU или другой precision нет. Whisper explicit activation этим
срезом не реализована.

## Проверки и воспроизведение

Полный набор: **81 unittest passed** на CPython 3.12.7 с существующим offline
NumPy 1.26.0. Импорт NumPy из указанного cache и арифметика массивов проверены
отдельно. Команда из корня worktree в PowerShell:

```powershell
$env:PYTHONPATH = 'python;C:/Users/Dmitry/AppData/Local/uv/cache/archive-v0/0eHePmxTSg4iRm6S'
& C:/Users/Dmitry/AppData/Local/Programs/Python/Python312/python.exe -m unittest discover -s python/tests
```

Тесты используют настоящие builtin adapter classes и host с контролируемыми
ML modules/models. Проверены CPU/CUDA placement acknowledgement, неправильные
device/dtype/provider, missing assets/components, cleanup, legacy compatibility,
OmniVoice initialize-to-WAV и remote initialize без сетевого вызова. Тест GigaAM
исполняет настоящий bundled forward и ASR infer с контролируемым Torch: legacy
CUDA использует autocast, explicit fp32 сохраняет отключённый autocast в
preprocessor, encoder и decoder и восстанавливает внешний контекст. Отдельный
host subprocess с `python -S` проверяет отсутствие ложной hardware readiness без
ML packages и работу legacy mock TTS.

Реальные веса GigaAM/OmniVoice, вычисления на физическом GPU и удалённый Google
API не запускались. Новые зависимости не устанавливались и не скачивались.
Эти проверки не заменяют Rust validation, реальную модельную приёмку и оценку
готовности всего #51.
