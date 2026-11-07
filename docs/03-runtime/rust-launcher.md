# Rust launcher: установка, владение процессом и CLI

**Issue:** #43 Launcher. **Статус:** реализовано в ветке `codex/issue-43-launcher`; commit/push не выполнялись.
**Исходный Python:** `C:\Development\Atm4x_NeuroMita`, commit `9f1c66f4178d900e1a898d38bf5808d2fcd7bb86`. Файлы `src/controllers/gui/home_page_controller.py`, `src/controllers/gui/home_page_view_model.py`, `src/core/unity_installation.py` и `src/services/unity_process_guard.py` на этом SHA чистые по `git status --porcelain`.

## Что проверено в Python

Закреплённые callsites на SHA `9f1c66f4178d900e1a898d38bf5808d2fcd7bb86`:

| Python callsite | Проверенное поведение |
|---|---|
| `HomePageController.__init__`, `_recover_process_marker`, `refresh_process_state` | QProcess state, marker PID/executable/create_time и попытка привязаться к уже запущенному процессу |
| `HomePageController.launch_unity` | Проверка running/starting/stopping, поиск executable и старт через QProcess |
| `HomePageController.stop_unity` | terminate, отложенный kill и повторная проверка identity |
| `HomePageController.open_unity_folder` | Создание каталога и запуск `explorer` с путём |
| `core.unity_installation.unity_install_dir`, `find_unity_executable` | Config/base-directory выбор и поиск известных имён, затем первого `.exe` |
| `HomePageViewModel._primary_action`, `_run_primary_action`, `_stop_unity`, `_open_unity_folder` | User intents primary button и отдельные операции процесса |
| `services.unity_process_guard.stop_unity_for_update` | Отдельная graceful/forced политика закрытия перед заменой установки, не обычный Stop |

`unity_installation.unity_install_dir` выбирает явно заданный каталог, `NEUROMITA_BASE_DIR` или каталог executable. `find_unity_executable` ищет известные имена в root и первом уровне, затем совпадение по имени и, наконец, первый `.exe`. `unity_process_guard` — отдельная политика закрытия игры перед заменой установки; она не используется как обычная кнопка Stop.

## Решение Rust

Реализация разделена на чистые слои без статического service locator:

| Слой | Ответственность |
|---|---|
| `LauncherConfig`, `ConfigStore` | Явный JSON schema 1: `schema_version`, `install_root`, необязательный `candidate`. Атомарная запись, явная ошибка отсутствующего/неверного файла |
| `LauncherApi` | Статический фасад с явными typed inputs: discovery/status, open-location, inspect identity, создание сессии. Глобального runtime slot нет |
| `LauncherService` | Owned config/opener service; строит read-only snapshot или передаёт себя единственной `LauncherSession` |
| `InstallSelection` | Канонический root и проверенный executable; отсутствие установки выражается ошибкой |
| `LauncherSession` | Единственный владелец реального `std::process::Child`; start/status/stop, завершение и очистка |
| `ProcessIdentity`, `ProcessScan` | Windows ToolHelp + `QueryFullProcessImageNameW` + `GetProcessTimes`; путь и время создания, без PID-only доверия |
| `cli` | `launcher status` и интерактивный `launcher session`; не требуют Gemini credentials |

`LauncherApi` не хранит config и не использует service locator: `LauncherApi::status(&config)`, `LauncherApi::open_location(&config, &opener)` и `LauncherApi::session(config)` получают явные typed inputs. `LauncherService` — owned слой для config/opener и единственный источник `LauncherSession`; `LauncherService::with_opener` предназначен для контролируемой проверки open-location.

`status` только читает установку и процессы. Существующая установка не запускается автоматически. `start` сначала проверяет owned child, затем точный путь установки; уже запущенный executable того же пути блокирует старт как `AlreadyRunningExternally`. Одноимённый процесс из другого каталога показывается как `same_name_foreign`, не становится owned и никогда не останавливается.

`stop` работает только с сохранённым `Child`: kill и wait. Он не ищет PID, не сканирует имя и не останавливает внешний процесс. Успешный stop очищает identity и возвращает `Finished { exit_code, requested_stop: true }`. Гонка, в которой процесс завершился между refresh и kill, распознаётся через `try_wait` и возвращает фактическое завершение, а не ложную ошибку. `Drop` напрямую kill/reap-ит owned handle, даже если обычный status refresh уже недоступен.

Закрытие интерактивной сессии завершает owned child. Сессия не переживает выход CLI и не выдаёт себя за daemon. Новый процесс launcher не наследует ownership, не читает PID marker и может только сообщить о наблюдаемом внешнем процессе.

## Discovery и безопасность

Discovery требует существующий explicit/config root. Без явного `candidate` автоматически принимаются только `NeuroMita.exe`, затем `NeuroMita-Unity.exe` в root или первом уровне. Первый `.exe` fallback удалён. `Unity.exe` не принимается автоматически, потому что это может быть editor/installer; он допустим только как explicit candidate.

Candidate должен быть обычным файлом внутри канонического root, не symlink/reparse point, с расширением `.exe` и сигнатурой `MZ` на Windows. Несколько одинаковых известных имён дают `AmbiguousCandidates`. Ошибки доступа к root, каталогу или executable возвращаются как `AccessDenied`, а не превращаются в «не установлено».

Open-location требует существующий root и запускает `explorer.exe` с каталогом отдельным аргументом. Shell/string command не используется, поэтому пробелы, `&` и кавычки в пути не интерпретируются. Тесты применяют только внедрённый recorder и не открывают видимый Explorer.

## CLI

```text
neuromita launcher status --install-root DIR [--candidate FILE] [--config FILE]
neuromita launcher session --install-root DIR [--candidate FILE] [--config FILE]
```

`status` read-only и не сохраняет config. `session` принимает `status`, `start`, `stop`, `open-location`, `help`, `quit`. `--config` загружает сохранённый выбор; при отсутствии файла требуется `--install-root`, после чего session сохраняет выбор. Отдельные `launcher start`/`stop` намеренно отклоняются: одиночный вызов не может честно удерживать `Child` после выхода процесса.

Сохраняются только config/install selections. PID, creation time, running state и ownership не сериализуются.

### User-facing path

| Действие пользователя | Команда | Наблюдаемый результат |
|---|---|---|
| Проверить выбранную установку | `neuromita launcher status --install-root DIR` | Root/executable или явная ошибка; внешний exact-path процесс виден в scan |
| Запустить игру | `launcher session`, затем `start` | `Started`/owned pid или `AlreadyRunning`/`AlreadyRunningExternally`/`AccessDenied` |
| Проверить ownership | `launcher session`, затем `status` | Owned state, install scan и same-name foreign процессы |
| Остановить игру | `launcher session`, затем `stop` | `Finished` с exit code; внешний процесс не останавливается |
| Открыть каталог | `launcher session`, затем `open-location` | Direct-argument запуск Explorer; в тестах recorder |
| Выйти | `quit` или EOF | Owned child kill/reap, затем завершение CLI |

`launcher status` не требует `GEMINI_API_KEY`. `launcher start`/`launcher stop` без session возвращают ошибку с указанием использовать `launcher session`.

## Соответствие критериям issue

| Критерий | Реализация | Проверка |
|---|---|---|
| Checked start | `LauncherSession::start_with`, проверка install/owned/external | `owned_helper_completes_with_its_real_exit_code` |
| Checked stop | owned `Child::kill` + `wait`, без PID/name | `already_running_is_rejected_and_stop_completes_the_owned_child` |
| Open location | `SystemLocationOpener`, direct argument, injectable trait | `open_location_is_explicit_and_can_be_reopened_without_explorer_in_tests`; unit-тест command |
| Access failure | spawn lock возвращает `AccessDenied` | `permission_denied_start_is_reported_without_owning_a_child` |
| Owned completion | `Finished` с фактическим exit code; natural и requested stop | `owned_helper_completes_with_its_real_exit_code`, stop test |
| Stale PID | path + creation time; несовпадение identity = `Stale` | `stale_identity_and_real_foreign_same_name_process_are_never_owned` |
| Same-name foreign | отдельный scan result, не owned, не kill | тот же тест; внешний child остаётся жив |
| Missing install | `MissingInstall` для status/start/open | `missing_install_is_reported_by_status_start_and_open_location` |
| Already running | owned `AlreadyRunning`, внешний exact path `AlreadyRunningExternally` | stop test, `exact_install_running_outside_the_session_blocks_start_and_is_not_owned` |
| Permission denied | реальная Windows share lock, не mock | permission test |
| Reopen | config reload без ownership; повторная атомарная запись существующего config; повторный open-location | config test, open test |
| CLI без Gemini | dispatch до `GEMINI_API_KEY`, оба CLI сценария | два CLI-теста |
| Safe discovery | explicit root, known names priority, verified candidate | `discovery_rejects_unverified_candidates` |
| No auto-run | fresh session `Idle`; read-only status | config/CLI reopen tests |
| No static locator | статический `LauncherApi` с явными входами, owned `LauncherService`/`LauncherSession`, явный opener | API и все тесты |

## Тесты и ограничения

`tests/launcher.rs`: 13 объявленных тестов, 12 выполняются, 1 helper помечен `ignored` и запускается только как реальный дочерний процесс. Helper — копия тестового executable с `launcher_helper_process`; реальная игра не запускается. Покрыты natural completion, stop, drop cleanup, exact external process, foreign same-name, stale identity, lock permission, config reopen, CLI status/session.

Осознанные отклонения и границы текущей реализации:

- GUI, update/download/installer, EULA, restart core, primary-action composition и Unity connection этой веткой не реализованы и не заявляются готовыми. По актуальному issue body updates/EULA не требуются; принятый объём — discovery/selection существующей установки, start/stop/open-location, access failures и ownership.
- Обычный stop является немедленным `Child::kill`; отдельная graceful terminate-политика Python/QProcess и updater guard не перенесены.
- Нет PID marker и восстановления ownership после рестарта CLI: новый процесс не может остановить старую игру, но `launcher status` сканирует точный путь и показывает её как внешний процесс.
- Open-location не создаёт отсутствующий каталог.
- Windows scanner использует path и creation time; Unix fallback сравнивает путь и `starttime` из `/proc/<pid>/stat`.
- `--candidate` принимает другой executable только как явный выбор; автоматический поиск `Unity.exe` и первый `.exe` намеренно отсутствуют.

## Проверки

Windows MSVC, Rust 1.99.1, общий `CARGO_TARGET_DIR=C:\Development\NeuromitaRust\target`. Независимо выполненные обязательные проверки прошли:

- `cargo test --locked --offline`: **23 passed, 2 ignored**. Ignored: harmless launcher helper и opt-in live Gemini.
- `cargo clippy --locked --offline --all-targets -- -D warnings`: PASS.
- `cargo fmt --all -- --check`: PASS.
- `git diff --check`: PASS.

Linux-код не исполнялся на этом Windows host; `/proc` starttime fallback проверен статически, не отдельной Linux-сборкой. Команды воспроизведения:

```powershell
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```
