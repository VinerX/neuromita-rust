# NeuroMita Rust

Slint 1.18.1 и slint-build включены в необязательную feature `gui` и vendor. Добавлен десктопный GUI с Home, страницами-заглушками и реальной страницей Sandbox: `cargo run --locked --offline --features gui --bin neuromita-gui`. Он использует Winit + FemtoVG/OpenGL. Home остаётся preview, но Sandbox работает через `SandboxPresenter` и отдельный desktop runtime с реальным Gemini-провайдером при заданной модели и `GEMINI_API_KEY`; фейковые ответы не генерируются. [Статус GUI, меню и ограничения](future/gui.md), [порт Sandbox из PyQt](future/sandbox-ui-port-20261103.md). Для feature `gui` требуется Rust 1.92+, обычное ядро сохраняет минимальный Rust 1.89.

Локализация — общий Rust-модуль с `i18n(ID, source)`, исходными JSON Python и переводом существующего интерфейса при смене языка. EN/RU доступны в sidebar, остальные языки — через глобус. Внешние JSON: `--translations-dir PATH`, стартовый язык: `--language CODE`; в меню глобуса есть перечитывание каталогов. [Контракты и формат](future/localization.md). Экспорт шаблона: `cargo run --locked --offline --bin neuromita-i18n -- template`.

Логи CLI используют tracing/tracing-subscriber: время, уровень, модуль, файл/строка и контекст RequestId/персонажа. Цвет включается только для терминала; перенаправленный stderr и NO_COLOR дают обычный текст. По умолчанию warn для сторонних модулей и info для NeuroMita. Например, в PowerShell `$env:RUST_LOG = 'warn,neuromita=debug'` включает подробные логи приложения. Некорректный фильтр останавливает запуск с явной ошибкой. Текст переписки, ответы и секреты в добавленные события не записываются. Ответы CLI остаются в stdout; автоматического файлового хранения/ротации пока нет.

Новая Rust-реализация с нуля. Один процесс/одно ядро, статические Api, владельцы персонажей, субъективный контекст, очереди, эффекты, независимые presenters, типизированные settings/binding и ServerLifecycle.

Самостоятельный Cargo-проект и Git-репозиторий. Исходники находятся в src/, тесты — в tests/, проектные решения — в future/, функциональный аудит Python-приложения — в docs/. Зависимости для offline-сборки сохранены в vendor/.

Актуальные детали: [контракты](future/minimal-contracts.md), [чеклист](future/implementation-checklist.md), [HANDOFF](HANDOFF.md). Исторический первый срез описан [отдельно](future/first-slice.md).

## Offline-сборка и передача веб-агенту

Выполнять из каталога проекта, чтобы Cargo прочитал .cargo/config.toml:

```sh
cd NeuroMita-Rust
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```

Tokio включён с full. Cargo.lock и vendor/ содержат текущий разрешённый набор зависимостей для платформ сборки. Vendor не заменяет Rust toolchain и native build tools. Минимум Rust 1.89; проверено на Rust 1.99.1 / Windows MSVC. Native aws-lc-sys требует C/CMake build environment.

Не менять line endings/содержимое vendor вручную: Cargo проверяет checksums. Исходники и лицензии vendored crates сохраняются в Git, target/ не включается.

## Запуск

Требуется GEMINI_API_KEY в окружении. Ключ не передаётся в аргументах и не печатается. Модель, персонаж и адрес listener задаются явно:

```sh
cargo run --locked --offline -- --help
cargo run --locked --offline -- --model YOUR_GEMINI_MODEL --character mita --listen 127.0.0.1:5555
cargo run --locked --offline -- --model YOUR_GEMINI_MODEL --character mita --listen 127.0.0.1:5555 --message "Привет"
```

YOUR_GEMINI_MODEL — доступный вам model ID. Канонический каталог: Crazy, Kind, Cappie, ShortHair, Mila, Sleepy, Ghost, Creepy, GameMaster. Описания и промпты минимальные, без переноса всего функционала Python.

Консольные команды: /character Crazy, /character Kind, /cancel REQUEST_UUID, /quit. Ctrl+C завершает приложение; EOF ждёт отправленные данным frontend обращения. Консоль вызывает ChatPresenter и наблюдает его модель. Она не готовит Gemini wire payload и не меняет Character напрямую.

## Launcher

`neuromita launcher status` только читает выбранную установку и наблюдаемые процессы. `neuromita launcher session` владеет одним процессом игры на время интерактивной сессии и принимает `status`, `start`, `stop`, `open-location`, `quit`. Для launcher не нужен `GEMINI_API_KEY`. Сохраняются только root/candidate; PID и ownership между процессами не переносятся. Детали и матрица проверок: [rust-launcher](docs/03-runtime/rust-launcher.md).

Fine-tuning dataset управляется offline-командой `neuromita dataset` без Core и ключа: enable/disable, лимит/unlimited, каталог, статистика, рейтинги, фильтры, Raw JSONL и ShareGPT/ChatML с явным предупреждением о потере non-text данных. Сбор использует фактический provider payload и ответ до application validation, а accepted outcome хранится отдельно. Команды, поля и границы: [rust-finetune-dataset](docs/03-runtime/rust-finetune-dataset.md).

Диагностика отдельного `RequestId` доступна через `/diagnostics` в терминале и команды TCP. Подготовленный prompt, provider request/response, частичная ошибка, оценка токенов и фактический usage разделены; экспорт учитывает redaction, владельца запроса и ограниченное хранение. Подробности: [rust-request-diagnostics](docs/03-runtime/rust-request-diagnostics.md).

## Перезапуск Core в одном процессе

Статические `ChatApi`, `SettingsApi` и `CharacterApi` используют общий синхронизированный runtime slot вместо необратимого `OnceLock`. Одновременно допускается только одно ядро. `ApplicationLifecycle::start()` резервирует слот, а `shutdown()`/`Drop` закрывают приём новых обращений и доводят исполнителей до завершения. `ApplicationLifecycle::wait_stopped()` ожидает полного освобождения слота; после него можно запустить новый экземпляр. Старые presenter/handle не перепривязываются к новому ядру. `tests/restart.rs` проверяет последовательные запуски, отменённый старт, сбой IO и старые handles.

## Генерация и результаты

GenerationPreset содержит типизированный ProviderTemplate. Текущий адаптер — GeminiProvider с GeminiTemplate. URL, авторизация, wire format и обработка ответа находятся в generation/gemini. Общий GenerationProvider не предполагает HTTP или Gemini.

Авторизация Gemini соответствует исходному Python-протоколу google_gemini_default: ключ передаётся как URL query-параметр ?key=..., без x-goog-api-key/Authorization. Значение кодируется URL builder. Authenticated URL не сохраняется в trace, ключ скрывается в Debug, transport errors формируются через without_url().

HTTP-клиент reqwest собирается с json, rustls, http2 и system-proxy. Системный прокси Windows/macOS подхватывается автоматически; переменные окружения прокси также учитываются. HTTP/2 согласовывается с сервером, HTTP/1.1 остаётся доступен. Локальный адрес прокси не зашит в код. Изменения системного прокси после создания клиента требуют пересоздания клиента.

Нет каталога, CRUD, ротации и fallback пресетов. В точке сборки явно задаётся шаблон; начальные параметры Gemini: temperature 0.7, max output tokens 2048, timeout 60 секунд.

ChatApi предоставляет reply/react/initiate/send и возвращает RequestHandle. Есть Pending, Processing(Text/Voice), Finished с типизированным исходом и Cancelled.

До Accepting отмена прекращает оставшуюся работу и не применяет эффекты. После входа в Accepting отмена возвращает TooLate. Ответ и эффекты принимаются до Finished и до следующей работы того же персонажа.

Озвучка подключается через VoiceProvider. Успех возвращает текст/аудио; ошибка озвучки возвращает принятый текст и VoiceGenerationFailed. При отмене озвучки пакет отвергается. InternalError сохраняет доступный частичный текст.

По умолчанию реальный voice engine не настроен: UnavailableVoiceProvider возвращает явную ошибку, а не фиктивное аудио. Флаги озвучки начального профиля выключены.

## Настройки и binding

SettingsApi предоставляет snapshot/replace/edit/observe/flush/retry_save. Schema 2 содержит разделы presets, characters, voice, capture и interface; поддерживаемая schema 1 мигрируется явно. Volume валидируется как тип. Повреждённая текущая конфигурация не восполняется скрытыми defaults.

SettingsApi::prepare_import проверяет и мигрирует байты в закрытый SettingsTransfer; import(expected_revision, transfer) применяет все разделы через текущего владельца с CAS и обновлением bindings. Export возвращает текущие значения для encode. Startup и импорт используют общий MigrationService: схема, шаги и атомарное хранилище принадлежат модулю. StartupMigrations выполняет обязательный settings job и ApplicationConfig.migrations до публикации Core; ошибка останавливает запуск. PythonSettingsMigration пока возвращает NotImplemented. [Общая система](future/migration-system.md), [перенос настроек](future/settings-migrations.md).

Offline-команды без Core/ключа: `neuromita settings export --source STORE.json --destination BACKUP.json` и `neuromita settings import --source BACKUP.json --destination STORE.json`. Они переносят AppSettings; тела пресетов, credentials и Timeline не включаются. Приложение, использующее файл, должно быть остановлено.

SettingsPresenter.voice предоставляет bindable volume/in_launcher/in_game/voice: get, set и observe. Значение из другого источника обновляет свойство; наблюдение не вызывает повторную запись. Ошибка проверки оставляет подтверждённое значение.

CLI использует persistent bootstrap: settings.json, presets.json без секретов и timeline.sqlite3 в --data-dir. ApplicationConfig::minimal остаётся явным transient-профилем для тестов. JSON-хранение подключается через ApplicationConfig.settings: SettingsSource::Initial или Stored и JsonSettingsRepository. Ошибка сохранения наблюдаема отдельно от принятия значения; явный retry защищён от старой запоздавшей ошибки.

## Тестовый сетевой контракт

Подробные границы внешнего клиента, ошибки, повторы и отличия от Python/Unity: [Rust local control](docs/03-runtime/rust-local-control.md).

ServerLifecycle обслуживает TCP с UTF-8 JSON lines. Каждое сообщение завершается newline. Сразу после подключения приходит connected с client_id; далее settings snapshot и обновления.

Для простейшей проверки отправить строку:

```text
test
```

Она создаёт запрос с текстом test и персонажем ServerConfig.test_character. Приходит accepted с request_id, затем request_status. GuiPresenter обнаруживает эту работу независимо от сетевого исполнителя.

Структурированные команды:

```json
{"command_id":"00000000-0000-4000-8000-000000000001","command":"test"}
{"command_id":"00000000-0000-4000-8000-000000000002","command":"reply","character":"kind-mita","text":"Привет"}
{"command_id":"00000000-0000-4000-8000-000000000003","command":"settings"}
{"command_id":"00000000-0000-4000-8000-000000000004","command":"set_volume","value":55}
{"command_id":"00000000-0000-4000-8000-000000000005","command":"cancel","request_id":"REQUEST_UUID"}
```

Для нового намерения нужен новый command_id. Повтор того же ID/содержимого в окне квитанций возвращает прежний RequestId; другой payload с тем же ID даёт ошибку. Квитанции ограничены текущим соединением и ёмкостью, а не являются вечной дедупликацией.

Отмена доступна для запросов данного соединения. Отключение клиента прекращает его наблюдение, но не отменяет уже принятую работу ядра. Настройки in_game управляют запросом озвучки сетевого клиента; in_launcher — запросами presenter.

Ограничены клиенты, размер команды, выходные очереди и квитанции. Это минимальный контракт, без аутентификации и совместимости с текущим Unity wire protocol.

## Timeline, персонажи и диагностика

Timeline — событийное хранилище с in-memory/SQLite, ветками, EventId и through. CharacterHandler предоставляет get_context(at); факты фильтруются по знанию персонажа. Состояние, память и история восстанавливаются из одного прочитанного префикса. Старый контекст не применяется к изменившемуся Character молча.

Character владеет description/prompts/state, а живым Character владеет один CharacterHandler. Registry выдаёт handles. Разные персонажи генерируются параллельно, один персонаж — последовательно.

ChatApi::trace(id) возвращает активный или недавно завершённый RequestTrace отдельно от смыслового timeline: input, redacted preset description, контекст, подготовленный текст, переходы и фазы. Число завершённых traces ограничено discovery_buffer. Terminal RequestRecord удаляется; существующие handles сохраняют итог.

## Проверки и ограничения

Локальные тесты не требуют сети/ключа. contracts.rs проверяет настройки, IO/retry, binding, at, семантические обращения, озвучку/частичный результат/отмену, эффекты, TCP test/status/settings, повтор и фрагментированный ввод. vertical_slice.rs проверяет настоящий HTTP-адаптер формата Gemini через локальный сервер.

Ignored live_gemini.rs запускается только с явными NEUROMITA_LIVE_GEMINI_PRESET_FILE и NEUROMITA_LIVE_GEMINI_PRESET_ID. Проверка 2026-11-01 с пресетом Main / gemini-3.6-flash прошла за 1,99 секунды: ответ «Готово.». Переменные HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY были очищены только в тестовом процессе; использовался системный Windows-прокси. Ранний TCP timeout был вызван отсутствием system-proxy в нашей сборке reqwest. Отдельная проверка без ключа согласовала HTTP/2 с тем же хостом. Ключ не выводился, не сохранялся и не изменялся.

SQLite, исторические чтения и ветки реализованы. Slint shell/Home работает как отдельный preview; Sandbox подключён к `SandboxPresenter` и десктопному runtime (отдельные voice/capture/debug функции пока не реализованы). Остальные страницы остаются заглушками. Полное crash recovery запросов, TUI, tools, мультимодальность, стриминг токенов и проверка реального TTS inference остаются незавершёнными. Объединённые ветки и границы проверки описаны в [отчёте интеграции](future/integration-20261103.md).

Проект выделен 2026-11-01 из Atm4x/NeuroMita, commit 7f6b2625d355b1e4e22ed078283df9cfac2468ca. Python/Unity-исходники в этот репозиторий не включены; ссылки на них в документации ведут к исходному снимку на GitHub. Локальный репозиторий использует ветку main; remote пользователь добавит самостоятельно.

## Архитектурные исправления, 2026-11-02

SQLite включён по умолчанию (историческое имя feature: sqlite-system), но собирается из bundled исходников rusqlite/libsqlite3-sys: внешняя sqlite3.lib не требуется. Проверка без SQLite: cargo test --locked --offline --no-default-features; persistent Timeline в таком режиме возвращает явную ошибку.

TTS выполняется до атомарного принятия ответа. Отмена не оставляет сообщения или внутренних эффектов; ошибка TTS допускает VoiceGenerationFailed с принятым рабочим текстом. UI-индикатор/анимация ещё не реализованы. [Границы изменений](future/architecture-fixes-20261102.md).
