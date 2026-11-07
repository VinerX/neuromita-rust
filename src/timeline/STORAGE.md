# Session Timeline: границы хранения (SQLite → CFMD)

Статус 2026-11-01: SQLite-адаптер и тёплый revision-aware cache реализованы в feature branch; эта заметка отделяет **реальные гарантии** от плана оптимизации и будущих миграций.

## Архитектура

```text
ApplicationLifecycle ── TimelineConfig ──────────────┐
                                                    ▼
MessageHandler → CharacterHandler → SessionTimeline (compatibility view)
                                                    │
                                                    ▼
                                     dyn TimelineRepository + Send
                                         │                   │
                                         ▼                   ▼
                                    TimelineStore    SqliteTimelineRepository
                                   [in-memory]         [sqlite-system]
                                                            │
                                                    timeline_* tables
                                                    (WAL + transactions)

SessionWorkspace -> TimelineSelectionRepository -> durable CAS (sqlite-system)

SettingsLifecycle -> SettingsRepository -> [Memory / JSON / future SQLite / future CFMD]
```

`TimelineRepository` — **не универсальная база**. Это набор предметных операций `create_session`, `branch`, `event`, `world`, `read`, `append`, `fork`, `view`, `knows`. `TimelineSelectionRepository` отдельно хранит выбранную ветку и CAS-generation. Доменные правила один раз реализованы в `TimelineStore`; SQLite адаптер под транзакцией `BEGIN IMMEDIATE` читает подтверждённое состояние, применяет те же правила и атомарно публикует изменившуюся ветку, события и receipt. Идемпотентный повтор проверяется до требования свежего `expected_head`.

Для `timeline` собственная структура таблиц `timeline_branches`, `timeline_events`, `timeline_observers`, `timeline_receipts`, `timeline_active_selection`, `timeline_schema`. Выбор ветки по поколениям **не** кодируется посредством SQLite rowid / data_version / транзакционного revision — это отдельная семантика Core.

При явной `TimelineConfig::Sqlite { database, session, root }` рабочий Core пишет непосредственно в единственный SQLite repository; при повторном запуске с теми же IDs история и durable active selection сохраняются. Повторное `create_session` существующего root пропускает полную реконструкцию и не записывает новую revision. `TimelineConfig::Transient` соответствует старому режиму и **не** подменяет собой SQLite при ошибках. `ApplicationLifecycle::start` возвращает ошибку, если база недоступна или имеет неподдерживаемую версию схемы. По умолчанию `ApplicationConfig::minimal()` задаёт Transient для старых тестовых сценариев.

## Settings и общая физическая БД

Часть Settings **может** жить в том же файле SQLite, если её собственный `SettingsRepository` получит отдельный адаптер и отдельную `settings_*` схему. Это не делает Timeline владельцем настроек, не объединяет lifecycle и не даёт двум доменам общий глобальный SQL-менеджер. В идеале один файл — разные логические репозитории, собственные инварианты и явные транзакционные области. Когда появится реальная операция, требующая одновременного commit настроек и timeline, сначала утверждается её *доменная* атомарность, и только затем проектируется координация транзакций.

## Условия замены SQLite на CFMD

CFMD-адаптер реализует **те же** предметные контракты и обязан подтвердить: атомарные batch/receipt/head, точную повторную выдачу receipt после продвижения head, причино-следственную допустимость в унаследованном префиксе, изоляцию несовместимых будущих веток, snapshot-consistent read, CAS selection, durability и недопущение кросс-веточного раскрытия приватных фактов. CFMD revision не заменяет `BranchId`/`EventId`/head sequence/selection generation. Settings отдельно перейдут через свой repository контракт. Перенос обеих частей в один физический движок не означает соединения их предметных API.

## Осознанные ограничения текущего адаптера

- `sqlite-system` **opt-in**. Он использует системную SQLite 3 C-библиотеку через изолированный FFI в `storage/sqlite.rs`, без добавления случайных сетевых зависимостей в offline-vendor. Для Windows-release потребуется поддерживаемый способ поставки SQLite DLL или замена FFI на аудированный vendored binding. Сборка здесь подтверждена только на Linux с системной SQLite; сборка Windows не проверена.
- Путь файла и IDs root/session задаются явно. Вложенные директории не создаются скрыто.
- `read` для прямых фактов SQL-индексированный и постраничный, но `view`/`knows` пока реконструируют состояние целиком. `append` восстанавливает только родовую цепочку целевой ветки на холодном пути и после записи из другого соединения; соседние ветки и другие сессии не загружаются. На последовательном **тёплом** пути он использует кеш `TimelineStore` текущей ветки, валидированный SQL-ревизией `timeline_revision`, обновляемой в той же `BEGIN IMMEDIATE` транзакции; на ошибке SQL кеш сбрасывается. Правила `TimelineStore::append` остались единственными. **Это частичное ускорение:** валидация доменного prefix всё ещё O(история ветки), холодный append читает всю родовую цепочку, а прямой `event()` ещё использует сканирование мира. При обычном чтении `world`/`view` используются индексированные выборки локальных событий веток, без реконструкции всех сессий. Потребуются предметные инкрементальные индексы и бенчмарки, прежде чем называть путь production-ready.
- Сохраняются timeline-события и receipts, **не** mutable Character state / эффектов LLM, не assets, не SessionArchive, не внешнее состояние Unity. Общая атомарность Timeline + Character state и crash recovery пока открыты.
- `TimelineTurn` теперь передаётся через `MessageHandler` и проверяется в `CharacterHandler.accept` при смене selection, reset и утрате знания trigger. `ApplicationLifecycle::select_timeline_branch` публикует выбор через durable CAS, а активный выбранный branch восстанавливается после restart. **Оставшаяся граница:** внешние SQLite-switch не транслируются в локальный `watch` сразу. При этом `TimelineRepository::append_guarded` атомарно сверяет generation, context epoch и trigger с persisted selection в `BEGIN IMMEDIATE`, защищая commit от гонки с внешним писателем. Межпроцессное раннее уведомление и durable commit `CharacterState` ещё не реализованы.
- Включение SQLite не означает миграции пользовательской Python `Histories/world.db`: схемы Python и Rust различаются. Нужен отдельный проверяемый импорт или transfer archive.

## Проверки

`cargo test --locked --offline --features sqlite-system` и `cargo clippy --locked --offline --all-targets --features sqlite-system -- -D warnings` на Rust 1.98.1. Плюс базовый тест без feature. Проверяются воспроизводимость семантики на in-memory и SQLite, SQL rollback в середине записи и после прогрева кеша, инвалидация кеша другим писателем, два независимых writer-соединения, долговременный selection CAS, live pin и отмена приёмки устаревшего результата, постраничные чтения через ветки и shutdown/restart Core с сохранённой историей и selection.

## Холодный append без полной реконструкции (схема 2)

Семантическая проверка одна: `TimelineStore::validate_batch` с доменным контрактом `WorldLookup`. In-memory реализация читает `WorldIndex`, SQLite — производные индексные факты (`timeline_fact_keys`, `timeline_acquisitions`, `timeline_resets`) по явной цепочке предков и cutoff. Это **не SQL-валидатор**: он не принимает решение, допустима ли ссылка, ответ, direct observation или edit; возвращает минимальные свидетельства доменному валидатору.

`timeline_events` и `timeline_receipts` остаются каноническими. Индексные таблицы заполняются в той же транзакции, что и публикация событий, и могут быть перестроены из журнала. При открытии schema 1→2 выполняется один транзакционный rebuild. Удаление сведений из индексных таблиц само по себе не является удалением истории. Внешний CFMD адаптер должен предоставлять эквивалентный `WorldLookup` из своих поддерживаемых запросов, не копируя SQL-схему и не переопределяя доменные правила.

Не закрыты: атомарная координация состояния `Character` между хранилищами, read-side инкрементальные MitaView, исторические snapshots, межпроцессная отмена и измерения большого/deeply forked workload.

## Локальные архитектурные границы

`domain::knowledge_change` единожды определяет последствия факта для свидетеля (reset/direct acquisition). `WorldIndex`, `character_view` и SQLite `index_fact` интерпретируют только результат этой операции. `SqliteTimelineRepository::append_guarded` проверяет generation и опциональную пару epoch/trigger под той же блокировкой и транзакцией, где записывает факт. Конфликт selection не может быть обойдён повторным receipt.

`SQLite event()`, `knows()` и `context_epoch()` выполняют индексированные lookup по родовой цепочке, не восстанавливая весь мир. Полные `world()` и `character_view()` остаются дорогими read-side операциями; MitaView paging/summaries — отдельная незакрытая задача. Адаптер rusqlite/bundled SQLite изолирован в `storage/sqlite/native.rs`; legacy-проекция вынесена в `timeline/legacy.rs`, а данные событий остаются в доменном формате.

## Общая валидация и production bootstrap

validate_batch проверяет специальные факты: payload, actor, приватных witnesses и диапазоны. Изменения памяти последовательно проверяются внутри транзакции; неизвестный ID, переполнение и конечные exact collisions отклоняют весь пакет. Индекс памяти — производная проекция событий; SQLite читает её по родовой цепочке/cutoff в той же транзакции.

TimelineConfig::Persistent восстанавливает существующую сессию или создаёт новую. Несколько сессий без selection дают Conflict. CLI хранит Timeline в data-dir/timeline.sqlite3. SQLite включён по умолчанию и собирается из vendor; --no-default-features не подменяет persistent конфигурацию transient-хранилищем.
