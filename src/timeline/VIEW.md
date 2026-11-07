# MitaView — предметная проекция и read-side

## Источники

- Историческая концепция: https://github.com/NeuroMita/NeuroMita-Unity/issues/366 (2026-09-18).
- Python reference: `NeuroMita/session-timeline`, `domain/timeline/projections.py` и `services/timeline_history_queries.py` на `5f3d061f`.
- Текущий источник истины — Rust `TimelineRepository` с immutable `TimelineFact`, а не Python/Unity history.

Issue #366 определяет **смысловые события**, границы знаний персонажа, причинность и сохраняемые ветви. Его пример маршрута Unity → Python → Timeline — исторический, а не обязательный runtime-топологический контракт Rust.

## Границы ответственности

```text
Unity / Sandbox / Games ──(typed semantic inputs)──> application operations
                                                      │
                                                      ▼
                                               TimelineRepository
                                                      │
                         ┌────────────────────────────┴────────────────────────┐
                         ▼                                                     ▼
                domain::CharacterMessageProjection                   other domain projections
                         │
                         ▼
                timeline::view::MitaView
                         │
                         ▼
                 provider/UI document adapter
```

- `domain::CharacterMessageProjection` — deterministic fold по `KnownEvent`. Домен отвечает за знание, reset, видимость, ревизии, роль и идентичность сообщения. Не знает про SQLite, HTTP, Python-сервисы или формат ChatAPI.
- `TimelineMessageQueries` — read-side orchestrator поверх одного `TimelineRepository`: фиксирует branch/head, читает только direct witnesses небольшими страницами, не разыменовывает причинные ссылки, передаёт домену непосредственные факты в порядке наблюдения. Не записывает events или mutable CharacterState.
- `MitaView` — независимый snapshot через `branch` + `through`, с `messages`, `provider_messages`, `page(limit, offset)`, `count`, `contains`. Копии результатов detached; чтение исторического `at` не привязывает результат к текущему head.
- Provider/UI map — **адаптер** `timeline::view`, а не модель доменного события. Сохранены произвольные вложенные поля `payload.message` и типизированный JSON `content`, включая мультимодальные массивы.

## Контракты и ограничения

1. Visibility одного персонажа не раскрывается другому. Поздняя disclosure использует исходный immutable факт, но `learned_at` ставит временем раскрытия. Повторное знание не дублирует сообщение.
2. События `MessageHidden` / `MessageRevised` / `MessageRestored` воздействуют только на уже известные данному персонажу сообщения. Reset очищает read-проекцию, но не историю мира.
3. Сортировка сообщений по `(learned_at, sequence)`, `page` — последние `limit` видимых сообщений с пропуском `offset` с конца, результат по возрастанию. Страница `0` — пустая. Изменения старых событий не переписывают source facts.
4. `through > head` — ошибка; legacy `HistoryView` продолжает работать отдельно до согласованной миграции всех consumers. Снимки через `capture` detached, последующий append их не меняет.
5. Read-side **не кеширует** свёрнутый view между вызовами и пока воспроизводит все видимые по истории direct-witness факты на каждый capture. Поэтому bounded storage reads **не равны** O(page size) общей стоимости, особенно для страниц из конца истории.
6. Полного равенства Python provider schema пока нет: Python вычисляет локальный строковый `time` из исходного ISO datetime. Rust хранит UTC Unix milliseconds и не подставляет отсутствующее поле `time`. Изменение поведения времени — явная отдельная задача адаптера, не доменной модели.
7. `capture` фиксирует immutable prefix, но **не** включает snapshot mutable CharacterState. Его согласованность с состоянием персонажа должна обеспечиваться отдельной операцией захвата контекста.

## Тесты

`cargo test --locked --offline --test timeline_mitaview`

`cargo test --locked --offline --features sqlite-system --test timeline_mitaview`

Проверяются reset и relearn, приватность, поздняя disclosure, edit/hide/restore и metadata, мультимодальный content, несколько страниц источника, fork cutoff и исторические снимки на обоих backends.

## Редактирование и видимость сообщений через Core API (2026-11-01)

- `ApplicationLifecycle::change_message(selection, character, operation_id, action)` предоставляет проверяемые операции `Hide`, `Revise` и `Restore` без обращения к SQLite напрямую. Один вызов записывает один `MessageHidden` / `MessageRevised` / `MessageRestored` с конкретным свидетелем.
- Доменная валидация требует, чтобы целевое сообщение **было известно указанному персонажу**; нельзя менять чужие приватные сообщения или защищённые поля идентичности. Редакция поддерживает любой JSON `content`, включая массивы provider parts.
- Событие изменения неизменно, исходное сообщение остаётся в Timeline. Для Python parity `Revise` скрытого сообщения вновь делает его видимым; `Hide → Restore` возвращает последнюю допустимую редакцию.
- Повтор операции с тем же `EventId` и тем же содержимым получает сохранённый receipt; конфликтующее повторение отклоняется. Неверная selection не производит запись. Старые MitaView не мутируют, чужие персонажи не получают чужие изменения.
- Это закрывает публичный путь **операций представления сообщений**; не объявляет завершённой всю `ConversationTimeline` (#10) и полную provider schema parity (#2).

## Runtime ContextCapture (следующий функциональный пакет)

- `SessionTimeline::capture_context(character, position)` фиксирует единый `ContextAnchor` до публикации нового входа. `MitaView` и временная legacy `HistoryView` вычисляются над одним и тем же immutable prefix. Снимки не смещаются автоматически при будущих append.
- `CharacterHandler::context` берёт снимок под собственным mailbox и локальным Timeline mutex, вычисляет `CharacterState` для того же prefix, после чего публикует вход и получает `TimelineTurn`. Текущий вход подаётся в генерацию отдельно и не дублируется в истории.
- `CharacterContext.view` — предметная read-model для провайдера; `history` временно оставлена только для существующих API/тестов. Счётчик принятых ответов теперь читается из канонических CharacterMessage с валидным request_id независимо от reset, hide или потери персонажем знания; это ветвевой snapshot, а не отдельный изменяемый источник истины. `CharacterView.view` предоставляет такую же проекцию при чтении исторического состояния через API.
- Gemini формирует контекст из `view.messages()`, а не из `HistoryView`. `User/Assistant` соответствуют `user/model`; `SessionNote` отправляется как **user-level** сведение, а не повышается до привилегированной `systemInstruction`. Лишь доверенные description/prompt и счётчик формируют системную инструкцию.
- В Gemini поддерживаются текстовые сегменты массива `content`; изображения и иные медиа-поля пока не кодируются в provider parts. Схема мультимодальности всё ещё требует отдельного контракта.
- Защита от внешней смены selection/reset на приёмке остаётся за `append_guarded`; локальная блокировка **не** является транзакцией между SQLite, внешними писателями и mutable CharacterState. Durable CharacterState и межпроцессная ранняя отмена остаются открытыми.

## Следующие изменения

1. Завершить `ContextCapture`: durable revision mutable CharacterState и explicit context policy по request cause/очереди; локальный prefix-capture уже подключён к `CharacterHandler` и Gemini.
2. Версионная инвалидация и ограниченный кеш для live MitaView, без второго источника истины.
3. Python/UI schema parity, время, page/resizing и адаптеры потребителей (ChatAPI, Unity, Launcher); отделить UI history от provider context.
4. Отдельная ранняя адресная отмена при reset/внешней смене мира; это не обязанность read-проекции.

## Контекст реакции и производное состояние (2026-11-01)

- Для обычной генерации (`ContextPosition::Latest`) `CharacterHandler` захватывает знания персонажа непосредственно в начале обработки работы из очереди. `reply`, `react`, `initiate` используют **всю** доступную на этот момент историю: причина реакции определяет causal link, а не фильтр истории. Поздние сообщения и наблюдения не скрываются лишь потому, что появились после `react_to`. При явном `ContextPosition::At` ограничение задаётся самим запрошенным историческим снимком.
- `SessionTimeline::reaction_cause_at` проверяет, что причина `react(event_id)` реально существовала и была известна персонажу на **захваченном** prefix. При несовпадении подготовка отклоняется **до** публикации нового входа. Эта проверка не меняет read-side.
- `ChatApi.reply` при admission резервирует место в очереди и сохраняет `PlayerMessage` в выбранной ветке **до** регистрации работы. Само событие не отменяется вместе с генерацией. Переполненная очередь не пишет событие; ошибка записи не регистрирует запрос. При исполнении `CharacterHandler` проверяет identity/знание опубликованного trigger и generation ветки, а не публикует его повторно. `react/initiate` оставляют инструкцию/`ProactiveOpportunity` событием начала исполнения, не маскируя их под речь игрока.
- `ContextCapture` при `Latest` включает весь известный персонажу мир к началу обработки, включая ожидающие сообщения. Gemini не добавляет текущий `UserInput` второй раз, если его событие уже включено в MitaView. `reply_to/react_to` остаются причинными ссылками и не фильтруют поздние известные сообщения. При явном `At` исторический cutoff сохраняется.
- Pin выборки берётся при admission, поэтому запрос, поставленный до смены ветки (даже если выбрана та же ветка повторно), не может исполниться после переключения в другой мир. Синхронизация с внешним SQLite-selection при admission и guarded append выполняется, но отдельный канал мгновенной межпроцессной отмены пока отсутствует.
- Отдельная публикация сообщений без генерации теперь доступна через `ChatApi::publish_message`; это не даёт snapshot-consistent захват внешнего mutable CharacterState.
- `CharacterState.accepted_replies/revision` для текущей узкой модели являются производными от канонических принятых `CharacterMessage` (actor совпадает, request_id валиден), а не от `HistoryView`. Reset, hide и чужие персонажи не изменяют счётчик. Значение сохраняется при SQLite reopen и относится к конкретной ветви/позиции; перед accept локальный владелец повторно читает его.
- Счётчик по-прежнему вычисляется проходом по каноническим событиям, а не индексированной агрегатной проекцией. **Не** реализован durable mutable `CharacterState`, межпроцессный единый read transaction MitaView+state и атомарность изменений внешних participants. Эти пункты остаются открытыми.
- Тесты проверяют обе реализации хранилища, приватные знания, реакцию на старую причину со всеми известными последующими репликами, мировые сведения, исторический cutoff, отказ неизвестной в указанной позиции причины, reset/reopen и раздельность состояний персонажей.

## Admission-time message ingress (2026-11-01)

- Прикладной `TimelineRequestIngress` реализует узкий порт `request::RequestIngress`; `request` не импортирует реализации SQLite, доменный timeline или глобальный контейнер Core.
- `PlayerMessage` является смысловым фактом, запись которого не зависит от успеха LLM, отмены, голоса и ожидания. Техническое состояние request registry не сериализуется в timeline.
- `SessionTimeline::append_player_message` и `append_proactive_trigger` разделены; `CharacterHandler` только проверяет ранее записанный вход для `reply`, затем захватывает `ContextCapture` и только после этого может сформировать trigger для `react/initiate`.
- SQLite-reopen, выбор другой generation, недостаточная очередь, отложенная реакция на событие двери и более позднее сообщение игрока покрыты регрессионными тестами. Не объявлять это завершением `SessionRestoration` или общей атомарности с CharacterState.

## GenerationFocus и независимые сообщения (2026-11-01)

`CharacterContext.focus` хранит `TurnIntent` отдельно от неизменного `MitaView`. Для `react` `SessionTimeline::reaction_cause_at` подтверждает существование и знание причины на конкретном историческом prefix и возвращает типизированный факт. `CharacterHandler` извлекает минимальный `ReactionCause`: тип и строковый `payload.content`, если он существует. Информация чужого персонажа не попадёт в причину реакции только по факту существования события в мире. Причина не фиксирует верхнюю границу видимости других событий.

Адаптер Gemini добавляет локальную пометку **только в сериализуемый prompt**, если исходное сообщение `reply` уже не последнее; оригинальный `ProjectedMessage` и Timeline не меняются. Если `react` ссылается на событие, представленное в MitaView, оно помечается; если это нетекстовое Unity/игровое событие, его доступный тип/текст передаётся как user-level контекст вместе с инструкцией. Текущий вопрос не дублируется при наличии целевого сообщения в MitaView. Более поздний вопрос игрока не фильтруется никакой `cause`-политикой.

`ChatApi::publish_message(character, UserInput)` публикует сообщение игрока без резервирования очереди, запуска модели или создания request identity. Это отдельная семантическая операция поверх существующего `RequestIngress`, а не новый глобальный сервис. Публикуется один адресный факт: многоперсонажный broadcast/набор свидетелей пока не реализован. После перезапуска SQLite факт сохраняется, а следующий `ContextCapture` получает его по обычным правилам знаний персонажа.

Следующие задачи: полноценное typed описание Unity/Game событий в provider read-side; согласованный mutable `CharacterState`; межпроцессная отмена/reset; мультимодальные provider parts и дополнительные администраторские операции над историей. Большие нагрузки сознательно отложены.

## События мира и игр в субъективном контексте (2026-11-01)

`MitaView` теперь возвращает не только сообщения, но и `world_events()` и объединённые
`context_entries()` в порядке **получения знания**, а не времени возникновения
события. Сохраняются исходный `EventKind`, `actor`, `payload`, timestamp, `sequence`
и `learned_at`. Покрыты уже существующие доменные виды `UnityEvent`,
`GameStarted`, `GameAction`, `GameEnded`, `CharacterJoined`, `CharacterLeft`,
`PlayerInactive`; технические события и `ProactiveOpportunity` не становятся
сообщениями мира автоматически. `count()`, `page()` и `provider_messages()`
по-прежнему относятся только к сообщениям и сохраняют прежние контракты.

Один и тот же факт виден персонажу только при непосредственном наблюдении;
`learned_at` соответствует sequence этого наблюдения. Причинные ссылки
на старый факт не раскрывают его. После `CharacterContextReset` ранее известные
факты больше не входят в текущий контекст: последующий рассказ о них
создаёт новое сообщение, но не восстанавливает оригинал. Fork наследует только префикс родителя;
продолжение соседней ветви не появляется в `MitaView`. Снимок фиксирован на
`through` и не меняется при дальнейшем append.

Gemini формирует **user-level** последовательность из сообщений и доступных
семантических событий мира. Структурированный payload события передаётся как
данные JSON без предположений о конкретном Unity IPC. При `react` знакомое
событие отмечается непосредственно в этой последовательности; новые известные
сообщения остаются в контексте и сама причина не дублируется в prompt.

**Граница:** здесь реализована проекция *уже сохранённых* типизированных
доменных событий. Текущий Unity↔Python transport, названия внешних полей,
версии wire-формата, producers и idempotency ingress ещё требуют отдельной
проверки фактических исходников: [Issue #1](https://github.com/Atm4x/neuromita-rust/issues/1)
и [Issue #13](https://github.com/Atm4x/neuromita-rust/issues/13). Нельзя
считать эту read-side работу интеграцией Unity. Нагрузочные проверки отложены.

## Read-only inspection of saved branches (2026-11-01)

- `CharacterHandle::get_context(ContextAnchor)` now reads a saved branch of the **same session** without selecting it. The returned `CharacterView.history`, `CharacterView.view` and derived reply counters use the requested `(branch_id, through)` rather than the active branch.
- `SessionTimeline::capture_context` remains deliberately **active-branch-only** for generation: `ContextPosition::At` cannot be used to create a response against an unselected branch. Inspecting an alternate branch never changes selection generation, writes facts, or cancels running requests.
- Historical views are detached. Appending or resetting the alternate branch cannot mutate an earlier returned view. A foreign session and a position beyond the branch head are rejected. Reading after SQLite reopen uses the same saved branch and prefix.
- This is a read-only functional extension, **not** a completed unified SQLite read transaction with external mutable `CharacterState`, a bounded incremental MitaView cache, a full timeline archive or a generalized time-travel runtime.

## Addressed local reset cancellation (2026-11-01)

- `ApplicationLifecycle::reset_character_context(selection, character, event_id)` creates
  a durable, observer-scoped `CharacterContextReset` with an idempotent receipt.
  Selection generation is checked before writing; a failed or stale reset does not
  notify workers or append a fact. Repeating a committed operation with the same
  event ID does not advance the head or emit a spurious update.
- The Core maintains one local `context_epoch` notification per character. Its value
  comes from the selected branch's canonical reset epoch; it is updated **after**
  a successful guarded reset. `MessageHandler` races ongoing LLM and voice work
  against both selection and character-specific epoch updates. A reset affecting
  Mita cannot cancel a KindMita request.
- Admission pins `(selection, context_epoch)` before a request enters the queue;
  `CharacterHandler` compares both when processing begins. Queued `react/initiate`
  requests admitted before reset are cancelled without creating a new generation.
  Previously published player speech remains in the canonical history, though
  its content is forgotten in the new subjective view until disclosed again.
- Existing final `append_guarded` checks remain mandatory. Local early cancellation
  is a resource-saving mechanism, not a substitute for atomic commit-time guards.

**Not implemented:** changes committed by another process to the same SQLite
repository do not directly push these in-process notifications; external reset
or selection changes are still detected by the final guarded acceptance check.
Likewise, this work does not add durable mutable CharacterState beyond the
existing accepted-reply projection. No Unity transport or polling service added.

## Historical generation and character knowledge epochs (2026-11-01)

- A read-only `CharacterHandle::get_context(old_anchor)` may inspect a saved state from before `CharacterContextReset`. It never changes selection or current knowledge.
- Generating with an explicit `ContextPosition::At(old_anchor)` is allowed only if the anchor and current active branch are in the **same character knowledge epoch**. Otherwise preparation yields stale state and the request is cancelled before LLM/TTS. This prevents historical generation from silently reusing knowledge that the character has since forgotten.
- `ContextPosition::Latest` continues to capture current knowledge when the worker starts. One character's reset does not invalidate another character's historical position unless that character also crossed a reset boundary.
- `TimelineRepository::context_epoch_at` evaluates the last reset at the requested branch position, including inherited fork prefixes. SQLite uses its existing reset indexes; in-memory and SQLite follow the same contract. No separate mutable knowledge state is introduced.
- This rule concerns explicitly addressed `CharacterId`s in the current minimal Rust Core. The Sandbox-selected character and any settings-driven character selection are not implemented here; the rule must not be confused with a client-side active-character resolver.

## Stable generation pin across concurrent writes (2026-11-01)

- A `TimelineTurn` now pins `context_head` and `context_epoch` from the **same immutable prefix** used to build the prompt. It must not re-read a later epoch after `MitaView` has been captured. This matters when an external SQLite writer resets the character and then discloses the old trigger again: knowing the trigger **now** does not validate a prompt from the forgotten epoch.
- `CharacterHandler` rejects a stale turn before invoking the generator. The final repository `append_guarded` remains the atomic authority if another writer changes the epoch afterward.
- `react/initiate` also guard creation of their `ProactiveOpportunity` against the captured epoch, so a stale request cannot leave a new trigger fact behind. Ordinary later events without a reset do not invalidate a previously captured turn.
- In-memory and two-connection SQLite regressions cover reset + re-disclosure between context capture and pin creation, rejection of stale proactive writes, and unchanged historical snapshots. This is **not** a full transaction across mutable character state, external participants, or a push-based notification channel from other processes.

## Accepted reply CAS at commit (2026-11-01)

- The `CharacterState.accepted_replies/revision` projection keeps its existing
  canonical definition (actor-matching accepted `CharacterMessage` with a valid
  request UUID); it is independent of subjective reset, hiding, and display.
- A pre-commit `CharacterHandler` read is useful for early rejection, but cannot
  protect against another SQLite writer accepting a reply immediately after it.
  Therefore `append_reply` also passes the **captured expected revision** to
  the repository's guarded append, which rechecks the same derived count inside
  its write transaction. Failed CAS writes no reply or receipt and returns
  `ConcurrentWrite` to the handler.
- A later unrelated world event or another character's accepted reply does not
  invalidate the current character's revision. The guard is scoped to the
  current branch's inherited prefix, and a reopened database derives the same
  count. This does not make external mutable state/effects durable; counting
  canonical replies remains linear in the branch's fact history for now.
