# Rust Core: внешний TCP-клиент

Проверяемая область [#53](https://github.com/Atm4x/neuromita-rust/issues/53) — существующий Rust JSON-lines endpoint: отправка сообщения, наблюдение результата, отмена своего запроса и настройки громкости. Это публичная граница минимального Core, отдельная от presenter. Listener принадлежит `ServerLifecycle`, запросы исполняет обычный Core через привязанные к экземпляру API. Сервер не владеет историей или состоянием персонажа и не исполняет генерацию самостоятельно.

## Подтверждённая референсная ревизия

2026-11-02 прочитаны `src/services/runtime_ipc_service.py`, `src/core/runtime_ipc.py`, регистрация в `src/controllers/main_controller.py` и `ai.restart_service` в `src/controllers/ai_engine_controller.py` из локального Python checkout `Atm4x_NeuroMita`, commit `9f1c66f4178d900e1a898d38bf5808d2fcd7bb86`. Эти четыре файла не имели локальных изменений. Python endpoint не запускался; это проверка исходников и production-регистрации, не live-проверка пользовательского клиента.

В Python подтверждены `ipc.ping`, `ipc.status`, `ipc.methods`, `hardware.snapshot`, `ai.topology`, `ai.restart_service`. Они зарегистрированы явно; listener включается настройкой и привязывается к loopback. Rust предоставляет собственные `ping`, `methods`, `server_status` для текущего Core, но не реализует Python wire format: аппаратный inventory и AI administration ещё отсутствуют. Их перенос, live-переключение порта и UI enable/disable остаются отдельной функциональной работой. Поддержка текущего Unity wire protocol также не заявлена. Сравнение с аудитом: [Python local control](local-control-ipc.md), [Unity transport](unity-transport.md).

## Соединение и команды

Адрес выбирается явно в `ServerConfig`; CLI принимает `--listen`. LocalControl принимает только loopback-адреса (`127.0.0.1` или `::1`); запуск на других адресах отклоняется до открытия listener. Endpoint не имеет TLS или аутентификации: локальный доступ даёт доступ к разрешённым командам и общим настройкам. Изоляция отмены привязана к соединению, а не к авторизованному пользователю.

После подключения сервер отправляет `connected` с новым `client_id` и первоначальный `settings` snapshot. Команды и ответы — UTF-8 JSON, один кадр заканчивается `\n`; принимается CRLF. Частичный TCP read не является концом команды. Нельзя полагаться на соответствие одного `write` одному `read`: несколько строк могут прийти вместе, одна строка — несколькими фрагментами.

Каждая JSON-команда содержит UUID `command_id`. Для нового намерения используйте новый UUID.

| Команда | Поля | Результат |
| --- | --- | --- |
| `ping` | без аргументов | `pong` с command_id; подтверждает работу endpoint, не готовность inference |
| `methods` | без аргументов | каталог тринадцати поддерживаемых команд |
| `server_status` | без аргументов | диагностика listener, клиентов и настроенных пределов |
| `publish_message` | event_id, occurred_at_unix_ms, speaker, content и audience | published с event_id, без генерации |
| `reply` | character, message_id и audience | accepted с request_id; цель — уже опубликованное известное сообщение |
| `react` | character, event_id, instruction и audience | генерация реакции на известное событие |
| `initiate` | character, instruction и audience | генерация без целевого события |
| `cancel` | `request_id` | результат отмены своего запроса либо отказ |
| `diagnostics` | `character`, `request_id`, optional `section` | диагностическая запись исходного запроса с проверкой источника |
| `diagnostics_recent` | `character`, optional `limit` | последние запросы, видимые этому клиенту |
| `diagnostics_errors` | `character`, optional `limit` | последние ошибки, видимые этому клиенту |
| `settings` | без аргументов | snapshot с revision |
| `set_volume` | `value`: целое 0..100 | `settings_applied` либо отказ |

Для smoke-тестирования используется типизированный `initiate` с явным `character`; для ответа на сообщение — `publish_message` и `reply`.

`accepted` подтверждает приём Core, не успешную генерацию. Далее приходят `request_status`: pending, processing, finished или cancelled. Snapshot может сразу оказаться terminal; поток не обещает доставку каждой промежуточной revision. Finished различает success, text_error, voice_error и internal_error. Voice error сохраняет принятый текст. Окончание генерации и принятие эффектов принадлежат Core; передача статуса по TCP не означает подтверждение его обработки клиентом.

## Повторы, ошибки и границы

Квитанции `publish_message`/`reply`/`react`/`initiate` живут в ограниченном окне текущего соединения. Одинаковые `command_id` и содержимое возвращают прежний результат публикации либо прежний RequestId и его текущий статус, включая terminal. Другое содержимое с удерживаемым ID отвергается. После вытеснения квитанции повтор может стать новым запросом. Это не durable exactly-once и не дедупликация между reconnect/restart. Команды settings/cancel не получают квитанций генерации.

Отмена возможна лишь через удерживаемую квитанцию данного соединения. Чужой RequestId и RequestId прошлого соединения не дают права отмены. Разрыв соединения завершает его writer/observers, но принятая Core работа продолжает исполняться. При переподключении выдаётся новый client_id; автоматического восстановления подписок нет.

Пределы задаёт ServerConfig: max_clients, outbound_capacity, max_command_bytes и receipt_capacity. Размер кадра считается в байтах вместе с завершающим newline (и CR при CRLF). При заполнении очереди персонажа Core отказывает в приёме. Переполнение учёта запросов также даёт отказ; переполненный listener закрывает лишнее подключение. Ограниченный outbound применяет backpressure, без гарантированного срока доставки медленному клиенту. Idle/read deadline не задан; shutdown должен прерывать в том числе idle и незавершённый ввод.

Некорректная JSON-команда отвергается; следующее корректное сообщение в соединении может быть обработано. Неизвестные и дублирующиеся поля отвергаются при типизированном чтении исходной строки. Нарушения framing (превышение размера, не-UTF-8, EOF посреди кадра) завершают соединение без гарантированного error-кадра. Ошибка такого клиента не останавливает listener или Core.

### Correlation и машинные ошибки

Все явные ответы `pong`, `methods`, `server_status`, `settings`, `settings_applied`, `cancel`, `accepted` содержат исходный UUID `command_id`. Первоначальный и последующие unsolicited `settings` snapshots не имеют command_id; они могут перемежаться с ответами. `request_status` коррелируется по request_id. Для вновь принятого запроса `accepted` ставится в outbound раньше его статусов; при replay `accepted` предшествует текущему snapshot. Промежуточные revisions не гарантируются. Control-команды не получают дедупликации: повтор `set_volume`/`cancel` исполняется заново, если ID не конфликтует с удерживаемой квитанцией генерации.

Форма отказа: `{"type":"error","code":"invalid_command","error":"описание","command_id":"UUID"}`. Строка error сохранена для человека; клиент должен выбирать поведение по code. ID возвращается, если исходная строка — корректный JSON с единственным валидным UUID command_id; при синтаксической ошибке, отсутствующем/невалидном/дублированном ID correlation опускается. Value-разбор используется лишь для классификации неизвестной команды, а не для приёма: он не может скрыть дублирующиеся поля исходного Envelope.

| code | Значение |
| --- | --- |
| `malformed_json` | JSON не разбирается: syntax/EOF/trailing content |
| `unknown_command` | строковое имя команды отсутствует в текущем каталоге |
| `invalid_command` | структура, типы, обязательные/лишние/дублирующиеся поля или десериализация значения (включая Volume вне 0..100) |
| `invalid_value` | типизированная команда отклонена проверкой Core input/settings, например пустой текст |
| `command_id_conflict` | удерживаемый command_id уже принят с другим намерением |
| `queue_full` | очередь персонажа не приняла запрос |
| `receipt_capacity` | все квитанции заняты незавершёнными запросами |
| `ownership_denied` | request_id не принадлежит удерживаемой квитанции этого соединения |
| `core_unavailable` | Core/settings остановлены, очередь закрыта или персонаж недоступен |
| `settings_conflict` | отказ по revision настроек |
| `settings_storage` | ошибка хранилища настроек |

Последние два кода отображают типизированные ошибки settings; текущие команды не предоставляют CAS/flush и не обещают, что эти ветви достижимы обычным клиентом. Исходы генерации остаются в существующих полях request_status (`outcome`, `error`), а не превращаются в отказ команды.

### Discovery и диагностика

`methods` возвращает `{"type":"methods","command_id":"UUID","methods":["ping","methods","server_status","publish_message","reply","react","initiate","cancel","diagnostics","diagnostics_recent","diagnostics_errors","settings","set_volume"]}`. Каталог и типизированные варианты Command объявлены совместно; dispatch проверяется исчерпывающим match. Это фиксированный контракт endpoint, без reflection и динамического поиска сервисов.

`server_status` возвращает type/command_id и поля `address` (фактически привязанный адрес, включая назначенный ОС порт), `running`, `connected_clients`, `accepted_connections`, `has_error`, `limits`. Limits содержит max_clients, outbound_capacity, max_command_bytes, receipt_capacity. Connected_clients включает запрашивающее соединение; accepted_connections считает только допущенные соединения за жизнь listener, а не отвергнутые сверх лимита. Has_error означает сохранённую ошибку клиентской задачи; флаг не сбрасывается успешной командой. Raw last_error не передаётся по сети, чтобы не раскрывать внутренние сообщения или credentials. Снимок может измениться сразу после чтения; running описывает listener, не готовность модели, провайдера, GPU или Python-сервисов.

## Воспроизводимая проверка

Тесты используют настоящий loopback TCP и управляемый GenerationProvider. Они проверяют transport и Core в одном процессе без внешнего AI-сервиса, GPU, ключей или Python-worker. Проверки нельзя считать live Unity/Python parity.

| Контрактный сценарий в `tests/local_control.rs` | Проверяемый результат |
| --- | --- |
| Discovery и correlation | все объявленные команды исполняются; pipeline control-ответов отличается от unsolicited settings; accepted предшествует terminal и replay |
| Локальная граница доступа | любые не-loopback адреса отвергаются при bind; тестовые команды отсутствуют в production catalog |
| Машинные ошибки | стабильные codes, recoverable command_id, strict duplicate fields, отсутствие мутации и восстановление после отказа |
| Диагностика | фактический порт, допуск/уход клиентов, пределы и sanitised error flag без утверждения inference readiness |
| Некорректные команды и восстановление соединения | JSON/types/unknown fields/volume bounds отклоняются без изменений, следующая команда работает |
| Fragmented UTF-8 и несколько строк за один write | символ не повреждается, команды не теряются, settings доставляются во время частичного ввода |
| Квитанции и terminal replay | нет второй генерации; конфликт намерения отклоняется; вытесняется завершённая запись; отказ не удаляет старую квитанцию |
| Отмена и reconnect | чужой запрос не отменяется; принятый запрос завершается после disconnect; reconnect создаёт отдельный command scope |
| Очередь персонажа заполнена | явный отказ без регистрации нового запроса; тот же command_id принимается при повторе после освобождения очереди |
| Лимит соединений | лишнее соединение закрывается, текущий клиент работает, освобождённый слот используется повторно |
| Ошибки framing и shutdown | точный лимит принимается; oversized/non-UTF-8/partial EOF закрываются; idle и partial read не задерживают остановку |

```powershell
cargo test --locked --offline --test local_control
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```

Существующий `tests/contracts.rs` также проверяет presenter discovery сетевых запросов, статусы, повтор и фрагментированный ввод во время обновления настроек. `tests/restart.rs` проверяет недоступность старых API handles, повторную привязку listener и сетевой запрос после нового старта Core.
