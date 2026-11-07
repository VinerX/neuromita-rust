# Семантический ingress Unity / Games — первый Rust-контракт

## Проверенный источник

Историческая спецификация [Unity issue #366](https://github.com/NeuroMita/NeuroMita-Unity/issues/366) определяет назначение событий, но **не** устанавливает текущий формат Unity IPC.

Проверенный Python-источник — `Atm4x/NeuroMita`, экспорт ветки `session-timeline`, файл `src/services/unity_timeline.py` (`UnityTimelineIngress.ingest`) на сохранённом исходном снимке 2026-11-01. Он формирует один пакет событий из `context.runtime_events`, `data.reaction_events`, участников диалога и timer-типа (`idle` / `periodic` и др.). Результат прикрепляется к `req_id`, записывается до принятия решения о генерации, а повторная доставка проверяет совпадение исходной команды. Вызов подтверждён в `src/game_connections/handlers/actions/create_task.py`, вызов `timeline_ingress.ingest` до проверки блокировки события и dispatch.

Это **свидетельство Python-стороны**, а не подтверждение, что актуальный Unity-клиент отправляет все поля в том же формате. Для настоящего transport adapter необходим отдельный аудит живой Unity-ревизии (GitHub issue #1).

## Реализованное поведение Rust

- `timeline::ingress::SemanticBatch` — одна операция с внешней стабильной идентичностью и последовательностью `SemanticEvent`.
- `SemanticEvent` несёт собственный стабильный ID, тип, origin actor, явных свидетелей, структурированный payload и source timestamp. Не моделирует сетевой request/response Unity и не решает, какие свидетели имеют право получать событие.
- Поддерживаются `UnityEvent`, `GameStarted/GameAction/GameEnded`, `CharacterJoined/Left`, `PlayerInactive`, `ProactiveOpportunity`; записи чата, reset знаний и администраторские изменения не маскируются под игровые события.
- `append_semantic_batch` выполняет один guarded append. Если событие в пакете недопустимо, не записывается ничего. Повтор того же operation ID с теми же event IDs, исходным временем и данными возвращает исходный receipt. При изменении содержимого или source timestamp — конфликт.
- `ApplicationLifecycle::publish_semantic_events(selection, batch)` подключает операцию к живому Core без создания LLM-запроса. Выбор ветки/поколения проверяется перед записью и в той же SQLite-транзакции.
- Сохранённые события поступают в уже существующий `MitaView` по правилам видимости и `learned_at`. Причина `react` не отсекает более поздние известные события.

## Неподтверждённое / не реализованное

- Фактическая совместимость с текущим wire-протоколом Unity: команды, авторизация, версии envelope, формат acknowledgments и delivery guarantees. **Транспортный адаптер не добавлен.**
- Правила определения реальных свидетелей по игровым позициям/разговорам, включая Python `can_hear_speaker` и `participants`. Сейчас набор свидетелей предоставляет вызывающая прикладная операция; доверять необработанному сетевому полю `observers` нельзя.
- Автоматическая смена присутствия персонажа на основании контекста диалога. Presence-факты можно записать, но их создание по Unity dialogue snapshot требует подтверждения актуального протокола.
- При повторной доставке необходимо сохранять прежний event ID/occurred_at и payload, а не вызывать `SemanticEvent::new` заново. Повтор только по одному operation ID с новыми IDs не является тем же намерением.
- Полное восстановление внешних эффектов Unity и гарантированная межпроцессная ранняя отмена не входят в эту операцию.

## Проверки

`cargo test --locked --offline --test timeline_semantic_ingress`

`cargo test --locked --offline --features sqlite-system --test timeline_semantic_ingress`

Покрыты атомарность пакета, idempotent receipt, конфликтующий повтор, отсутствие события при неверной категории, приватность двух персонажей, presence projection, reopen SQLite и отказ устаревшего поколения selection внешнего writer.
