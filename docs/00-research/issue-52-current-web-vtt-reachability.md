# Issue #52: текущая достижимость web/VTT и mock API

**Итог:** в текущем Python-срезе нет работающего пользовательского web/VTT-сценария и нет production-подключения standalone HTTP-сервера. `src/web/server.py` — отдельное FastAPI-приложение, запускаемое только из собственного `__main__`; обычный launcher его не запускает. `/vtt/*` — реальный standalone-код на Vosk, но не часть desktop/headless runtime; `/mock/openai/v1/*` — тестовая поверхность, включаемая переменной окружения и возвращающая заранее заданный JSON. Ни один из этих маршрутов не достижим из GUI, `MainController`, Unity game server или штатного launcher path.

**Рекомендуемое решение:** `RESOLVED AS DEFERRED — STANDALONE/EXPERIMENTAL, НЕ ВХОДИТ В ACTIVE PARITY RUST CORE`. В рамках issue #52 не реализовывать VTT/mock в Rust, не добавлять FastAPI/Vosk/uvicorn в core и не считать эти маршруты пользовательской функцией. Переоткрыть только по отдельному продуктовому требованию с самостоятельным HTTP-контрактом.

**Терминология:** в этом коде `VTT` означает Voice-To-Text (`/vtt/*`), а не virtual tabletop. Отдельного web-клиента или VTT-игрового сервера в репозитории нет.

## Зафиксированная ревизия

| Параметр | Значение |
|---|---|
| Python checkout | `C:\Development\Atm4x_NeuroMita` |
| HEAD | `9f1c66f4178d900e1a898d38bf5808d2fcd7bb86` |
| Branch | `codex/final-integration-2026-10-29` |
| Remote branch | `origin/codex/final-integration-2026-10-29` |
| Dirty state | только untracked `release-notes-2026.11.01.md`; `src/web`, `src/__main__.py`, `requirements.txt`, `scripts/run.py`, `build.py` чистые |
| `src/web` относительно `7f6b2625d355b1e4e22ed078283df9cfac2468ca` | без изменений |

Ревизия проверена read-only. Permalink-ссылки ниже привязаны к `9f1c66f`; это тот же commit, на который указывает `origin/codex/final-integration-2026-10-29`.

## Что именно проверено

Проверялись все слои, которые могли бы сделать HTTP-сервер частью пользовательского runtime:

1. Единственный launcher: `scripts/run.py` запускает `python -S NeuroMita.pyz` без выбора web-режима ([scripts/run.py:816-852](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/scripts/run.py#L816-L852)).
2. `src/__main__.py` имеет только GUI и headless-режимы; headless создаёт `HeadlessRuntimeHost`, а не FastAPI/uvicorn ([src/__main__.py:317-352](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/__main__.py#L317-L352)).
3. `MainController` регистрирует 13 optional features: telegram, audio, local_voice, voice_models, speech, capture, desktop_watch, reminders, embedding, reranker, graph, install, installables. Среди них нет web/VTT ([src/controllers/main_controller.py:353-560](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/controllers/main_controller.py#L353-L560)).
4. `ServerController` создаёт `ChatServerNew` — игровой TCP-сервер с host/port из `NEUROMITA_SERVER_HOST`/`SERVER_PORT`; это не FastAPI и не порт 8000 ([src/controllers/server_controller.py:209-276](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/controllers/server_controller.py#L209-L276), [src/game_connections/server.py:34](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/game_connections/server.py#L34)).
5. `src/web` собирается в `.pyz` как обычные файлы (`build.py` обходит весь `src`), но упаковка файла не создаёт точку запуска ([build.py:430-459](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/build.py#L430-L459)).
6. В `requirements.txt` есть только `uvicorn`; `fastapi`, `vosk` и `python-multipart` не объявлены ([requirements.txt:52](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/requirements.txt#L52), [requirements.txt:79](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/requirements.txt#L79)).
7. В `tests/` нет тестов web/VTT/mock. Упоминание `uvicorn` в `test_installer_windows_resilience.py` — имя wheel в сценарии отказа установщика, не тест HTTP-сервера ([tests/test_installer_windows_resilience.py:112-122](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/tests/test_installer_windows_resilience.py#L112-L122)).
8. В `docs/` и README Python-репозитория нет пользовательской инструкции или маршрута VTT. Упоминания mock HTTP в README относятся к тестам LLM-протокола, не к `/mock/openai/v1`.

### Поиск импортов и вызовов

Команда

```powershell
cd C:\Development\Atm4x_NeuroMita
git -c safe.directory=C:/Development/Atm4x_NeuroMita grep -n -I -E "^(from|import) web(\.|$)" -- "src/**/*.py"
```

возвращает только импорты внутри самого `src/web`: `web.server` импортирует `web.vtt.router`, VTT-модули импортируют друг друга, mock router импортирует mock service. Ни один production-модуль вне `src/web` не импортирует `web.*`.

`git grep -n -I "web.server" -- . ':!*.pyc'` не находит внешних вызовов. `git grep -n -I "NEUROMITA_ENABLE_MOCK_API"` находит только сам `src/web/server.py`. Комментарий в коммите `aff1b98e` прямо фиксирует, что mock router «test-only surface not reachable from the GUI yet»; коммит `7981bb43` добавлял его для проверки ошибок UI без реального провайдера.

## Карта HTTP-поверхности

Ниже перечислены ASGI-маршруты, объявленные в коде. `root_path="/api"` задан в `FastAPI`, но router'ы объявлены без префикса `/api`; публичный префикс зависит от uvicorn/прокси и в репозитории не настроен. Runtime-проверка URL не проводилась.

| Метод и путь | Где объявлен | Что делает | После ручного standalone-запуска | В обычном GUI/headless |
|---|---|---|---|---|
| `GET /` | [server.py:32-34](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/server.py#L32-L34) | возвращает `{"message": "API для локального Voice-To-Text"}` | доступен | недостижим |
| `/docs`, `/redoc`, `/openapi.json` (публичный префикс зависит от `root_path`/прокси) | [server.py:13-19](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/server.py#L13-L19) | автоматическая Swagger/Redoc-документация FastAPI | доступны как dev UI | недостижимы |
| `GET /vtt/models` | [vtt/router.py:13-19](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/router.py#L13-L19) | `os.listdir("web/vtt/models")`; путь относителен текущему каталогу процесса | доступен, если каталог существует; иначе необработанное исключение | недостижим |
| `POST /vtt/transcribe` | [vtt/router.py:21-34](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/router.py#L21-L34) | принимает `model_name` и WAV, создаёт `KaldiService`, возвращает `{text,time_elapsed}` | доступен при наличии модели и зависимостей | недостижим |
| `GET /mock/openai/v1` | [mock_api/router.py:19-34](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/router.py#L19-L34) | статическая информация о mock-маршрутах | только при `NEUROMITA_ENABLE_MOCK_API=1` | недостижим |
| `GET /mock/openai/v1/models` | [mock_api/router.py:37-65](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/router.py#L37-L65) | статический список `mock-gpt-4o-mini`, `mock-deepseek-chat` или ошибка по query | только при флаге | недостижим |
| `POST /mock/openai/v1/chat/completions` | [mock_api/router.py:68-110](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/router.py#L68-L110) | статический OpenAI-подобный JSON; `model` берётся из тела, текст — из query или default | только при флаге | недостижим |

WebSocket-маршрутов нет. В `server.py:39-41` оставлены TODO про будущую передачу текста частями и вынос сервиса в отдельный процесс; это не реализовано ([server.py:39-41](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/server.py#L39-L41)).

Ответ `GET /mock/openai/v1` перечисляет пути без префикса `/api`, хотя приложение объявляет `root_path="/api"`. Какой путь станет публичным, зависит от uvicorn/прокси; это статическое расхождение, runtime-проверка не выполнялась.

Frontend-файлов продукта нет: в Git нет отслеживаемых `.html`, `.js`, `.ts`, `.tsx`, `.jsx`, `.vue`, `.svelte` или QML-ассетов. Единственный браузерный интерфейс — сгенерированные FastAPI Swagger/Redoc. Это не клиент NeuroMita.

## Пользовательский сценарий: что реально может сделать пользователь

### Обычный пользователь приложения

В штатном интерфейсе подтверждённого пути нет. В GUI нет пункта меню, кнопки, настройки, диалога или экрана VTT/mock. В headless-режиме нет аргумента `--web`, `--vtt` или `--mock`; `--server`/`--headless` запускает игровой TCP-сервер, не FastAPI. Launcher и `scripts/run.py` не выбирают `web.server`. Штатная установка не объявляет `fastapi`/`vosk`/`python-multipart`, поэтому даже импорт standalone-приложения не обеспечен requirements.

### Разработчик вручную

Возможен только неподдерживаемый standalone-сценарий:

1. Установить отдельно `fastapi`, `uvicorn`, `vosk`, `python-multipart`, `soundfile` и `loguru` (в `requirements.txt` из них объявлены только `uvicorn` и `soundfile`).
2. Положить Vosk-модель в `src/web/vtt/models/<name>`; каталог моделей Git игнорирует ([src/.gitignore:186-187](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/.gitignore#L186-L187)).
3. Из каталога `src` выполнить `python -m web.server`; приложение поднимет uvicorn на `0.0.0.0:8000` ([server.py:36-37](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/server.py#L36-L37)).
4. Открыть Swagger UI и вызвать `GET /vtt/models`, затем `POST /vtt/transcribe` с `model_name` и mono/16-bit WAV.
5. Для mock дополнительно выставить `NEUROMITA_ENABLE_MOCK_API=1` до импорта `web.server`; mock router монтируется только в этот момент ([server.py:26-30](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/server.py#L26-L30)).

Это developer-only процедура. Она не описана в пользовательской документации, не имеет launcher-команды, не покрыта тестами и не проверялась в этом аудите запуском.

## Auth, доступ, состояние и live generation

| Аспект | `/vtt/*` | `/mock/openai/v1/*` |
|---|---|---|
| Триггер | ручной `python -m web.server` | тот же запуск + `NEUROMITA_ENABLE_MOCK_API=1` |
| Аутентификация | отсутствует; в `src/web` нет auth/security middleware или API-key dependency | отсутствует |
| Сетевой доступ | `0.0.0.0:8000`, без TLS; доступен с других интерфейсов при запуске | тот же процесс и порт |
| CORS | middleware не добавлен | middleware не добавлен |
| Пользователь/сессия | нет; сервис stateless | нет; ответы stateless |
| Состояние | модель загружается заново на каждый `POST`; история/БД не используются | нет; JSON собирается из query и тела |
| Live generation | локальный Vosk inference; LLM не вызывается | нет; текст и usage захардкожены |
| Стриминг | нет; один JSON-ответ | нет; один JSON-ответ |
| Сохранение | нет; аудио читается в память | нет |

### VTT: фактическая реализация

`GET /vtt/models` сканирует `web/vtt/models` через `os.listdir` без обработки отсутствующего каталога ([src/web/utils.py:3-6](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/utils.py#L3-L6)). Путь задан строкой `web/vtt/models`, поэтому зависит от cwd ([src/web/vtt/consts.py:3](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/consts.py#L3)).

`POST /vtt/transcribe` проверяет MIME `audio/wav`, читает весь upload в память, декодирует через `soundfile`, перекодирует в WAV и требует mono/16-bit/uncompressed ([src/web/vtt/dependencies.py:9-32](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/dependencies.py#L9-L32)). Проверки размера загрузки и отдельной containment policy для `model_name` нет: проверяется только `os.path.exists` ([dependencies.py:14-17](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/dependencies.py#L14-L17)). Это статический риск, а не воспроизведённая уязвимость.

`KaldiService` создаёт `vosk.Model` в конструкторе и новый `KaldiRecognizer` на запрос; цикл прерывается на первом `AcceptWaveform(...) == True` и возвращает `rec.Result()`, а не `FinalResult()` всего файла ([src/web/vtt/service.py:9-30](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/service.py#L9-L30), вызов из [router.py:30](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/router.py#L30)). Для длинного WAV это означает частичный результат, а не полную транскрипцию. `time_elapsed` измеряет только обработку запроса.

`/vtt/*` — не mock: это реальный локальный inference-код. Но он не является live ASR desktop pipeline, не использует `ASRHandler`, не связан с персонажами, Unity или настройками и не входит в обычный runtime.

### Mock: намеренно фиктивная поверхность

Mock router не вызывает провайдера и не генерирует текст. `build_openai_models_payload` возвращает фиксированные `mock-gpt-4o-mini`/`mock-deepseek-chat`; `build_openai_chat_payload` возвращает `chatcmpl-mock`, фиксированный `created=1730000000` и фиксированный usage ([src/web/mock_api/service.py:50-96](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/service.py#L50-L96)). Ошибки задаются query `scenario` или `status_code`; это инструмент проверки UI/парсера ошибок, не production endpoint ([service.py:6-31](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/service.py#L6-L31)).

Mock не смонтирован по умолчанию: условие `os.environ.get(..., "0") == "1"` проверяется на import-time. Без флага маршрутов `/mock/*` в приложении нет. Даже с флагом это отдельный ручной процесс, а не настройка NeuroMita.

## Почему поверхность не является active

| Проверка | Результат |
|---|---|
| Production import вне `src/web` | не найден |
| `MainController` feature registration | не найдена; 13 optional features без web |
| GUI action/button/settings | не найдены |
| Startup mode или CLI | только GUI/headless; оба без FastAPI |
| Launcher `scripts/run.py` | только `python -S NeuroMita.pyz` |
| Штатные requirements | `fastapi`, `vosk`, `python-multipart` отсутствуют |
| Тесты | web/VTT/mock тестов нет |
| Пользовательская документация | нет |
| Упаковка | модули попадают в `.pyz`, но не запускаются |
| Внешние вызовы | статически не найдены; внешний оператор не исключён |

Упаковка `src/web` в `.pyz` объясняется тем, что `build.py` копирует все файлы `src`, а не регистрацией сервиса. Наличие `uvicorn` в requirements также не доказывает запуск: `uvicorn` импортируется только из `src/web/server.py`, а сам модуль нигде не импортируется.

## Пределы вывода

* Это статический аудит. Сервер не запускался, HTTP-запросы не выполнялись, Vosk-модель не загружалась, аудио не декодировалось.
* Не проверялись пользовательские каталоги моделей, `Settings`, `Histories`, БД, логи и credentials. Содержимое `src/web/vtt/models` намеренно не перечислялось.
* Нельзя статически исключить внешнего оператора, который вручную запустит `python -m web.server` или добавит модуль в `sys.path`. Это не делает поверхность частью штатного runtime.
* Точный публичный URL при `root_path="/api"` зависит от uvicorn/прокси; в репозитории обратный прокси не настроен. Runtime-проверка `root_path` не выполнялась.
* Риски отсутствия auth, `0.0.0.0`, отсутствия лимита upload и path-existence проверки являются статическими наблюдениями. Эксплуатация не проверялась.

Уверенность: **высокая** в отсутствии production import/registration/GUI/test; **средняя** в том, что внешние callers за пределами репозитория отсутствуют; **нулевая runtime-уверенность** в поведении endpoint'ов, поскольку они не исполнялись.

## Маппинг критериев issue #52

| Критерий | Статус | Доказательство |
|---|---|---|
| Проверить, существует ли работающий standalone web/VTT user scenario или mock | `[x]` | standalone server есть; пользовательского сценария нет; mock отделён |
| Не смешивать mock endpoints с production | `[x]` | `/vtt/*` — реальный standalone Vosk; `/mock/openai/v1/*` — фиктивный JSON за флагом |
| Описать user actions/auth/access/state/live generation, если surface active | `[x]` | surface не active; для standalone описаны ручные действия, отсутствие auth, stateless и Vosk без LLM |
| Проверить production entrypoints и transitive imports | `[x]` | `src/__main__.py`, `scripts/run.py`, `MainController`, `ServerController`, grep импортов |
| Проверить callers/config/feature registration | `[x]` | 13 features без web; env-флаг только внутри `server.py` |
| Проверить docs/tests | `[x]` | нет VTT-документации и тестов; installer-тест с `uvicorn` нерелевантен |
| Перечислить точные frontend/backend routes | `[x]` | таблица маршрутов; frontend отсутствует, есть только Swagger/Redoc |
| Проверить user flow vs stubs | `[x]` | обычный пользователь не имеет flow; VTT требует ручного запуска; mock — stub |
| Acceptance: real caller scenarios+behavior checks **или** deferred/research with evidence | `[x]` | выбран deferred/research; evidence выше; runtime не выдаётся за проверенный |
| Указать confidence/limits | `[x]` | раздел выше |
| Дать exact recommended disposition | `[x]` | раздел ниже |

## Architecture findings и границы будущего решения

Сейчас в Rust Core не нужны ни HTTP-порт, ни Vosk-адаптер, ни mock-провайдер. Игровой TCP-сервер и standalone HTTP-сервер — разные контракты; их нельзя объединять по имени `server`.

Если позже появится явное продуктовое требование к standalone VTT, полный scope должен быть отдельным: opt-in процесс, явный bind-address и auth/exposure policy, провижининг моделей, ограничение размера и формата upload, полная транскрипция файла с корректным завершением, таймауты/отмена, health/наблюдаемость и отдельный версионированный HTTP-контракт. Mock-сценарии должны оставаться тестовым адаптером, выключенным по умолчанию, и не попадать в production-конфигурацию. Перенос Vosk inference в Rust Core не обоснован текущими доказательствами.

**Точное решение:** `STANDALONE / EXPERIMENTAL`; normal-runtime reachability — `NOT PRESENT`; disposition — `DEFERRED / EXCLUDED FROM ACTIVE PARITY`. Это закрывает исследовательский вопрос issue #52 без реализации и без молчаливого объявления функции частью Core.

## Воспроизводимые статические команды

Выполнять из PowerShell; команды только читают Git и файлы.

```powershell
$py = "C:\Development\Atm4x_NeuroMita"
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py rev-parse HEAD
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py rev-parse --abbrev-ref HEAD
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py status --short --untracked-files=all
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py status --short --untracked-files=all -- src/web src/__main__.py requirements.txt scripts/run.py build.py

git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py grep -n -I -E "^(from|import) web(\.|$)" -- "src/**/*.py"
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py grep -n -I "web.server" -- . ':!*.pyc'
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py grep -n -I "NEUROMITA_ENABLE_MOCK_API" -- . ':!*.pyc'
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py grep -n -I -i "vtt|voice-to-text|transcrib|mock openai|fastapi|uvicorn" -- tests docs README.md README_EN.md
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py ls-files src/web/vtt/models
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py ls-files | Select-String -Pattern '\.(html|js|ts|tsx|jsx|vue|svelte|qml)$'

git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py diff --stat 7f6b2625d355b1e4e22ed078283df9cfac2468ca..9f1c66f4178d900e1a898d38bf5808d2fcd7bb86 -- src/web
git -c safe.directory=C:/Development/Atm4x_NeuroMita -C $py log --oneline -6 -- src/web
```

## Источники

* [src/web/server.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/server.py) — app, `root_path`, docs, optional mock, `__main__`/uvicorn.
* [src/web/vtt/router.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/router.py) — `/vtt/models`, `/vtt/transcribe`.
* [src/web/vtt/dependencies.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/dependencies.py) — MIME, model path, upload и WAV checks.
* [src/web/vtt/service.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/service.py) — Vosk model/recognizer и ранний break.
* [src/web/vtt/consts.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/consts.py), [schemas.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/schemas.py), [exceptions.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/vtt/exceptions.py), [utils.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/utils.py).
* [src/web/mock_api/router.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/router.py) и [service.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/web/mock_api/service.py) — фиктивные OpenAI-совместимые ответы.
* [src/__main__.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/__main__.py), [src/controllers/main_controller.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/controllers/main_controller.py), [src/controllers/server_controller.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/controllers/server_controller.py), [scripts/run.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/scripts/run.py), [build.py](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/build.py).
* [requirements.txt](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/requirements.txt) и [src/.gitignore](https://github.com/Atm4x/NeuroMita/blob/9f1c66f4178d900e1a898d38bf5808d2fcd7bb86/src/.gitignore).
