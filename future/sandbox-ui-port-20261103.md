# Sandbox: перенос Python Qt Widgets → Rust Slint (2026-11-03)

## Импорт архива и исправления на Windows

Последующее исправление: удалён добавленный вне Python-макета блок подключения модели. Запуск генерации остаётся через явный `--model MODEL` и `GEMINI_API_KEY`; полноценный UI конфигурации провайдера ещё не реализован. Уведомление центрирует кнопку закрытия. Во всех трёх вкладках инспектора ширина содержимого считается по `visible-width`, то есть без области вертикального скроллбара, и есть правый отступ 10px. В render test добавлен снимок отключённого ядра с уведомлением.

Коммит архива `535c94c2` импортирован через Git bundle поверх `9011649c` в `codex/sandbox-presenter-20261103`, с сохранением истории. Исходники из `modified-source/` соответствуют этому коммиту; архив и временная распаковка не добавлены в репозиторий.

- Placeholder использует координаты, размеры, шрифт и вертикальное выравнивание редактора. Общий отступ 4px сверху/снизу оставляет одну строку по центру composer; исчезает сдвиг между подсказкой и вводимым текстом.
- Селектор персонажа использует общий `Avatar` вместо прямоугольного `Image`. Матовая маска полностью непрозрачна, поэтому через углы не просвечивает квадрат исходного PNG даже на полупрозрачной поверхности.
- В render test после проверки сохранности draft поле очищается: снимки теперь проверяют именно видимый placeholder. Проверены размеры 1560×918 и 1200×760, вкладки и свёрнутый инспектор; это software rendering, а не нативный GPU capture.
- После дополнительного замечания выравнивание исправлено по всей странице: `Glyph` занимает высоту строки и центрирует внутренний текст, а `Centered` задаёт вертикальное центрирование кнопок composer, аватара, BETA, кнопок заголовка/сворачивания, плашек, переключателей и маркеров capture/debug. Фиксированные размеры больше не прижимают элементы к верху HorizontalLayout. Повторены GUI render test, Clippy с GUI и debug GUI build.

Проверки Windows: полные locked/offline тесты — 324 passed с GUI, 323 passed без GUI (по 2 ignored); Clippy для обеих конфигураций с `--all-targets -- -D warnings`, rustfmt и debug GUI build. После окончательного выравнивания повторён GUI render test. Для текущего Clippy 1.99 сохранение тестовых PPM переведено с `chunks_exact(4)` на `as_chunks::<4>()`. Живых вызовов Gemini и скачивания моделей этот импорт не выполняет.

## Проверенные исходники

- Rust: `codex/sandbox-presenter-20261103`, HEAD `9011649cf6b46e66712e2ef6fc2b2fff53f37917`.
- Python: `session-timeline`, HEAD `5f3d061f523d9abefa6a264bb2953048a935fa65`; получен через временный Git Bridge Actions export, а не прямым git clone GitHub из Linux.
- Использованы `src/styles/theme.py`, `src/styles/base.py`, `src/styles/pages/sandbox.py`, `src/ui/pages/sandbox_page.py`, `src/ui/widgets/chat_panel.py`, `src/ui/chat/chat_widget.py`, `src/ui/chat/message_widget.py`, `src/ui/widgets/toggle_switch.py`, `src/ui/widgets/character_state_panel.py` и `future/gui-reference/03-sandbox.png`.
- Историческое описание исходного состояния и точные измерения сохранены в `future/sandbox-checkpoint.md`.

## Перенесённое представление

- `ui/pages/sandbox.slint` — композиция Sandbox: заголовок/кнопки, chat host, inspector, ошибочный toast, строго типизированные Slint properties/callbacks.
- `ui/sandbox/chat.slint` — conversation header, scrollable message list, multiline composer, вложенные поверхности действий и текущие статусы ответа.
- `ui/sandbox/inspector.slint` — Session/State/Debug, селектор персонажа, статусы, visual capture controls, quick actions, поля состояния и memory, сворачиваемый rail 420 ↔ 60 logical px.
- `ui/sandbox/message.slint` — bubble с геометрией хвоста, 12px radius, оригинальными размерами шрифта/аватара. Ширина ограничена 600px и рассчитывается из доступной ширины строки; при 1200px применяется минимальная ширина 240px для предотвращения переполнения текста.
- `localization/catalogs/en.json` — убраны stale ID удалённого грубого Slint-макета и сопоставлены новые декларации с английскими переводами.
- `src/bin/neuromita-gui/binding_tests.rs` — UI render test с переключением вкладок, режимом свернутой панели, окнами 1560×918 и 1200×760.

## Подключения и границы

Сохранены исходные `SandboxPresenter`, runtime, `src/bin/neuromita-gui/sandbox.rs`. UI вызывает `send`, `cancel`, `select-character`, `refresh`, `settings` и `dismiss-error` callbacks. Добавленный ранее model selector удалён по замечанию пользователя: настройка провайдера должна быть отдельной согласованной функцией, а не дополнением макета Sandbox. Без `--model` открывается оболочка; для запуска Core/генерации сейчас нужны явный `--model MODEL` и `GEMINI_API_KEY`. История — проекция presenter, не локальная каноническая копия; provisional messages и cancellation зависят от реального запроса. Шестерёнки лишь переключают на ещё не реализованную страницу настроек. Статусы game/microphone/RAG и capture controls пока являются макетами, не наблюдением соответствующих подсистем.

Неподдерживаемые backend-операции **не имитируются**: history browser / clear, microphone, screen/camera capture, memory details/DB, reset character, debug snapshot/system insertion/last-context. Они показаны в PyQt-структуре, но не выдают фиктивного успешного результата; для полноценной функциональности нужны самостоятельные контракты/сервисы. Доступные числа состояния идут из presenter, неизвестные счётчики памяти отображаются `—`.

Это реализация всей компоновки **страницы**, но не порт всех независимых Python runtime-функций. Pixel-perfect автоматическое дифференциальное сравнение с PyQt не проводилось; отладка проводилась по исходным константам и реальному software render. Скриншоты получены из headless Slint test fixture, не из живого Gemini/Unity runtime. Native Winit GPU/window capture не выполнялся.

## Верификация

Rust 1.98.1 (supplied offline toolchain), Linux, vendored crates:

- `cargo check --locked --offline --features gui --bin neuromita-gui` — PASS.
- `cargo clippy --locked --offline --features gui --bin neuromita-gui -- -D warnings` — PASS.
- `cargo fmt --all --check` — PASS.
- `cargo test --locked --offline --features gui --bin neuromita-gui existing_slint_bindings_retranslate_on_locale_change_and_catalog_revision` — PASS; сохраняет Session, State, Debug, collapsed и compact screenshots при `NEUROMITA_GUI_CAPTURE_DIR`.
- `cargo test --locked --offline --test localization` — 9/9 PASS.
- `cargo test --locked --offline --features gui --test vertical_slice sandbox_reads` — 1/1 PASS (pending/cancel/history/character isolation/provider local-fixture).

Оставшиеся отдельные задачи: feature-команды без backend, полная история/аттачменты/отображение времени, нативная Windows проверка Segoe UI/DPI и автоматическое image diff по установленному PyQt.
