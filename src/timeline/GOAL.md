> **Текущий статус (2026-11-02).** Этот подробный план описывает исторические этапы Timeline и содержит старые имена API/упоминания позднего раскрытия. Для текущего приоритета см. [корневой GOAL.md](../../GOAL.md), для обязательной семантики знания — [KNOWLEDGE.md](KNOWLEDGE.md), для операций — [CHAT_API.md](CHAT_API.md). При расхождениях приоритет у действующей реализации и этих контрактов; перенос знания X через рассказ Y запрещён. Unity сейчас вне scope.

# Session Timeline — перенос Python-контрактов в Rust

**Feature branch:** `feature/session-timeline-port` (основана на `audit/acyclic-dependencies`).
**Python reference:** `Atm4x/NeuroMita`, `session-timeline`, `5f3d061f523d9abefa6a264bb2953048a935fa65`.
**Rust starting point:** `8a4922b2a8cd19e757525030d57dea4bf6b35528` (содержимое опубликованной ациклической ветки).

## Цель

Перенести **семантику**, а не структуру Python-сервисов: канонические неизменяемые факты, независимые субъективные знания Миты, причинность, сохранение веток и восстановление состояния. Оставить один Rust Core, один активный пользовательский контекст, отдельные источники событий Unity и Sandbox. Не подменять Unity-мир моделью Rust. Не внедрять EventBus, общий service locator или альтернативное хранилище «истины» в MitaView.

## Этап A — чистое математическое / доменное ядро (`src/timeline/domain/`)

- [x] Типизированные `SessionId` / `BranchId` и устойчивые `EventId` / `CharacterId`.
- [x] Набор `EventKind`, типизированная причинность `reply_to`/`react_to`/`caused_by`/`observes`.
- [x] Immutable `TimelineFact`, event drafts, явная последовательность фактов, сохранение источникового времени (не concurrency-token).
- [x] Атомарный append в **in-memory** store по `expected_head` (без частично записанных batch).
- [x] Идемпотентность по ключу + сохранение receipt: повторный идентичный intent получает тот же результат после продвижения головы; конфликтующие намерения отклоняются; пустые операции не создают событий.
- [x] Запрет ссылок на ещё не существующие события, чужие ветки и недоступные персонажу цели реакции.
- [x] Private observations, запрет позднего раскрытия через пересказ и прямое знание одного факта.
- [x] `CharacterContextReset` обнуляет проекцию знаний без удаления событий.
- [x] Presence как fold `CharacterJoined` / `CharacterLeft` без ретроактивного раскрытия фактов.
- [x] Fork только от checkpoint; prefix cutoff + отдельное продолжение родителя, включая nested forks.
- [x] Правки/скрытие/восстановление сообщений как независимые факты; базовая per-character `character_messages()` проекция.
- [x] Публичная запись общего conversation message как одного факта: speaker, target, participants, witnesses, per-character role; пересказы как отдельные сообщения без изменения знаний об оригинале ([CONVERSATION.md](CONVERSATION.md)).
- [ ] Полная Python `CharacterMessageProjection` (UI metadata, bounded paging, cache, resize semantics).
- [ ] Полная валидация versioned JSON-envelope, UTC-ISO и всех Python-правил; текущий Rust домен хранит UTC Unix milliseconds.
- [ ] Унификация внешних `string` identities с Python при импорте архивов (сейчас доменные IDs UUID).

## Этап B — постоянное хранилище и перенос между хранилищами

- [x] `TimelineRepository`: независимый объектный порт доменной семантики. CFMD должен реализовать этот же контракт без изменения событий/веток/знаний.
- [x] SQLite (`sqlite-system`): WAL, нормализованные ветки / события / свидетели / receipts; `BEGIN IMMEDIATE` для атомарных операций и проверка `expected_head` под блокировкой записи; открытие после завершения Core.
- [x] Отдельный порт `TimelineSelectionRepository`: durable CAS выбора ветки; повторный выбор инкрементирует generation.
- [x] SQL-индексированное ограниченное чтение прямых фактов по ветке, позиции, количеству, непосредственному наблюдателю и inherited prefix; это ещё не готовый кеш MitaView.
- [x] Тесты одного контракта на in-memory / SQLite: receipt после reopen, атомарный batch и SQL fault injection после обновления head, конкурентные отдельные соединения, совпадение страниц через ветвление.
- [x] SQLite `append` не восстанавливает доменный `TimelineStore`: валидатор `TimelineStore::validate_batch` получает адресный `IndexedWorld` непосредственно в транзакции. `timeline_revision` остаётся метаданными совместимости, а не критерием актуальности кеша. Все правила остаются в одном домене.
- [x] SQLite `branch()`, durable selection CAS и повторное `create_session` для уже существующего root выполняются без восстановления всей истории или изменения revision при повторном открытии.
- [x] SQLite `world` и `view` читают мир выбранной ветки (с inherited prefixes), без других сессий. `knows`, `event` и `context_epoch` используют индексные обращения с lineage cutoff, без полной материализации истории. Семантика reset выделена в единую доменную операцию `knowledge_change`, от которой строятся проекция и производные индексы.
- [x] **Инкрементальная проверка на тёплом пути**: `TimelineStore` поддерживает принадлежащий домену `WorldIndex` по ветке (event identities, external message identities, knowledge after reset). Batch сначала проверяется на overlay без изменения основного индекса; при ошибке не изменяются ни факты, ни индекс. После успешного append индекс расширяется на новые факты. На тёплом пути проверки зависят от размера batch и ссылок, а не длины всей истории. [Измерения](PERFORMANCE.md).
- [x] **Холодный SQLite append без загрузки истории**: единый доменный `TimelineStore::validate_batch()` запрашивает `WorldLookup` (существование и тип события, внешний message ID, знания после reset); SQLite читает эти факты из транзакционных производных `timeline_fact_keys`, `timeline_acquisitions`, `timeline_resets` с индексами и ограничением родовой цепочки ветки. Запись фактов, наблюдателей, receipts, индексов и продвижение head выполняются одной `BEGIN IMMEDIATE` транзакцией; прежняя версия схемы мигрирует 1→2 единожды в транзакции. Сложность lookup определяется количеством предков и индексными обращениями, а не полной длиной истории. **Не утверждать O(1) во всех случаях**: уровень вложенности веток и SQL/IO влияют на время.
- [ ] Перенести вложения `TimelineAssetStore` и контентно-адресуемые ссылки с верификацией.
- [ ] Версионированный `SessionArchive`: export/import, строгая проверка дерева, ссылок, receipts и assets.
- [ ] Полный crash recovery для сквозных операций Python state participants / Unity / Character — SQLite транзакция только для timeline не решает распределённое восстановление.

## Этап C — MitaView / история / наблюдения

- [x] Базовый **беспобочный** `character_view`: события + момент получения знания, соблюдение reset; поздние раскрытия разрешаются через индекс уже встреченных фактов, без квадратичного поиска по всей истории.
- [x] Семантические сообщения, actor/role, правки, скрытие и восстановление без изменения исходного события.
- [ ] Постраничное чтение и эквивалентность полному Python-формату провайдерских сообщений.
- [ ] Context summaries, инвалидация по branch / head / epoch / версии правил без попадания скрытых фактов в prompts.
- [ ] Историческая реконструкция `at`; выборка большого объёма без полного сканирования для каждого запроса.

## Этап D — связи с живым ядром и средами

- [x] Подключить in-memory доменный `TimelineStore` как единственный источник фактов в действующем `SessionTimeline` (`timeline/mod.rs`); старые `HistoryView`/`EventContent` стали вычисляемыми compatibility-проекциями.
- [x] В `ApplicationConfig.timeline` можно **явно** выбрать SQLite repository по пути/SessionId/BranchId (опциональная Cargo feature `sqlite-system`), единственное хранилище событий для работающего Core. Старый transient-режим сохранён как явно выбранная конфигурация.
- [x] Живой `MessageHandler` переносит `TimelineTurn` из `CharacterHandler.prepare` через генерацию в `CharacterHandler.accept`; выбор другой ветки/повторный выбор, сброс знаний и исчезновение права знать trigger отклоняют устаревшее принятие.
- [x] **Локальная адресная отмена in-flight запросов** при `select_branch`: наблюдение `watch` за текущей `TimelineSelection` прерывает ожидание LLM или TTS в `MessageHandler` и переводит устаревший запрос в `Cancelled`. Запросы, начатые с новой selection, не отменяются. `CharacterHandler.accept` сохраняет окончательную проверку. Нет общего EventBus.
- [ ] **Внешняя отмена и reset:** выбор ветки другим процессом уже атомарно сверяется с `append_guarded` во время записи, но не уведомляет немедленно локальный `watch`; reset знаний персонажа атомарно проверяется перед commit, но не прерывает генерацию заранее. Ещё нужны явные межпроцессные/контекстные контракты, а не периодический опрос всего Core.
- [ ] Завершить **сквозную транзакционную приёмку** состояния персонажа и Timeline. Сейчас приёмка проверяет актуальность selection/epoch и согласует собственный `CharacterState` в памяти с проекцией истории, но durable commit mutable state/effects не реализован.
- [x] Локальный контракт `SessionWorkspace`: selection CAS, generation, pin и validation для reset/смены ветки; подключён к `MessageHandler` и `CharacterHandler`. SQLite принимает pinned-ответ только через `TimelineRepository::append_guarded`, включая generation, reset epoch и право знать trigger в той же транзакции, что и события.
- [ ] `SessionTimelineRuntime` целиком: есть durable selection CAS, pin в реальном MessageHandler и проверка перед accept, но ещё нет целевой отмены активных токенов, восстановления владельцев состояния до активации или сквозной атомарности.
- [ ] `ConversationTimeline`: message identities, reply/react/initiate; отдельные факты речи и наблюдений.
  - [x] Адресные `Hide / Revise / Restore` доступны через публичный Rust Core API, с проверкой знаний и сохранением оригинальных фактов.
- [ ] Unity ingress: сведения о мире, видимость, приватность, игровые события, idempotent receipts.
- [ ] Launcher Sandbox: самостоятельный messenger context (удалённое общение), без симуляции Unity.
- [ ] `SessionRestoration`: state participants, journal, компенсирующие переходы и возобновление после сбоя.

## Инварианты и ограничения

- **Один источник истины:** Python `SessionTimeline` → Rust authoritative timeline; минимальный внешний контракт теперь является совместимым вычисляемым представлением над новым единственным in-memory store. Не объявлять данный этап полным переносом.
- **Causal links не выводятся из близости событий.** Субъективные знания не равны всей истории.
- **Rollback/fork не уничтожает альтернативное будущее.** Изменение ветки/контекста инвалидирует подготовленную работу, если она относится к старой реальности.
- **Не вводить новые Api/Lifecycle/Handler**, если достаточно чистой модели и адресной операции.
- **Ни один unchecked пункт не объявлять готовым.** Измерения Python не являются бенчмарками Rust.

## Проверка

- [x] `cargo test --offline` Rust 1.98.1 (локально, 2026-11-01; 1 live Gemini intentionally ignored).
- [x] `cargo clippy --offline --all-targets -- -D warnings`.
- [x] `cargo fmt --check`.
- [x] Domain parity tests на ветвление, receipts, causal, знание, reset, presentation edits (18 integration (including deterministic stress and concurrency) + 3 workspace unit + 2 compatibility unit).
- [x] SQLite integration (feature `sqlite-system`): domain parity, SQL fault injection без кеша, reopen, конкурентные записи, инвалидация кеша другим писателем, индексированные страницы, durable selection и реальный Core restart с сохранением выбранной ветки.
- [x] End-to-end тест: смена selection между `MessageHandler.prepare` и ответом провайдера **отменяет ожидание** старого ответа до разрешения провайдеру отвечать; свежий запрос успешно завершается, исходное событие остаётся без ответа.
- [x] Bench: `cargo run --offline --example timeline_append_bench`; 8k одиночных append до/после индекса ~556 / ~6.2 мкс на событие (локальный debug-профиль, не гарантия и не SQLite throughput).
- [x] SQLite cold append: `cargo run --locked --offline --features sqlite-system --example timeline_sqlite_cold_bench -- 200 1000 3000`; локальный debug-прогон ~7–16 мс на событие сразу после reopen (транзакция + открытие нового соединения на запрос, без сканирования событий). Test parity: reset/relearn/sibling fork + legacy schema v1→v2 + previous suite.
- [x] Проверено, что новые timeline/domain и selection импортируют только identity/interaction и собственные низовые компоненты; инверсии `timeline -> character/request` не добавлены. Это ручная проверка направленности, не автоматическое доказательство компилятором.

## Архитектурное уточнение (refactor/session-timeline-architecture-20261101)

Единый `domain::knowledge_change` определяет получение/сброс знания; SQLite и in-memory индексы и read-проекция не определяют его независимо. `append_guarded` атомарно проверяет persisted selection, context epoch и trigger в SQLite `BEGIN IMMEDIATE`; `CharacterHandler` интерпретирует конфликт как устаревшую приёмку. `legacy.rs` содержит преобразование старого формата, `storage/sqlite/native.rs` изолирует unsafe FFI. Внешняя мгновенная отмена, durable commit состояния персонажа, read-side paging MitaView и сквозной recovery **не закрыты**.

## MitaView read-side: первый функциональный пакет (2026-11-01)

- [x] Независимый `domain::CharacterMessageProjection` поддерживает per-character знания, reset, позднее раскрытие, edit/hide/restore, мультимодальный JSON payload без SQL/provider зависимостей.
- [x] `timeline::view::TimelineMessageQueries::capture` читает direct witnesses постранично через единый `TimelineRepository`, адресно разрешает disclosure и возвращает immutable `MitaView` со стабильным `branch/through`, `messages/page/count/contains`.
- [x] Provider/UI map адаптирован отдельно от domain, с сохранением вложенных полей. Проверены in-memory/SQLite и fork/history snapshot. [Архитектура и ограничения](VIEW.md).

**Это не закрывает** оставшиеся чекбоксы Stage A/C: нет кеша MitaView, полного Python format/time/resize parity, отдельной атомарной CharacterState snapshot и wiring всех ChatAPI/Unity consumers. Bounded read pages не означают bounded total work на каждый capture.

## ContextCapture и интеграция MitaView (2026-11-01)

- [x] Один immutable prefix через `SessionTimeline::capture_context` для MitaView и compatibility HistoryView; `CharacterHandler::prepare` берёт их на исполнении из фиксированного prefix, в который уже входит принятый при admission `PlayerMessage`, state с тем же anchor; `CharacterView` экспортирует domain view вместе с legacy history.
- [x] Gemini использует предметные сообщения MitaView вместо разбора legacy EventContent, сохраняет роли user/model, не включает скрытые события или знания другого персонажа; SessionNote остаётся user-level контентом, без повышения привилегий provider system instruction.
- [x] Регрессионные тесты на отсутствие дублирования текущего input, исторический prefix, private/hidden/reset и мультимодальные текстовые фрагменты; базовые и SQLite тесты, Clippy.

**Не закрыто:** межпроцессная атомарность mutable CharacterState и Timeline, прочие providers и мультимодальные `image/audio` части, оптимизация кеша MitaView и legacy consumer migration. `CharacterContext.history` пока остаётся compatibility-проекцией и продолжает materialize полный известный мир — это не утверждение об O(page size) подготовке запросов.

## Cause-scoped prompt и восстановление счётчика ответов (2026-11-01)

- [x] `react(event_id)` проверяет знание причины на том же `ContextAnchor`; причина реакции не обрезает видимую историю: провайдер получает все известные персонажу к моменту подготовки сообщения и сведения мира, включая сообщения позже `react_to` (если они уже опубликованы в Timeline).
- [x] Узкий `CharacterState` (accepted replies/revision) восстанавливается по фактам принятых ответов конкретной ветви, не по субъективной HistoryView. Reset/hide не обнуляют счётчик, SQLite reopen сохраняет его. На приёмке счётчик перечитывается для выявления сторонних изменений.
- [x] Контрактная проверка In-memory/SQLite, включая cause snapshot и provider rules; локальные и SQLite-тесты Rust 1.98.1.

**Граница:** полный mutable CharacterState не сделан durable/atomic с Timeline; подсчёт принятых ответов по канонической истории сейчас O(история). И `reply`, и `react` используют все известные персонажу события на зафиксированном при исполнении anchor. Входы `ChatApi.reply` теперь публикуются при admission до исполнения; публичный message-only ingress реализован отдельно и не создаёт RequestRecord.

## Следующий пакет изменений

Холодный SQLite append и инструмент замера добавлены. В текущем цикле большие бенчмарки отложены: приоритет — политика контекста и snapshot-consistent MitaView, полная provider/UI schema, ранняя адресная отмена при reset/внешнем переключении и затем согласованное восстановление владельцев состояния. Release-бенчмарки 50k+ событий остаются отложенной проверкой, не блокирующей предметную разработку. См. [STORAGE.md](STORAGE.md) — границы возможного общего файла SQLite для Settings и условия будущей миграции на CFMD. **CharacterHandler уже использует единый TimelineRepository (transient либо SQLite) через совместимый фасад, но не обладает всем контрактом Python TimelineRuntime.** До завершения SQLite, восстановления, архивов и Unity/Sandbox adapters **нельзя** считать Python SessionTimeline полностью перенесённым.

## Входящие сообщения до исполнения запросов (2026-11-01)

- [x] `reply` записывает `PlayerMessage` в канонический Timeline **при принятии** после успешного резервирования очереди, до регистрации RequestRecord. Сообщение остаётся фактом при cancellation и переживает SQLite reopen. При отказе из-за capacity новое событие не возникает.
- [x] При `prepare` захватывается актуальный известный prefix, включая ожидающие сообщения; собственный опубликованный trigger проверяется по request id, actor, content и знанию персонажа и не записывается повторно. Gemini не повторяет собственное сообщение в контексте.
- [x] Admission pinning: selection generation связывает очередь с выбранным миром; stale queued requests отменяются без перехода в новую ветку. Отдельные `append_player_message`/`append_proactive_trigger` и `RequestIngress` в application composition не загрязняют domain правилами request registry.
- [x] Тесты: отложенная `react` видит позднее `reply` до фактического dispatch; отменённое ожидающее сообщение остаётся; отказ из-за queue overflow не пишет факт; SQLite reopen сохраняет опубликованное сообщение; reselect инвалидирует очередь.

**Открыто:** межпроцессная быстрая отмена без ожидания очереди; согласованная транзакция mutable CharacterState и provider multimodal parts. Не считать ingress-стадию самостоятельным завершением `ConversationTimeline`/`SessionRestoration`.

## Публикация без генерации и явная цель ответа (2026-11-01)

- [x] `ChatApi::publish_message(character, input)` записывает `PlayerMessage` независимо от `RequestRegistry`/LLM/очереди. В Timeline нет выдуманного `request_id`: операция идемпотентна по `message:<EventId>`, сохраняется SQLite-reopen, видна лишь указанному персонажу. Запись требует активного Core и актуального selection, отказ не создаёт запрос к провайдеру.
- [x] `GenerationFocus` внутри `CharacterContext` отдельно хранит `TurnIntent` и разрешённую `ReactionCause` (id, kind, текстовое описание, если есть). Причина `react` проверяется на знание персонажем в фиксированном `ContextAnchor`; MitaView остаётся неизменённой read-моделью и включает все доступные последующие факты.
- [x] Gemini-адаптер обозначает цель `reply`, если за ней появились более поздние сообщения, не копируя сам текст целевого сообщения второй раз. Для `react` обозначает уже известное событие, в том числе UnityEvent, отсутствующее в текстовой MitaView. Внешние инструкции не попадают в provider `systemInstruction`.
- [x] Регрессионные тесты: старый reply target при новых сообщениях; нетекстовое событие как причина реакции, новые сообщения, приватность между персонажами; message-only вход → следующий prompt без запроса при публикации; SQLite reopen.

**Не закрыто:** полная типизированная передача семантических Unity payload в мультимодальный provider, массовый broadcast сообщения, durable CharacterState и межпроцессная ранняя отмена. `ConversationTimeline` требует дальнейшего покрытия обратными сообщениями, реакциями и интеграцией игровых producers.

## Субъективные semantic world events в MitaView (2026-11-01)

- [x] `MitaView.world_events()` и `context_entries()` объединяют известные
  сообщения и смысловые world/game события по времени *приобретения знания*;
  оригинальные JSON данные не преобразуются в предполагаемый формат Unity IPC.
- [x] Gemini получает известные миру события как user-level structured data,
  учитывает позднее раскрытие и reset, не отсекает поздние сообщения относительно
  `reply_to/react_to`, не дублирует известную причину.
- [x] Контрактные тесты in-memory и SQLite на private witnesses, disclosure,
  snapshot, reset/relearn и fork cutoff.
- [ ] Проверить актуальный Unity/Python wire-контракт и реализовать настоящий
  ingress от работающих игровых producers. Проекция **не** означает завершение
  Unity ingress из этапа D. См. GitHub Issues #1 и #13.

## Semantic ingress — Rust Core без предположений о Unity wire (2026-11-01)

- [x] Публикация идентифицированного `SemanticBatch` с несколькими смысловыми событиями через `ApplicationLifecycle`, без запуска генерации; одна транзакция, один operation receipt, явные свидетели, исходные payload/ID/time и защита selection generation.
- [x] Доменная проверка категорий: игровой ingress не выполняет reset, не редактирует сообщения и не подменяет запись диалога. Контракты SQLite и in-memory проверены, включая повторную доставку и crash/reopen.
- [ ] Актуальный Unity↔Rust wire/transport, правила назначения свидетелей по Unity snapshot и автоматическая синхронизация presence не реализованы; старая Python `UnityTimelineIngress` изучена как источник функционального поведения, не как окончательная схема текущей Unity.

Подробнее: [INGRESS.md](INGRESS.md). Это частичное закрытие функционального ingress, не завершение Unity integration из этапа D.

## Saved-branch inspection (2026-11-01)

- [x] `CharacterHandle::get_context` can inspect an inactive branch of the selected session without changing the selected branch or starting generation. `HistoryView`, `MitaView` and derived accepted-reply counts use the same requested `ContextAnchor` (branch + prefix).
- [x] In-memory and SQLite/reopen tests cover inherited fork prefix vs parent continuation, per-character privacy, reset with immutable earlier snapshots, foreign-session rejection and active-generation protection.
- [ ] Full snapshot-consistent mutable `CharacterState` transaction and bounded incremental read-side remain open; inspecting an immutable prefix does not solve them.

## Early local character reset cancellation (2026-11-01)

- [x] Durable, idempotent addressable character reset operation with selection generation precondition. One committed reset forgets only the named character's knowledge without erasing canonical facts.
- [x] Per-character local context-epoch observation: pending jobs pin epoch at admission; LLM and TTS are cancelled early if their character is reset before acceptance. Another character's generation continues. New requests after reset use the new epoch.
- [x] Tests: blocked Gemini, independent character, waiting reaction, interrupted voice synthesis, idempotent reset, stale branch generation, SQLite reopen, old historical context, Clippy and Rustfmt.
- [ ] External-process reset/selection notifications still need an independently designed transport or event mechanism; guarded accept already rejects stale commits, but cannot abort the LLM await early across processes. Full durable mutable CharacterState and effects recovery are also not implemented.

## Согласованность исторической генерации после reset (2026-11-01)

- [x] `ContextPosition::At` проверяет эпоху знаний персонажа на запрошенном префиксе против его текущей эпохи. Если между снимком и генерацией был `CharacterContextReset`, запрос отменяется до LLM/TTS, а историческое чтение остаётся доступным.
- [x] Индексированный SQLite и in-memory обеспечивают один контракт `context_epoch_at`, включая унаследованные префиксы fork и повторный запуск.
- [x] End-to-end проверка через `ChatApi::reply(...).at(...)`: один персонаж не может восстановить забытые сведения, независимый адресат сохраняет доступ к своему историческому контексту; текущий выбор ветки не меняется.
- [ ] Это не реализация выбора активного персонажа Sandbox, расширяемого реестра персонажей или полной атомарности mutable `CharacterState`.

## Captured-prefix turn pinning (2026-11-01)

- [x] `TimelineTurn.context_head/context_epoch` refer to the actual captured MitaView prefix, not the SQLite head observed later when creating the turn.
- [x] React/initiate trigger creation has a guarded context epoch; a concurrent reset (including reset followed by reacquisition of the cause) rejects the stale work without recording an orphan proactive trigger.
- [x] Deterministic in-memory and two-connection SQLite regressions for reset/relearn between capture and turn pin, plus non-reset concurrent append, ensure generation cancellation without erasing immutable history.
- [ ] External SQLite writers still do not push immediate cancellation notifications; no full multi-component snapshot or durable CharacterState transaction is claimed.

## Atomic accepted-reply revision (2026-11-01)

- [x] The existing narrow `CharacterState.revision` is derived from accepted
  `CharacterMessage` facts on the selected branch. `AppendGuard` now carries the
  expected **character-specific** accepted-reply revision. For both in-memory and
  SQLite repositories, the revision is checked as part of the guarded append;
  SQLite checks it under the same `BEGIN IMMEDIATE` transaction as the reply.
- [x] Two independent SQLite connections cannot accept two responses prepared
  against the same character revision, even if the second connection observes
  the updated branch head after its earlier state read. Another character's
  reply does not invalidate this revision. Fork cutoffs, reset, and SQLite
  reopen do not create another mutable reply counter.
- [ ] The current check scans canonical branch facts (including inherited
  prefix) for accepted replies; a versioned derived index is a separate
  optimization task. This is **not** durable atomic storage of arbitrary
  mutable `CharacterState` or external effects.


## Примирение результата запроса после перезапуска (2026-11-01)

- [x] Read-only `ApplicationLifecycle::accepted_reply_at(character, request, anchor)` возвращает канонический `CharacterMessage` для указанного RequestId, актёра и исторического префикса той же сессии. Проверены fork cutoff, reset, SQLite reopen, отсутствие request registry и отсутствие повторного выполнения операции. [Семантика и ограничения](RECOVERY.md).
- [ ] Полный crash recovery request lifecycle, инварианты внешних effects и восстановление технических статусов — **не закрыто**. `None` от lookup не означает отмену или ошибку; source facts не содержат достаточно информации для такого вывода.

## Unified conversation message contract (2026-11-01)

- [x] One `ConversationMessage` canonical draft conversion for explicit publication,
  `ChatApi` player input and generated `reply/react/initiate` character answers.
  Speaker is a typed Player/Character; no separate message-shape writer.
- [x] `ConversationAudience` carries independent typed `targets`, `participants`
  and actual character `seen_by`. A character sender always knows its own
  utterance; target/participation never imply observation or broadcast.
- [x] Per-message routing from request to admission and accepted answer:
  `ReplyRequest::with_input_audience` and all generated-request
  `.with_audience`. The two audiences are independent because the input and
  answer are different utterances. Old single-character defaults remain.
- [x] One canonical fact appears with relative role in each witness's MitaView;
  private character-to-character messages, group replies, `react` to a known
  character utterance, `initiate`, historical access and late disclosure are
  exercised by tests. SQLite and in-memory implementations use the same schema.

**Still open:** direct generated `reply_to` referring to an existing
character message without synthesizing a player input; actual Unity dialogue
roster/witness resolution; full provider/UI parity. No new recovery path or
second authoritative message store was introduced.

## Unity-directed conversation replies (2026-11-01)

- [x] `ChatApi::reply` takes the `EventId` of an already published visible Message. Player and character messages follow the same path; no nested `ReplyTarget` or `.to_event` and no synthetic `UserInput` are needed.
- [x] `ChatApi::react` accepts any observed Event. `ChatApi::initiate` has no event target. Both write one generated Message, not a `ProactiveOpportunity` control event.
- [x] Unity chooses the responder and sends each turn explicitly via TCP `ServerLifecycle` (`publish_message`, `reply`, `react`, `initiate`). Rust never auto-initiates further turns.
- [x] Generated replies use `ReplyTo` links; reactions use `ReactTo`; no transfer of knowledge follows either link.
- [ ] Validate and update Unity C# transport and dynamic scene witness mapping against this Rust contract; Python structured-output provider parity remains separate.
