# Structured response v3 — текущий срез

**Референс:** Python `src/schemas/structured_response.py`, `RESPONSE_PROTOCOL_VERSION = 3`, ревизия `5f3d061f523d9abefa6a264bb2953048a935fa65`. Реализован первый этап #19; **полная parity ещё не достигнута**.

## Контракт

- Один `StructuredResponse` (v3) и упорядоченные `ResponseSegment`. Схема содержит поля segments, reasoning, image_description, изменения состояния, памяти, напоминаний, knowledge graph, tool_call, custom_fields, working_state. Интенты/аргументы принимают объект или JSON-строку, как в Python Gemini.
- `GenerationProvider` теперь возвращает **типизированный** `StructuredResponse`, а не сырой текст. Общий `ResponseGenerator` проверяет ответ и формирует `ProposedResponse { structured, text, effects }`.
- Gemini отправляет `responseMimeType: application/json` и `responseSchema` в `generationConfig`; декодер исключает thought-parts, требует finishReason=STOP и строго разбирает JSON в v3.
- `structured_message_gemini.json` — срез **разрешённой сегодня** схемы, выведенный через Python `StructuredResponse.gemini_schema_dict(exclude_fields=..., exclude_segment_fields=...)`. В нём доступны `segments[].text`, `segments[].emotions` и опциональные `attitude_change` / `boredom_change` / `stress_change`, `working_state` и `memory_add/update/delete/merge`. Эмоции — данные сегмента, не выполнение анимаций или других действий. Протокол данных v3 и урезанная схема конкретного запроса — разные понятия.
- Контракт ошибок намеренно строже терпимого Python-парсера: никакой автоматической починки JSON, markdown-парсинга или превращения невалидного ответа в текст. Неподдерживаемые поля/команды (tool calls, game actions и др.) отклоняются **до** принятия ответа в Timeline и до TTS.
- `reasoning` допускается схемой *входного* DTO для совместимости, но **не запрашивается** у Gemini и не выводится пользователю/Timeline. Нельзя включать его в пользовательские логи.

## Что ещё предстоит

- Следующий набор вариантов `ResponseEffect` пока не реализован: tool calls, game actions, напоминания, отдельная доставка позиционных команд и адресатов. Для них нужен собственный исполняющий контракт, а не общий event bus.
- Полная capability-dependent генерация схем для доступных исполнителей и кастомных параметров; Python/Rust golden fixture parity.
- Sparse response, flexible Python parser, работа с tool calls, мемори, внутренним состоянием и UI, безопасные retries; всё это остаётся в #19 и соответствующих Issues.
- Первый типизированный эффект: `ResponseEffect::ChangeCharacterState(CharacterStateDelta)` формируется только из валидированного `StructuredResponse`; это **предложение**, не мутация. `CharacterHandler` при приёмке материализует `AppliedEffect::CharacterStateChanged(CharacterBehavior)` и передаёт вместе с Message в один `append_guarded` batch. Отдельный `CharacterStateChanged` содержит **абсолютное принятое состояние** и ссылку `CausedBy` на сообщение. В SQLite это одна транзакция. `AcceptedResponse.effects` содержит принятые значения, `state_after` — подтверждённое состояние. Нет второго mutable state store.
- Working → Accepting → Applied/Discarded относится ко всему пакету. TTS выполняется в Working: отмена/reset/shutdown отбрасывают предварительный результат. Затем начинается короткое принятие, после которого отмена — TooLate. Ошибка TTS допускает VoiceGenerationFailed с принятым текстом; отмена не превращается в частичный успех.
- В первом срезе применяются default шкалы Python: исходное состояние (60/10/5), per-turn hard limit ±6, округление до 2 знаков и итоговые границы [0,100]. **Конфигурируемые пределы из Python пока не перенесены**, их следует хранить вместе с принятой операцией, не переписывая исторические значения; текущие абсолютные факты уже защищают от ретроактивного пересчёта.
- В Timeline `CharacterMessage.content` теперь содержит массив типизированных сегментов. Provider/UI-проекция сохраняет JSON-массив `content`, `ProjectedMessage.content` и legacy history выводят производный текст. AcceptedResponse также несёт сегменты; `reasoning`, инструменты и изменения состояния не становятся содержимым сообщения. Не считать это исполнением всех полей Python v3.

## Проверки

Unit tests Rust DTO и bounded state policy, Gemini wire, запрет неисполняемых действий; вертикальные HTTP-тесты типизированных segments и первого применяемого эффекта, stale-turn rollback, reset, historical state и SQLite reopen. Базовые наборы с SQLite/без SQLite, Rustfmt и Clippy.

## Принятое рабочее состояние (новый срез)

- Тип `generation::WorkingState` — реэкспорт доменной структуры `timeline::domain::CharacterWorkingState`, поэтому Timeline не зависит от DTO и провайдера.
- Непустой `StructuredResponse.working_state` → `ResponseEffect::ReplaceWorkingState` → `AppliedEffect::CharacterWorkingStateReplaced`. Пустой `{}` является **явной очисткой**. Отсутствующее поле **не изменяет** предыдущий снимок. В отличие от старого Python, где включённая поддержка и отсутствующее поле могли очистить кеш, это намеренная семантика частичных typed-effects в Rust.
- Snapshot ограничен 2000 Unicode-символов (focus ≤600, четыре списка по ≤6 элементов, элемент ≤400). Превышение ограничений — ошибка до приёмки; ответ не коммитится. Контент не заменяется автоматически усечённой версией.
- Timeline сохраняет `CharacterWorkingStateReplaced` в общем атомарном append с ответом, как приватное событие с `CausedBy` и наблюдателем-персонажем. `character_working_state_at` восстанавливает последнюю замену на branch prefix; `CharacterContextReset` обнуляет текущую проекцию, но не удаляет исторические значения. Отдельного mutable-cache нет.
- `CharacterContext` и `CharacterView` получают эту проекцию. При следующей генерации текстовое представление отправляется отдельным assistant context-block `[WORKING STATE]`, не как системная инструкция, не в `Message.content`, не в публичную проекцию мировых событий. Содержимое считается недоверенным model-produced handoff, **не** chain-of-thought.
- Это **расхождение с Python**: Python держит working_state в ephemeral in-process cache и не восстанавливает его после перезапуска; Rust использует канонический Timeline для воспроизводимых веток и historical reset. Решение требует отдельного product-level review, если понадобится exact persistence parity.
- Долговременная память реализована отдельно от временного `working_state`: базовые CRUD/Merge эффекты и детерминированный ограниченный отбор для LLM-контекста. Семантический RAG пока отсутствует.

## Долговременная память — базовый typed-effect срез

- `src/timeline/domain/memory.rs`: идентичность `MemoryId(UUID)`, приоритет `Low/Normal/High/Critical`, `MemoryRecord` и `CharacterMemories`. Это отдельная доменная модель; Timeline остаётся единственным каноническим журналом **принятых** изменений, отдельного mutable cache нет.
- DTO Python v3 сохраняет четыре массива строк; Rust декодирует их в `MemoryCommand::{Add,Update,SoftDelete,Merge}` **до acceptance**. Новая адресация update/delete/merge — только стабильный UUID, **не** Python ordinal; этот разрыв требует явной миграции UI/промптов и не является exact parser parity. `memory_add`: `priority|content` или plain text с Normal, `memory_update`: `uuid|priority|content` либо `uuid|content`, `memory_delete`: `uuid`, `memory_merge`: `target_uuid,source_uuid,...:optional content`. Unknown IDs, duplicate merge IDs, слишком длинный текст и неверный формат не дают частичной записи. Legacy ranges/islands отсутствуют.
- На acceptance `CharacterHandler` читает `CharacterMemories` из фиксированного branch-prefix, **валидирует и материализует всю последовательность** в `MemoryChange::Upsert`, только затем вместе с `CharacterMessage` передаёт её одним `append_guarded`. Все `CharacterMemoryChanged` имеют `CausedBy` на Message, владельца-character и не появляются в общем `MitaView` world feed. Результат в `AcceptedResponse.effects`; одни `EffectPhase` и граница отмены для всего пакета.
- При перезапуске SQLite и historical view восстанавливаются из фактов; каждому персонажу принадлежит своя проекция. `CharacterContextReset` очищает непосредственное знание о событиях и рабочий handoff, но сохраняет личную долговременную память (явное product-semantic решение). В prompt добавляется отдельно маркированный `[LONG-TERM MEMORY]` с ID, приоритетом и контентом; это данные, не инструкции. Отбор не изменяет проекцию: см. [MEMORY_RETRIEVAL.md](MEMORY_RETRIEVAL.md).
- Ограничения текущего контракта: максимум 16 операций за один ответ, до 256 живых memories на персонажа, до 2000 Unicode-символов на запись. Prompt теперь выбирает максимум 12 записей в пределах 6000 Unicode-символов (включая метаданные) по точному лексическому совпадению, приоритету и стабильному ID. Повторный Add с нормализованным совпадающим содержимым сохраняет ID и при необходимости повышает приоритет; Update/Merge, создающие точный дубликат другой записи, отклоняются. Это **не** semantic duplicate suppression или RAG. Нет embedding/vector lookup, entities, TTL, island upsert, auto-forget, user editor или адаптера Python ordinal ID. Не заявлять memory parity до их реализации.

## Предметный пакет

TurnProposal в turn.rs имеет закрытые поля; generation::ProposedResponse сохранён как реэкспорт. Генератор через from_structured валидирует DTO. CharacterHandler использует getters сегментов, текста и эффектов, без повторного разбора DTO. Правила находятся в character/state.rs и memory.rs, старые пути timeline::domain сохранены как реэкспорты.

- Новый `memory_delete` из LLM переводит факт в `Deleted` (soft delete), а merge оставляет исходные записи в `Deleted` с сохранением идентичности. Старые физические `MemoryChange::Delete` продолжают воспроизводиться без изменения истории; физический purge остаётся отдельной административной операцией с backup.
