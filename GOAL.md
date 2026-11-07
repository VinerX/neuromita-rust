# NeuroMita Rust — текущая цель

Интеграция 2026-11-03: объединены актуальные GUI/localization, AI runtime, memory/semantic retrieval и provisional-request ветки. [Состав и проверки](future/integration-20261103.md). Ниже сохранены исторические этапы и незакрытые продуктовые цели; ранняя фраза «semantic retrieval ещё не реализован» не описывает текущую кодовую базу.

Обновление после интеграции PR #55–58: все четыре merged в main, вместе с исправлениями ядра и системой миграций. Semantic retrieval реализован как отдельный opt-in срез: Gemini 001/2 embedding HTTP, bounded derived cache, точное cosine-ранжирование и синтетические fixture-тесты. Живое качество модели и тарифная стоимость не подтверждены; см. [semantic-retrieval](future/semantic-retrieval.md).

**Актуальный срез:** 2026-11-02. **Базовая ревизия перед текущим проходом:** `8994f8d` (`codex/typed-long-term-memory-20261102`); текущий проход — ограниченный отбор воспоминаний и точная дедупликация. Этот документ — рабочий план, а не утверждение о полной Python parity.

## Новая согласованная цель: отмена ответа во время TTS для всех клиентов

Уточнение пользователя от 2026-11-02 заменяет целевой порядок принятия до TTS. Ревизия `ae0f9ca` использовала старый порядок; нижний перечень реализованного описывает именно его. Ядро теперь реализует отмену во время TTS; прежний порядок ниже относится к ae0f9ca. Визуальный frontend ещё не реализован.

- [x] Единый контракт генерационного запроса для Sandbox и внешних клиентов: RequestId, очередь, наблюдение этапов, предварительный готовый текст, отмена и конечный исход. Sandbox — встроенный клиент через presenter, без обязательного сетевого транспорта; административная панель остаётся отдельной поверхностью. Токеновый streaming не реализован, см. #17.
- [ ] Показывать стриминг/готовый LLM-текст как предварительное сообщение. Во время подготовки озвучки — видимый индикатор по обводке сообщения; оно ещё не считается окончательным ответом.
- [ ] Разрешать отмену всего запроса во время TTS: остановить синтез, убрать предварительное сообщение с понятным визуальным завершением (например, распадом на частицы), не принимать сообщение и предложенные изменения состояния/памяти и не исполнять клиентские команды.
- [x] Перенести короткую атомарную границу принятия после подготовки озвучки, если она требуется. После победы acceptance отмена результата возвращает TooLate. Ошибка TTS с рабочим текстом и пользовательская отмена — разные исходы.
- [ ] Разделить принятие сообщения ядром, доставку клиенту и фактическое исполнение команд. Готовность аудио не подтверждает доставку; отправка команды не подтверждает её исполнение.
- [x] Проверить одинаковую семантику отмены в Sandbox и сетевом клиенте, гонку cancel с завершением TTS, отсутствие эффектов/команд отменённого ответа и сохранность уже принятого результата при ошибке доставки.

Подробный целевой контракт и граница с текущей реализацией: [предварительный ответ, TTS и отмена](future/request-delivery-and-cancellation.md).

## Текущий приоритет: самостоятельный Generation Core (без Unity)

1. [x] Каноническая SessionTimeline: общий `Event` / `Message`, MitaView и прямое знание наблюдённых событий, SQLite, `Reply` на Message / `React` на любой известный Event / `Initiate` без цели.
2. [x] Отдельная публикация Message, единая подготовка LLM-истории, типизированные настройки персонажей и presenter bindings.
3. [x] `ChatApi` без пресета; назначение `Active | Specific(PresetId)` по персонажу и операции; **единственное разрешение и материализация пресета перед генерацией**.
4. [x] **#16, базовый persistent catalog**: независимые определения, CRUD, постоянное хранение без API-ключей в JSON, presenter и проверка после restart. Полный пользовательский редактор/авторизация/другие протоколы остаются в #16.
5. [~] **#19: StructuredResponse v3**: типизированный DTO, Gemini JSON Schema (текст, эмоции, три delta-поля состояния), строгая проверка; сегменты сохраняются как JSON одного Message. Первое применяемое действие оформлено как `ResponseEffect::ChangeCharacterState` → `AppliedEffect::CharacterStateChanged`: validation → `EffectPhase::Accepting` → guarded atomic append Message + CharacterStateChanged → `EffectPhase::Applied` → TTS. `AcceptedResponse` содержит материализованные эффекты и производное состояние. Отмена после начала принятия — TooLate, сбой TTS не откатывает коммит. Рабочее состояние теперь поддерживает `ResponseEffect::ReplaceWorkingState` → `AppliedEffect::CharacterWorkingStateReplaced`: bounded snapshot атомарно пишется с ответом, воспроизводится по Timeline и явно очищается пустым объектом или CharacterContextReset. Базовые долгосрочные воспоминания теперь поддерживаются: `ResponseEffect::MutateMemory` разрешается в атомарные `AppliedEffect::CharacterMemoryChanged`, с устойчивыми UUID, приоритетом, Add/Update/Delete/Merge и исторической проекцией. Выбор для LLM-контекста ограничен 12 записями/6000 Unicode-символами, с детерминированным lexical ranking. Добавлена нормализация точных дубликатов Add без создания новых ID и запрет Update/Merge exact collisions. Полная Python memory parity (RAG/embeddings/islands/TTL/утилиты) и инструменты/игровые действия остаются открытыми.
6. [ ] #18: prompt sets / переменные / DSL; #16: transforms, альтернативные протоколы; #17: streaming/retry.
7. [ ] Функциональная проверка end-to-end генерации, настроек, сохранения и восстановления без Unity. Unity C# и выбор игровых свидетелей сейчас вне scope.

## Границы и инварианты

- `SessionTimeline` — единственный источник событий, но **не** настроек, пресетов или секретов. Рассказ Y о событии X даёт слушателю знание Y, но не X.
- `Character` не владеет назначениями пресетов. `AppSettings.presets.active` содержит только активный ID; `AppSettings.characters` — назначения по операциям; **определения пресетов принадлежат отдельному каталогу**.
- Очередь хранит команду, а не выбранную модель. После выхода из очереди исполнитель ровно один раз материализует `GenerationPreset` и передаёт его в `GenerationRequest::new`. Уже выполняемый запрос не перечитывает каталог.
- `SettingsApi` предоставляет типизированные `snapshot/edit`; `SettingsPresenter` — свойства `get/set/observe`. Не вводить универсальный строковый путь.
- Ошибки конфигурации явные; нельзя молча подменять отсутствующий пользовательский PresetId активным. Секреты не сохраняются в файле с определениями пресетов.
- Не объявлять полную готовность на основании наличия структур или единичных happy-path тестов. SQLite/без SQLite, Rustfmt и Clippy проверять после изменений.

Техническое описание: [src/generation/PRESETS.md](src/generation/PRESETS.md). Исторический подробный план Timeline: [src/timeline/GOAL.md](src/timeline/GOAL.md). Бэклог: [#16](https://github.com/Atm4x/neuromita-rust/issues/16), [#19](https://github.com/Atm4x/neuromita-rust/issues/19).

### Реализация текущего прохода: каталог (частично)

- [x] Независимые типизированные `PresetRecord` / Gemini definition и стабильные IDs; CRUD + отдельно подключаемое JSON-хранилище без секретов.
- [x] `PresetApi` как отдельный API редактирования каталога, без зависимости от `ChatApi` и `Character`.
- [x] Включить постоянный каталог в обычный CLI bootstrap и предоставить CRUD/observe через presenter; проверить сохранение назначений и определений при перезапуске.
- [ ] Настоящий графический редактор пресетов, безопасное хранение/ввод credentials помимо env refs, модельный discovery и другие протоколы. Runtime-only bootstrap ID 0 сохраняется; обычный CLI восстанавливает Timeline из timeline.sqlite3 в data-dir.

Срез #19: [src/generation/STRUCTURED_RESPONSE.md](src/generation/STRUCTURED_RESPONSE.md). Отбор памяти: [src/generation/MEMORY_RETRIEVAL.md](src/generation/MEMORY_RETRIEVAL.md).

### Следующий приоритет после текущего среза

- [x] Типизированная замена рабочего когнитивного контекста (`working_state`) без второго mutable store, с общим acceptance и контекстом следующей генерации.
- [x] Базовая долгосрочная память: стабильные UUID, приоритет, Add/Update/Delete/Merge, атомарные эффекты, исторический branch-prefix read model и контекст следующей генерации. `CharacterContextReset` очищает knowledge/working state, но оставляет личные длительные воспоминания; полное забывание — явный Delete.
- [x] Ограниченное лексическое извлечение памяти для контекста (не семантический RAG), приоритеты + устойчивый tie-break, точная дедупликация Add и atomic collision rejection при Update/Merge. Полная проекция Timeline не обрезается.
- [ ] Долговременная память, расширенная parity: Python numeric IDs, entities/tags, island upserts, RAG/vector lookup, TTL/auto-forget, **semantic** duplicate policies и UI CRUD; сравнить lifecycle/reset с реальными требованиями приложения.
- [ ] Crash recovery фазы Accepting и подтверждение outcome для уже принятого RequestId; полные executor contracts для внешних действий.

## Исправления архитектурного аудита

См. [учёт изменений](future/architecture-fixes-20261102.md): общая валидация фактов/пакетов, закрытый TurnProposal, правила Character/Memory, чтение одного префикса без нового TimelineReadSnapshot, bundled SQLite и восстановление памяти при обычном запуске. Отмена TTS через presenter и TCP реализована; frontend-индикатор и анимация остаются планом.

## Architectural Goal: Python AI Plugin Runtime (2026-11-03)

**Статус: проектирование, без реализации; не заменяет текущую цель Generation/Request.** Детальная спецификация: [AI Python Plugin Runtime](future/ai-python-plugin-runtime-architecture.md). Источник поведения: Python NeuroMita 5f3d061; архитектурные ограничения: future/root-primitives-and-client-contexts.md.

**Цель:** динамический AI Hub из Python-плагинов (builtin/community) без hardcoded моделей в Rust. Rust владеет OS-процессами, IPC, выбором активных моделей, состоянием установок и пользовательскими аудиосессиями. Python SDK описывает discovery, настройки, модельные зависимости, установочный план, asset probes и inference. Новая система не вводит EventBus, capabilities/service locator или второй процессный Supervisor внутри Python.

- [ ] **#59:** standalone uv bootstrap → pinned managed Python → versioned Python SDK; статический manifest + изолированный dynamic discovery, typed settings/plan/verify/infer contract, недоверенные install hooks с explicit approval.
- [ ] **#42:** versioned backend PyTorch/ONNX layers и model-specific overlays; staged install/verify/activate/rollback, реальные проверки imports/version/asset hashes и explicit readiness.
- [ ] **#24:** один Rust AiSupervisor (Lifecycle), владелец shared/split workers; один выбранный TTS и ASR одновременно, request correlation, generation ID, heartbeat, cancellation, restart. Не создавать per-model workers без подтверждённой потребности.
- [ ] **#60 / #25:** обособить Rust speech capture/PTT/VAD/segmentation от Python ASR; протестировать wavekat-vad Earshot и optional Silero ONNX. Native VAD работает без Python bootstrap; перенос физических микрофонов только после parity-tests.
- [ ] **#61 / #26:** отделить Python scripts, packages, weights и voice profiles; plugin-defined speaker lists и per-character RVC pitch без hardcoded CrazyMita mappings. .pth/.onnx/.index/.wav/.txt валидировать по ролям выбранного backend.
- [ ] **#37 / #51:** AI Hub отражает discovered/installed/environment-ready/worker-ready/model-ready/failed, показывает plugin-provided settings и причины ошибки; никакой фиктивной готовности.

**Минимальная приёмка:** два добавленных без сборки Rust mock-плагина (плюс невалидный), выбор/смена TTS без переподнятия ASR в split, install failure с rollback, missing/incorrect voice assets, unknown speaker/pitch validation, stale worker result и VAD mock PCM segment tests. До этих проверок не объявлять AI Runtime завершённым. Текущий VoiceProvider и ResponseGenerator не подменяются общим AI service.


### Реализация AI Runtime: первый сквозной срез (2026-11-03)

- [x] Базовый Python SDK API v1, обнаружение из plugin.json, mock TTS/ASR, host JSON-lines IPC.
- [x] Rust-owned Supervisor Shared/Split, version handshake, correlations, bounded frames и отдельные поколения worker; `PythonVoiceProvider` подключает существующий voice trait.
- [x] Contract tests в Python и Rust, Rust 1.98.1 full no-default suite + Clippy.
- [ ] Python bootstrap через uv, установка и активация environments, production discovery, assets, VAD, клиенты и интеграция в ApplicationLifecycle. Архитектурные issues #24/#42/#59/#60/#61 остаются открытыми.

## AI Runtime Implementation — staged bootstrap and overlays (2026-11-03)

**Первая публикация:** Python SDK, mock TTS/ASR, типизированный host IPC и Rust-owned Supervisor — коммиты `c4b5748`, `4458119` (предшествующий этап).

**Текущая публикация:** отдельные `src/ai_runtime/bootstrap.rs` и `src/ai_runtime/overlay.rs`. Это инфраструктурные компоненты, **не** завершённый сквозной installer.

- [x] Managed CPython bootstrap из **уже предоставленного uv binary**: exact major.minor.patch, `uv python install --no-bin --install-dir`, `uv python find --managed-python --no-python-downloads`, отдельные AIData/cache, проверка абсолютного managed path, повторный запуск без повторной установки, explicit errors.
- [x] Immutable package overlay из **предварительно разрешённого lock**: `uv pip install --target` с `--no-deps`, staging, manifest, idempotence/conflict, очистка staging при ошибке, запрет установки torch/onnxruntime в модельный overlay.
- [x] Локальные Rust tests: 8 AI Runtime tests PASS, full `--no-default-features` PASS, `cargo clippy --no-default-features -D warnings` PASS; полный default-feature suite PASS при последовательном выполнении (параллельный launcher test один раз дал Linux ETXTBSY и отдельно прошёл).
- [ ] Получение/верификация самого uv с релиза или доверенного зеркала, SHA-256/архивы и конкретная pin-policy.
- [ ] Resolver/lock step, verifiable backend layers, asset download/probe/hooks, plugin approval и atomic activation.
- [ ] Стыковка bootstrap/install с `AiSupervisor` и AI Hub, shared/split environment composition.
- [ ] Реальный скачанный Python/uv integration test и Windows validation (сейчас mock uv; offline tests).
- [ ] Контрактная сверка опубликованного Git tree в CI; локальные тесты выполнены на исходниках рабочей копии.

Все статусы readiness различать: discovered / installed / environment-ready / worker-ready / model-ready. Не считать наличие manifest доказательством импортируемости модулей и работоспособности модели.


### AI Runtime implementation continuation — 2026-11-03

- [x] Добавлен `UvInstaller`: отдельный pinned distribution descriptor (release version, HTTPS/file URL, SHA-256, archive format), ограниченный download в staging, verified tar/ZIP extraction, запуск `uv --version`, atomic publish и receipt with executable digest; no shell installer, no PATH mutation.
- [x] Общая immutable installation infrastructure `ai_runtime::layer` для model overlays и backend layers; shared uv invocation, staging и revisions. Overlay запрещает torch/onnxruntime distributions; backend validates selected runtime family and exclusive ONNX variants.
- [x] Contract tests: install/reuse/tampered uv, wrong archive checksum, backend revision conflict/family, previous overlay regressions; tests without real model downloads.
- [ ] Соединить `UvInstaller.prepare()` и `PythonBootstrap.prepare()` с пользовательским AI Hub startup: сейчас обе операции — самостоятельные Rust API, не автоматическая цепочка при старте приложения.
- [ ] Получить/закрепить platform-specific official distribution checksums, mirrors, CI для Windows ZIP; проверить настоящий managed Python download и native wheel install. Сейчас network installer покрыт лишь локальными архивами и mock uv.
- [ ] Добавить resolver/version lock generation и Python runtime import/file probes. Manifest о наличии backend/overlay означает **файловую установку**, не доказательство inference readiness. Разрешить композиции общих backend layers и активных overlays с проверкой cross-layer distribution conflicts.

**Проверка:** `cargo fmt --check`, `cargo clippy --offline --locked --all-targets -- -D warnings` для default и no-default PASS; полный `cargo test --offline --locked --no-default-features -- --test-threads=1` PASS (199 passed, 2 ignored). Полный default-feature `cargo test` не завершился до инструментального таймаута во время компиляции, не помечать PASS.

### Managed Python bootstrap integration — 2026-11-03

- [x] `ai_runtime::managed::ManagedRuntimeBootstrap`: orchestration `UvInstaller.prepare()` → `PythonBootstrap.prepare()`, returns verified uv and managed Python executable paths; no venv, system Python or second Supervisor.
- [x] End-to-end mock bootstrap contract test on Linux (local pinned tarball with fake uv; install/find/reuse managed Python); 13 AI Runtime Rust tests and 4 Python SDK tests PASS, Clippy `-D warnings` PASS.
- [ ] ApplicationLifecycle/AI Hub must invoke the bootstrap before worker availability; no actual internet download or Windows integration has been verified.

### AI Runtime dependency resolution / selected compositions — 2026-11-03

- [x] Новый `ai_runtime::resolver::DependencyResolver`: `uv pip compile` из точных прямых Python pins, с constraints установленного backend; результат — строгий транзитивный `LockedPackage` список, из которого исключаются authoritative backend packages. Несовместимые версии запрещены явно.
- [x] Новый `ai_runtime::composition::RuntimeComposition`: одна service selection по слотам; `split` включает только её overlays, `shared` проверяет все одновременно активные overlays/backend layers и отказывает при конфликтующих пакетах/версии Python. Неселектированные модели не попадают в `PYTHONPATH`.
- [x] `AiSupervisor::start_composed` запускает Python worker с проверенными путями выбранных слоёв; разделение ответственности Rust process/IPC и Python model inference сохранено. Contract test: Python module в одном selected overlay доступен worker без установки в системный Python.
- [x] Python host поддерживает отдельный `installplan.py: InstallContract` для `install_plan/probe_install/settings_schema`, не импортируя потенциально отсутствующие heavyweight inference packages. Устаревший builtin layout временно сохраняет совместимость (fallback `plugin.py`). Новый test демонстрирует отсутствие требуемого runtime-пакета.
- [ ] Полноценный resolver для wheels с markers/URLs/extras/hashes и динамических Python/OS platform tags, без запуска непроверенных build scripts; текущий парсер сознательно принимает только plain exact pins и отклоняет неподдерживаемый формат.
- [ ] Контроль согласованности фактического Python ABI с выбранными locks; verify импортов, версии и целостности файлов в опубликованных слоях; ещё нет production installation-to-activation orchestration.
- [ ] Реальный managed uv/CPython download, PyTorch/ONNX packages, RVC/Silero/F5 TTS, API и UI settings/live lifecycle остаются открытыми. Test fixtures не доказывают production readiness.

**Локальные проверки:** no-default-features full cargo test PASS, `cargo clippy --no-default-features --all-targets -- -D warnings` PASS, Python SDK 5 tests PASS. Реальный dependency download/inference и default-feature full cargo test этого среза отдельно не подтверждены.


### AI Runtime implementation checkpoint — 2026-11-03: install evidence and candidate activation

**Source commits:** fb3c971, bf95b39, 61e83ef, 40f85f9, b437553. These are incremental AI Runtime changes in the architecture PR, **not Python parity or production AI readiness**.

- [x] Rust parses Python SDK's typed install plan from an **ephemeral** Python child (not the live inference worker).
- [x] A model install handler validates backend choice, resolves model pins against backend constraints and publishes an immutable overlay revision.
- [x] Required/optional model assets are inspected at safe relative paths, with symlink rejection and optional SHA-256; report separates Missing, Present, Verified, HashMismatch. Missing/invalid required files block **ready-for-initialization**, while an installed package overlay can still exist.
- [x] Supervisor can prepare and initialize a candidate worker and replace the active generation **only after success**. Failure preserves the previous worker; late requests from an old generation return StaleGeneration.
- [x] Split-mode activation locks are **per worker slot**; preparing TTS does not block ASR calls. Contract tests cover rollback, successful replacement and concurrent ASR access.
- [ ] **Not done:** actual asset download/installation hooks; verification of Python distributions/imports beyond package-layer manifest; durable active-model selection; real GPU inference / model warmup; persisted readiness; interruption-safe restore across process crashes; OS-level sandbox for arbitrary community Python plugins.
- [ ] Verify published HEAD through CI on Windows and Linux; local Rust 1.98.1 and Python tests do not substitute for full remote CI or real installed models.

Keep #24 (worker lifecycle), #42 (installable lifecycle), #59 (dynamic plugin contract), #61 (voice assets) open. Prefer real model probes and installation verification next; do not add a second Python supervisor.

## Semantic retrieval (новый срез)

- [x] Независимый EmbeddingProvider и Gemini 001/2 HTTP adapters; явные модели, task/protocol и cache-key space.
- [x] Асинхронный retrieval от зафиксированной проекции Timeline без второго canonical store и блокировок во время HTTP.
- [x] Корректное членство при изменении, удалении, merge, fork/history и пересоздании cache; lexical явно сохраняется.
- [x] Синтетическое сравнение качества и числа embedding-запросов; HTTP fixture tests в обоих профилях.
- [x] Опциональный live benchmark harness: единый lexical/semantic shortlist, проверка размеченного корпуса, Hit@1/Hit@3, latency, request/text/cache counters; синтетический offline fixture. См. `src/generation/SEMANTIC_BENCHMARK.md`.
- [ ] Реальные измерения на представительном корпусе с явно предоставленным Gemini API key, точные billing tokens/USD, GUI/пресеты и расширенные RAG/knowledge graph операции.

Технический контракт: [src/generation/SEMANTIC_RETRIEVAL.md](src/generation/SEMANTIC_RETRIEVAL.md).

### AI Runtime TTS SDK implementations — 2026-11-03

- [x] Добавлены двенадцать Python TTS plugin variants, **использующих** нейтральный `neuromita_ai_sdk`: Edge+RVC CUDA/ONNX, Silero+RVC CUDA/ONNX, F5 RU/EN с RVC и без, Fish regular/compiled/RVC, OmniVoice. `installplan.py` доступен без тяжёлых импортов, конкретная реализация находится в `python/neuromita_voice`, а не в SDK.
- [x] В `PluginInstallPlan` добавлены Python requirement specs (точные пины и ограничения); Rust `DependencyResolver` применяет `uv pip compile` с ограничениями backend layers. Python-реализации сохраняют `numpy==1.26.0` и `scipy==1.12.0` вместо неявной установки NumPy 2.x. Для RVC нужен совместимый managed Python (исходная модель рекомендует 3.11; прежний тестовый Python 3.13.5 не production-default).
- [x] 10 Python contract tests и 88 Rust library tests без default features PASS локально; Clippy `-D warnings` PASS. Проверены manifest discovery, install plans, settings, отсутствие фиктивной readiness и отказ от неверных assets/settings.
- [ ] **Не считать TTS port готовой production parity:** реальные model weights и pip packages не загружались; inference на GPU не запускался. RVC native runtimes, Fish Speech compile hooks, platform-specific installs и F5/Omni asset download требуют реальных end-to-end проверок.
- [ ] Задать/проверить backend lock с `torch` + `torchaudio` + `numpy==1.26.0` и подходящими CUDA wheel indexes, вместо установки одного `torch` с неограниченными transitive deps. Реализовать installer hooks и verified downloads, Python distribution probes, plugin-specific voice profile paths; затем воспроизвести реальный синтез.
- [ ] Разделить сообщение о наличии каталога/контракта TTS от доказанной работоспособности конкретных моделей; не считать полный аудио pipeline рабочим по mock tests.
### AI Engine — verified runtime increment (2026-11-03)

The implementation and exact remaining limitations are tracked in [AI Engine implementation increment](future/ai-engine-implementation-20261103.md). Model asset staging with SHA-256, verified package layers, typed install hook approvals, typed Python settings, voice profile references and a Rust-to-mock-Python WAV contract are implemented and tested on the AI Runtime branch. These are infrastructure acceptance slices, **not real model inference**. The default `ApplicationConfig` still uses an unavailable voice provider; AI Hub, full lifecycle integration, production installs, ASR and VAD remain open. Do not close #24/#26/#37/#42/#51/#59/#61 solely on these tests.


### AI Engine — explicit application-owned Python worker activation (2026-11-03)

- [x] Optional `ApplicationConfig.ai_runtime` activates resolved Python compositions during `ApplicationLifecycle::start`, after fallible migrations/settings, TCP bind and Timeline setup; the running application owns worker shutdown and injects the selected TTS through the existing `VoiceProvider` port. Absence of explicit AI configuration preserves the previous unavailable-voice behavior without fallback.
- [x] Reject mismatched locked Python, invalid service topology, missing/duplicate selected models and failed activation before client APIs are exposed.
- [x] Abort of an in-flight IPC call terminates/poisons the worker; stale results cannot be reused without a new worker generation. Mock tests cover cancel/restart and startup failure paths.
- [ ] Build the complete production `uv`/CPython/SDK/overlay/weights resolver into the application and persistent settings; implement AI Hub controls and smoke-test actual Silero+RVC inference. See [implementation report](future/ai-engine-implementation-20261103.md). All global AI Runtime issues remain open.

### AI Runtime implementation — verified generated assets (2026-11-03)

- [x] `AssetHook` is separate from overlay mutations and is returned by the versioned Python SDK.
- [x] Rust-owned model asset generation requires exact hook approval, a verified source revision, private staging, declared output validation, hash receipts and atomic publication. Failed hooks do not replace an active revision.
- [x] GigaAM ONNX has an explicit Python export hook and package requirement for `onnx`; the runtime worker no longer needs to export its own checkpoints.
- [ ] Real GigaAM CTC/RNNT checkpoint export and ORT inference on Windows/CPU/DirectML are **not verified**; production uv/bootstrap/install/activation/AI Hub orchestration is still open.

Implementation and limitations: [AI asset generation](future/ai-asset-hooks-20261103.md).

## AI audio pipeline / streaming transport / model source catalog (2026-11-03)

A typed application-owned speech session (FFmpeg PCM input → Earshot VAD/PTT → ASR → ChatApi → TTS → FFplay output), bounded post-inference TTS streaming IPC and SHA-256-pinned source catalog with `ModelAssets::Catalog` have been implemented. **These are code-level integration increments, not complete end-user parity.** Normal Slint Desktop does not yet select and activate AI models or expose physical mic/PTT controls; the source catalog intentionally contains no unverified model URLs/hashes, and TTS streaming only chunks WAV *after* model inference. See [audio-pipeline-streaming-catalog](future/audio-pipeline-streaming-catalog-20261103.md).
