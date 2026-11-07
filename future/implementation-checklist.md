# Учёт реализации контрактов Rust

Обновляется по мере работы. Начат 2026-11-01. Рабочий проект: [rust](../README.md). Это новая реализация с нуля. Неотмеченный пункт не считается готовым по одному наличию типов.

С 2026-11-01 этот документ находится в самостоятельном репозитории NeuroMita-Rust. Cargo-проект расположен в корне, архитектурные документы — в future/, аудит Python — в docs/. Раздел «Передача и Git» ниже фиксирует доставку в исходный Atm4x/NeuroMita; новый локальный репозиторий использует main и пока не имеет remote. Исходный снимок: 7f6b2625d355b1e4e22ed078283df9cfac2468ca.

## Уже проверенный первый срез

### Аудиовход и общая геометрия настроек, 2026-11-05

- [x] Нативный каталог CPAL: настоящие устройства, friendly names, endpoint IDs и доступные backends; зависимость в vendor.
- [x] Core-owned AudioInputOwner и функциональный AudioInputPresenter; loopback, RMS/watch, остановка и восстановление распознавания.
- [x] Удалён скрытый десятисекундный clamp; согласованы ограничения сегментов, WAV и Python IPC.
- [x] Высоты строк по содержимому, исходный NumberStepper/QSS, видимые обводки checkbox.
- [x] Общий viewport/колонка скроллбара, исправленные границы контента Game/AI Engine и остальных потребителей контейнера.
- [x] Несжимаемые слоты иконок, естественные ширины вкладок с горизонтальной прокруткой.
- [x] Rust-иконка Home; название пресета вместе с моделью и отдельный аватар/акцентная шестерёнка в Sandbox.
- [x] Физический захват FIFINE K670 проверен без сохранения звука.

Исходники Qt, цвета, связи и ограничения: [аудит](gui/settings-layout-audio-20261105.md). Точные результаты финальных проверок — в HANDOFF.md; этот срез не означает полной готовности установщика речевых моделей и остальных Core-настроек.

### GUI и локализация, 2026-11-03

- [x] Отдельная Slint-оболочка, Home и страницы-заглушки, layout-контейнеры и макет меню лаунчера.
- [x] Общий Rust-модуль i18n(ID, source), сохранение описаний сообщений вместо готовых переводов.
- [x] Импорт 15 исходных JSON-каталогов и русского исходного языка; contextual ID overrides.
- [x] Извлечение деклараций, проверка конфликтов и параметров, экспорт шаблона переводчику.
- [x] Смена языка и revision переводят существующие Slint bindings без пересоздания компонента.
- [x] Явный внешний каталог, добавление языков, атомарный reload и сохранение рабочего состояния при ошибке.
- [x] Общие тесты с GUI: 243 passed, 2 ignored; Clippy/fmt, offline debug build.
- [ ] Сохранение всех LocaleId через core settings и подключение настоящего SettingsPresenter к GUI.
- [ ] Plural forms и локальное форматирование чисел/дат.

Детали: [localization](localization.md), [GUI](gui.md).

- [x] Статический ChatApi, явная инициализация, отсутствие ожидания LLM под lock приёма.
- [x] Ограниченные очереди персонажей и конкурентная генерация разных персонажей.
- [x] RequestId, RequestHandle, активный RequestRegistry, удаление terminal записей.
- [x] Начальный снимок и асинхронное наблюдение без polling.
- [x] Обнаружение presenter'ом обращений всех источников.
- [x] Отмена pending/running и атомарная граница Working/Accepting.
- [x] Независимые владельцы Character; субъективная история из общего Vec timeline.
- [x] Общий провайдерский контракт; типизированный GeminiTemplate отдельно от общего пути.
- [x] Presenter и консольная привязка, локальный HTTP-тест Gemini, обработка паники провайдера.

## Расширение до полного минимального контракта

- [x] Reply/react/initiate: типизированные сообщения, семантические события и проверка знания цели.
- [x] Минимальный at: позиция события, чтение истории и состояния через CharacterHandler.
- [x] Озвучка как порт: GeneratingVoice, результат аудио, ошибка с рабочим текстом, отмена до эффектов.
- [x] Полезный частичный результат при internal error и диагностическая история сборки запроса.
- [x] Типизированные настройки разделов, validation, revision/conflict, наблюдение.
- [x] Общий Migration trait, единая Rust-миграция startup/import, SettingsTransfer, live import/export с CAS и offline CLI; Python stub без переноса. [Контракт](settings-migrations.md).
- [x] Общий MigrationService со Schema/Step/Store, атомарными версиями и возобновлением; StartupMigrations и ApplicationConfig.migrations для независимых модулей до старта Core. [Контракт](migration-system.md).
- [x] SettingsLifecycle, memory/JSON repository, отдельный исход сохранения и flush/shutdown.
- [x] SettingsPresenter и типизированные bindable-свойства без циклов обратной записи.
- [x] CharactersPresenter и полный контракт GuiPresenter.
- [x] Lifecycle как общий примитив запуска/остановки.
- [x] ServerLifecycle: TCP, connected/read, команда test → ChatApi, доставка статусов.
- [x] Сетевые команды настройки/отмена, изоляция клиентов, ограниченные очереди, остановка.
- [x] Согласованная сборка и shutdown всех исполнителей; явные ошибки до init/после stop.
- [x] Сквозные проверки контрактов, конкурентности, эффектов, binding и server lifecycle.
- [x] Живой Gemini с конфигурацией пользователя; ключ не сохраняется и не выводится.
- [x] Системный прокси и HTTP/2 включены; живой запрос проверен без proxy env, HTTP/2 подтверждён отдельной проверкой соединения.
- [x] Актуальная документация запуска, протокола и оставшихся ограничений.

## Передача и Git

- [x] Tokio full и зависимости разрешены в Cargo.lock.
- [x] Vendor исходников зависимостей и .cargo/config.toml; offline-проверка из каталога проекта.
- [x] Отдельный handoff для веб-агента: команды, карта модулей, ограничения, незавершённое.
- [x] Ветка codex/rust-core-contracts создана.
- [x] Проверено содержимое commit: исходники/документы/vendor, без ключей, target и локальных архивов; vendored upstream fixtures сохранены целиком.
- [x] Commit и push в origin выполнены: код/vendor отправлены как e87d392ca327f3e6f50302051458498af0e84bb5; итоговый документационный SHA указывается в отчёте пользователю.

## Правило остановки

Пользователь просит при остатке примерно 8–10% 5-часового лимита зафиксировать и отправить текущее состояние и явно указать незавершённое. Инструмент счётчика найден в процессе работы; остаток контролируется. Не требуется расходовать лимит до порога, если задача уже завершена.

## Launcher (#43)

- [x] Типизированные config/install selections и атомарное JSON-хранение без PID/ownership.
- [x] Статический `LauncherApi` с явными входами и отдельный owned `LauncherService`/`LauncherSession`, без нового глобального runtime.
- [x] Безопасный explicit/known-name discovery с проверкой root, path containment, reparse point и executable signature.
- [x] Read-only status и Windows process identity по пути и creation time; stale/foreign процессы отделены.
- [x] Owned `std::process::Child`: start/stop/status/completion, kill только собственного handle, drop cleanup.
- [x] Direct-argument open-location без shell; тестовая операция через recorder без Explorer.
- [x] CLI `launcher status`/`launcher session` до Gemini initialization; standalone start/stop отклоняются.
- [x] Реальные process integration tests: missing install, stale PID, already running, permission denied, completion, stop, drop, reopen.
- [x] Документация, отклонения и точная матрица критериев: [rust-launcher](../docs/03-runtime/rust-launcher.md).

## Текущее состояние

Дополнительное исправление по исходному Python-протоколу: Gemini использует ?key=..., а не заголовок x-goog-api-key. Добавлена проверка URL-кодирования и отсутствия auth-заголовков в локальном HTTP-сценарии. Ключ не сохраняется в диагностическом URL.

Контракты реализованы и проверены локально, в том числе offline из vendor. При расширении проверок найдена и исправлена гонка старой ошибки сохранения с новым retry, добавлен отдельный регрессионный тест. Минимальные ограничения и дальнейшие задачи перечислены в [minimal-contracts.md](minimal-contracts.md) и [HANDOFF](../HANDOFF.md).

Итог проверки после исправления query-auth Gemini: 9 тестов прошли (7 unit и 2 сквозных сценария), один opt-in live тест по умолчанию ignored. Clippy с -D warnings, форматирование и запуск --help проходят. Проверены 168 vendored пакетов: checksum совпадают, все файлы включены в индекс, локальные target/архивы и пользовательские ключи не включены.

Живой Gemini подтверждён 2026-11-01: Main / gemini-3.6-flash, ответ «Готово.» за 1,99 секунды без HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY. Причина прежнего таймаута — отсутствовавшая feature system-proxy у reqwest при работающем Windows-прокси. Включены system-proxy и http2, обновлены lock/vendor (179 пакетов). Отдельная проверка без ключа согласовала HTTP/2. Девять локальных тестов, Clippy и rustfmt прошли. Ключ не выводился, не сохранялся и не менялся.
