# Unity ↔ NeuroMita Rust: GameLink TCP integration contract

**Статус:** спецификация текущей серверной реализации, пригодная для написания нового Unity-клиента.
**Срез Rust:** `codex/gamelink-issue53-20261105`, commit `9badd68a0cccec8ee6623c3c141be18daf38a89c`.
**Источник истины:** `src/game_link.rs`, `src/game_context.rs`, `src/server.rs` (`status_with_audio`), `src/turn.rs`, `tests/local_control.rs`.
**Связанные документы:** [game-link.md](game-link.md), [Issue #53](https://github.com/Atm4x/neuromita-rust/issues/53).

Документ описывает **реально принимаемые команды и выдаваемые кадры**. Предложения по клиентской архитектуре и сценарии приёмки выделены отдельно. Версии протокола/negotiation в этом срезе нет: клиент и сервер должны быть согласованы с данным контрактом.

## 1. Транспорт, запуск и форматы

- **TCP**, локальный bind (`127.0.0.1`/loopback); сервер отклоняет нелокальный bind. Это **не WebSocket и не HTTP**.
- Каждый кадр — **один JSON-объект в UTF-8, завершённый `\n`** (NDJSON). Приём `\r\n` также поддерживается. Чтение выполняется потоково: один `ReadAsync` не обязан совпадать с границами кадра.
- Все клиентские кадры содержат `command_id` (UUID-строка) и `command` (snake_case). Ответы имеют `type` (snake_case). Для корреляции ответа используется `command_id`; асинхронные push-кадры могут обходиться без него.
- Для игровой генерации используются `request_id` (UUID) и `event_id` (UUID); они имеют отдельные назначения. `session_id` — UUID логической сессии, `session_token` — UUID-секрет для её возобновления.
- Имена персонажей строго соответствуют `CharacterId`: `Crazy`, `Kind`, `Cappie`, `ShortHair`, `Mila`, `Sleepy`, `Ghost`, `Creepy`, `GameMaster`.
- Все временные метки `occurred_at_unix_ms` — неотрицательные целые Unix milliseconds; `revision`/`generation` — целые без знака. Отрицательные значения не принимаются.
- `GameLink` включается конфигурацией приложения `ApplicationConfig.game_link = Some(GameLinkConfig::new(bind))`. По умолчанию поле **`None`**. Адрес/порт клиенту передаёт launcher или другая точка композиции; автоматическое обнаружение адреса протоколом пока не предусмотрено. В тестах сервер получает случайный свободный порт через bind `127.0.0.1:0`.

### Серверные лимиты по умолчанию

| Параметр | Значение |
|---|---:|
| Максимальный входной кадр | 65 536 байт |
| TCP-подключения | 32 |
| Сохранённые игровые сессии | 16 |
| TTL отключённой сессии | 30 минут |
| Ожидание `hello`/`resume` | 10 секунд |
| События в `publish_events` | 1–64 |
| Квитанции команд на сессию | 4096 |
| Буфер исходящих кадров на подключение | 64 |
| `read_audio_chunk.length` | 1–16 384 байт |

Эти значения — параметры `GameLinkConfig`; при превышении отдельных ограничений сервер возвращает `error`, а при переполнении исходящего канала отключает медленного клиента. Unity должна восстановить сессию через `resume`.

## 2. Подключение и восстановление

### 2.1 Первое подключение — `hello`

Клиент → сервер:

```json
{"command_id":"00000000-0000-4000-8000-000000000001","command":"hello"}
```

Сервер → клиент:

```json
{"type":"hello_ack","command_id":"00000000-0000-4000-8000-000000000001","session_id":"00000000-0000-4000-8000-000000000101","session_token":"00000000-0000-4000-8000-000000000102"}
```

Unity сохраняет `session_id` и `session_token` на время игровой сессии. `session_token` — bearer credential: не записывать в диагностические логи, не передавать сторонним игровым системам. После завершения процесса Rust прежние сессии и токены утрачиваются.

### 2.2 Переподключение — `resume`

На **новом** TCP-соединении:

```json
{"command_id":"00000000-0000-4000-8000-000000000002","command":"resume","session_id":"00000000-0000-4000-8000-000000000101","session_token":"00000000-0000-4000-8000-000000000102"}
```

Сервер отправляет последовательно:

1. `session_resumed` с `command_id`, `session_id` и числом `requests`.
2. `session_state` с `context_revision`, состоянием PTT, playback и результатами `acknowledge_result`.
3. **Текущие snapshots** всех сохранённых запросов этой сессии (`request_updated`/`request_completed`/`request_failed`/`request_cancelled`).

Пример `session_state`:

```json
{
  "type":"session_state",
  "session_id":"00000000-0000-4000-8000-000000000101",
  "context_revision":3,
  "ptt":null,
  "playback":[],
  "deliveries":[]
}
```

Сервер закрывает прежнее TCP-соединение владельца сессии. Порядок snapshots между разными запросами не определяется: клиент сопоставляет их по `request_id` и `revision`.

`resume` работает **в пределах жизни того же процесса Core** и до истечения TTL отключённой сессии. При `session_denied` клиент выполняет новый `hello`, заново публикует актуальный игровой контекст и продолжает работу с новой сессией. Восстановление незавершённых запросов между перезапусками Core относится к отдельной задаче #54.

### 2.3 `ping`

```json
{"command_id":"00000000-0000-4000-8000-000000000003","command":"ping"}
```

Ответ: `{"type":"pong","command_id":"00000000-0000-4000-8000-000000000003"}`.

`ping` доступен до `hello`; для остальных прикладных команд требуется установленная сессия. Частота heartbeat на текущем сервере не регламентирована, её выбирает Unity-клиент.

## 3. Каталог клиентских команд

Во всех запросах ниже подразумевается **новый UUID `command_id`**; в JSON-примерах поле показано явно. Если у команды есть side effect, при повторе после сетевого сбоя нужно повторить **тот же `command_id` и тот же payload**.

| `command` | Обязательные параметры помимо `command_id` | Ответ `type` |
|---|---|---|
| `hello` | — | `hello_ack` |
| `resume` | `session_id`, `session_token` | `session_resumed`, затем `session_state` + snapshots |
| `ping` | — | `pong` |
| `get_game_settings` | — | `game_settings` |
| `update_context` | `context` | `context_applied` |
| `publish_message` | `event_id`, `occurred_at_unix_ms`, `content`, `character` | `published` |
| `publish_events` | `events` | `events_published` |
| `reply` | `character`, `message_id` | `request_accepted` → push lifecycle |
| `react` | `character`, `event_id`; опционально `instruction` | `request_accepted` → push lifecycle |
| `initiate` | `character`; опционально `instruction` | `request_accepted` → push lifecycle |
| `cancel` | `request_id` | `cancel_ack` → push lifecycle, если отмена состоится |
| `report_ptt` | `capture_id`, `generation`, `active`; опционально `cancelled` | `ptt_accepted` |
| `report_playback` | `request_id`, `playback_id`, `phase` | `playback_accepted` |
| `read_audio_chunk` | `request_id`, `offset`, `length` | `audio_chunk` |
| `acknowledge_result` | `request_id`, `event_id`, `applied`; опционально `error` | `result_acknowledged` |

У `cancel` нет автоматического `request_accepted`; его `cancel_ack.result` — строковое представление результата метода отмены. Авторитетное изменение состояния запроса приходит push-кадром.

## 4. Семантическая история: сообщения и игровые события

### 4.1 Сообщение игрока: `publish_message`

```json
{
  "command_id":"00000000-0000-4000-8000-000000000010",
  "command":"publish_message",
  "event_id":"00000000-0000-4000-8000-000000000201",
  "occurred_at_unix_ms":1793912400000,
  "character":"Crazy",
  "content":"Привет, Мита"
}
```

Успешный ответ:

```json
{"type":"published","command_id":"00000000-0000-4000-8000-000000000010","event_id":"00000000-0000-4000-8000-000000000201","replayed":false}
```

Core создаёт `ConversationMessage` с `ConversationSpeaker::Player`, адресатом и наблюдателем `character`. `content` — произвольное JSON-значение (для обычного текста — JSON-строка). `event_id` назначает Unity, чтобы повтор передачи не порождал другую идентичность события. После успешной публикации `event_id` можно использовать как `message_id` в `reply`.

### 4.2 Игровой факт: `publish_events`

```json
{
  "command_id":"00000000-0000-4000-8000-000000000011",
  "command":"publish_events",
  "events":[
    {
      "event_id":"00000000-0000-4000-8000-000000000202",
      "kind":"unity_event",
      "occurred_at_unix_ms":1793912401000,
      "actor":"Player",
      "observers":["Crazy"],
      "payload":{"name":"DoorOpened","room":"Kitchen"}
    }
  ]
}
```

Ответ:

```json
{"type":"events_published","command_id":"00000000-0000-4000-8000-000000000011","event_ids":["00000000-0000-4000-8000-000000000202"],"replayed":false}
```

Разрешённые `kind`: `unity_event`, `game_started`, `game_action`, `game_ended`, `character_joined`, `character_left`, `player_inactive`, `proactive_opportunity`.

Каждый элемент содержит `event_id`, `kind`, `occurred_at_unix_ms`, массив `observers` из 1–9 канонических персонажей, JSON-объект `payload` и, при необходимости, `actor` (строка до 256 байт). `actor` может быть опущен или иметь `null`. Batch применяется атомарно через `SemanticBatch` в текущей Timeline selection. Порядок событий в `events` сохраняется при приёме.

`event_id` принятого игрового факта используется в `react`. Политику авторства/наблюдателей и список допустимых payload по конкретному игровому действию клиент и Core должны согласовать при интеграции: текущий транспорт проверяет форму и доменную валидность события, а отдельной Unity-specific авторизации actor/observers пока нет.

## 5. GameContext: актуальный runtime snapshot

`update_context` заменяет **весь** snapshot сессии:

```json
{
  "command_id":"00000000-0000-4000-8000-000000000012",
  "command":"update_context",
  "context":{
    "revision":1,
    "scene":"Kitchen",
    "world":{"room":"Kitchen","actors":["Player","Crazy"]},
    "capabilities":["look","walk"]
  }
}
```

Ответ: `{"type":"context_applied","command_id":"00000000-0000-4000-8000-000000000012","revision":1}`.

Структура `GameRuntimeContext`:

| Поле | Тип | Требование |
|---|---|---|
| `revision` | `u64` | Строго больше предыдущей ревизии этой сессии; стартовое значение сервера — `0` |
| `scene` | `string \| null` | До 256 байт |
| `world` | JSON object | До 128 ключей, до 128 байт на имя ключа, сериализованный размер до 32 768 байт |
| `capabilities` | `string[]` | До 64 непустых строк, каждая до 128 байт |

Все четыре поля обязательны; допускается `"scene":null`, `"world":{}` и `"capabilities":[]`. При `resume` приходит только `context_revision`, а сам snapshot сохраняется на стороне Rust. Клиент сравнивает ревизию и при необходимости отправляет **более новую** полную версию.

При `reply`/`react`/`initiate` GameLink захватывает неизменяемую копию GameContext **в момент принятия команды**. Генерация получает её отдельно от исторической MitaView, как живой игровой snapshot. Изменения GameContext после начала запроса влияют на следующие запросы.

## 6. Генерация: Reply / React / Initiate

### 6.1 `reply`

Ответ на ранее опубликованное `ConversationMessage`:

```json
{"command_id":"00000000-0000-4000-8000-000000000020","command":"reply","character":"Crazy","message_id":"00000000-0000-4000-8000-000000000201"}
```

### 6.2 `react`

Реакция на известное персонажу событие:

```json
{"command_id":"00000000-0000-4000-8000-000000000021","command":"react","character":"Crazy","event_id":"00000000-0000-4000-8000-000000000202","instruction":"Отреагируй на открытие двери"}
```

### 6.3 `initiate`

Самостоятельное начало реплики:

```json
{"command_id":"00000000-0000-4000-8000-000000000022","command":"initiate","character":"Crazy","instruction":"Осмотрись в комнате"}
```

`instruction` можно опустить (значение по умолчанию — пустая строка). Все три операции запускают существующий `ChatApi` с `RequestSource::Game(session_id)`; настройки голоса выбираются из `VoiceSettings.in_game`, а результат адресован игровой сессии.

### 6.4 Push lifecycle — Unity читает один непрерывный поток

После принятия команды:

```json
{"type":"request_accepted","command_id":"00000000-0000-4000-8000-000000000022","request_id":"00000000-0000-4000-8000-000000000301","replayed":false}
```

Затем сервер **сам** присылает изменения состояния. Основные поля snapshot:

| Поле | Описание |
|---|---|
| `type` | `request_updated`, `request_completed`, `request_failed` или `request_cancelled` |
| `session_id` | Игровая сессия |
| `request_id` | Постоянная идентичность генерации |
| `character` | Канонический ID персонажа |
| `revision` | Монотонная ревизия конкретного RequestHandle |
| `state` | `pending`, `processing`, `finished` или `cancelled` |
| `stage` | При `processing`: `text`, `text_ready`, `voice`, `accepting` |
| `can_cancel` | Возможность отмены в текущем состоянии |
| `text` + `provisional` | Предварительный текст в процессе работы (при наличии) |
| `outcome` | При `finished`: `success`, `voice_error`, `text_error`, `internal_error` |
| `event_id`, `text`, `segments` | Для принятых ответов `success` и `voice_error` |
| `error`, `partial_text` | По соответствующему ошибочному исходу |
| `audio_asset` | Только для принятого ответа с синтезированным WAV |
| `dataset_sample_id`, `dataset_warning` | Опциональные данные диагностики dataset capture |

Пример терминального результата **без звука**:

```json
{
  "type":"request_completed",
  "session_id":"00000000-0000-4000-8000-000000000101",
  "request_id":"00000000-0000-4000-8000-000000000301",
  "character":"Crazy",
  "revision":4,
  "state":"finished",
  "can_cancel":false,
  "outcome":"success",
  "text":"Привет!",
  "event_id":"00000000-0000-4000-8000-000000000302",
  "segments":[{"text":"Привет!"}]
}
```

Это **образец полей**, а не обещание конкретного числа промежуточных обновлений или точного значения `revision`.

- `request_completed` бывает и при `outcome:"voice_error"`: текст и `event_id` уже приняты Timeline, звук не готов.
- `request_failed` бывает при `text_error` и `internal_error`.
- `request_cancelled` содержит `state:"cancelled"`.
- Операции могут завершаться быстро; Unity должна принимать терминальный результат сразу после `request_accepted`.
- При повторной отправке того же запроса (тот же `command_id` и payload) сервер возвращает `request_accepted` с `replayed:true` и текущий snapshot.
- После `resume` сервер отправляет актуальные snapshots, а не обещает повторить все промежуточные переходы.

**Ни `get_status`, ни `subscribe` в GameLink нет.** Основной поток — push по постоянному TCP-соединению; `resume` синхронизирует состояние после reconnect.

### 6.5 `cancel`

```json
{"command_id":"00000000-0000-4000-8000-000000000023","command":"cancel","request_id":"00000000-0000-4000-8000-000000000301"}
```

Ответ: `{"type":"cancel_ack","command_id":"...","request_id":"...","result":"..."}`. Значение `result` сериализуется из внутреннего результата отмены; клиент использует последующий push snapshot как итоговое состояние. Отменять можно запросы, принадлежащие данной сессии.

## 7. Озвучка, передача WAV и применение результата

### 7.1 Настройки игры

Unity при установке сессии запрашивает:

```json
{"command_id":"00000000-0000-4000-8000-000000000030","command":"get_game_settings"}
```

Ответ (значения иллюстративные):

```json
{"type":"game_settings","command_id":"00000000-0000-4000-8000-000000000030","revision":7,"voice":{"enabled":true,"volume":80,"profile":"default"}}
```

При любом изменении SettingsOwner подключённые сессии получают `game_settings_updated` с `command_id:null`, общей ревизией настроек и той же структурой `voice`. `voice.enabled` соответствует `in_game`, `volume` — число `0..100`, `profile` — строка настроек голоса. На `resume` запросите `get_game_settings`, чтобы подтвердить актуальность после сетевого разрыва.

### 7.2 WAV asset и загрузка чанками

Если TTS завершился успешно, `request_completed` содержит descriptor:

```json
{
  "audio_asset": {
    "request_id":"00000000-0000-4000-8000-000000000301",
    "format":"wave",
    "size_bytes":123456,
    "sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  }
}
```

Значения `size_bytes` и `sha256` здесь показаны как пример формата. Сервер реально вычисляет SHA-256 по байтам ответа TTS.

Для каждого блока Unity отправляет:

```json
{"command_id":"00000000-0000-4000-8000-000000000031","command":"read_audio_chunk","request_id":"00000000-0000-4000-8000-000000000301","offset":0,"length":16384}
```

Сервер отвечает:

```json
{"type":"audio_chunk","command_id":"00000000-0000-4000-8000-000000000031","request_id":"00000000-0000-4000-8000-000000000301","offset":0,"next_offset":16384,"size_bytes":123456,"base64":"<encoded bytes>"}
```

Алгоритм клиента: запросить блок с `offset=0`, декодировать `base64`, продвигать `offset = next_offset`, пока `offset == size_bytes`, сверить собранную длину и SHA-256 с descriptor, затем передать WAV в игровую систему воспроизведения. Для последнего блока сервер вернёт фактический размер, вплоть до конца файла. Запрос с `offset == size_bytes` даёт пустой `base64`. Аудиоданные доступны только из сохранённого `RequestHandle` данной сессии; дополнительное хранилище на стороне Rust не создаётся.

### 7.3 Подтверждение применения результата

После того как Unity обработала текст и структурированные `segments`, отправляется:

```json
{"command_id":"00000000-0000-4000-8000-000000000032","command":"acknowledge_result","request_id":"00000000-0000-4000-8000-000000000301","event_id":"00000000-0000-4000-8000-000000000302","applied":true}
```

Ответ: `{"type":"result_acknowledged","command_id":"...","request_id":"...","event_id":"...","applied":true}`.

При ошибке применения: `"applied":false,"error":"причина"` (не более 1024 байт). При `applied:true` поле `error` отсутствует. Сервер принимает один результат применения на `request_id`. Повторяйте тот же `command_id` для идемпотентного replay. Состояние подтверждений приходит в `session_state.deliveries` после `resume`.

`segments` имеют тип `ResponseSegment`: `text`, `emotions`, `animations`, `idle_animations`, `commands`, `movement_modes`, `visual_effects`, `clothes`, `music`, `interactions`, `face_params`, `intents`, `start_game`, `end_game`, `target`, `hint`, `allow_sleep`. В ответе отсутствующие опциональные/пустые поля допустимы. Список полей — контракт десериализации; фактическое применение анимаций, музыки, движения и визуальных эффектов реализует игровой клиент. В Core исполняются только поддержанные доменные игровые эффекты.

### 7.4 Feedback фактического воспроизведения

После начала playback:

```json
{"command_id":"00000000-0000-4000-8000-000000000033","command":"report_playback","request_id":"00000000-0000-4000-8000-000000000301","playback_id":"00000000-0000-4000-8000-000000000401","phase":"started"}
```

По окончании — тот же `playback_id`, новый `command_id`, `phase:"completed"` или `phase:"failed"`. Ответ: `playback_accepted` с `command_id`, `request_id` и `phase`.

Сервер принимает playback только для принадлежащего сессии запроса с завершённым `outcome:"success"` и аудио. Допустимые переходы: первый `started`, затем `completed`/`failed`; повтор того же состояния допустим, смена терминального состояния или `playback_id` — ошибка. Последнее состояние сохраняется в `session_state.playback`.

### 7.5 PTT feedback

```json
{"command_id":"00000000-0000-4000-8000-000000000034","command":"report_ptt","capture_id":"00000000-0000-4000-8000-000000000501","generation":1,"active":true,"cancelled":false}
```

Ответ: `ptt_accepted` с `command_id`, `capture_id`, `generation`.

`capture_id` — UUID конкретного захвата микрофона; `generation` — монотонная версия состояния этого захвата. Сервер отклоняет уменьшение generation в пределах одного `capture_id`, `cancelled:true` вместе с `active:true`, а также начало другого capture при ещё активном предыдущем. Актуальный PTT snapshot восстанавливается через `session_state.ptt`.

**Граница текущей реализации:** GameLink принимает, проверяет и сохраняет PTT/playback feedback; реальная привязка этих сигналов к игровой ASR/echo-suppression политике и к Unity audio controller выполняется в интеграционном коде. Сам приём `report_ptt` не запускает ASR inference в Rust.

## 8. Ошибки и повторная доставка

Все прикладные ошибки используют:

```json
{"type":"error","command_id":"00000000-0000-4000-8000-000000000035","code":"ownership_denied","error":"unknown game request"}
```

Если `command_id` невозможно извлечь (например, некорректный JSON или слишком большой кадр), сервер указывает `command_id:null`.

| `code` | Ситуация / действие клиента |
|---|---|
| `invalid_command` | Ошибка десериализации, неизвестный `command`/поле или неверный тип; исправить JSON |
| `hello_required` | Выполнить `hello` или `resume` |
| `already_connected` | На этом сокете уже установлена игровая сессия |
| `handshake_timeout` | Handshake не выполнен вовремя; новое подключение |
| `command_too_large` | Кадр превысил лимит; соединение закрывается |
| `capacity` | Лимит сессий или квитанций; уменьшить нагрузку/создать новую сессию при необходимости |
| `session_denied` | Неизвестная, истёкшая сессия или неверный токен; выполнить новый `hello` |
| `session_replaced` | Другое TCP-подключение стало владельцем сессии |
| `command_id_conflict` | UUID повторно использован с другим payload; исправить управление ID |
| `publish_failed`, `invalid_events` | Некорректное событие/отказ Timeline; проверить поля и доменную валидность |
| `request_rejected` | ChatApi отклонил постановку генерации; отобразить причину |
| `ownership_denied` | `request_id` не принадлежит текущей сессии |
| `settings_unavailable` | Настройки не прочитаны |
| `context_rejected` | Неверный context или немонотонная revision |
| `ptt_rejected`, `playback_rejected` | Неверный переход обратной связи |
| `audio_unavailable`, `invalid_chunk` | Нет готового WAV или параметры чанка некорректны |
| `result_not_accepted`, `delivery_rejected` | Нет принятого результата с этим `event_id` или подтверждение уже записано |

**Идемпотентность:** `publish_message`, `publish_events`, генерация, `update_context`, `report_ptt`, `report_playback`, `acknowledge_result` сохраняют receipts по `command_id` **внутри текущей игровой сессии**. Повтор с идентичным payload возвращает квитанцию или текущий snapshot; повтор с изменённым payload отклоняется. `cancel`, `get_game_settings`, `ping`, `read_audio_chunk` выполняются как обращения без сохранённой квитанции.

При переподключении требуется сохранить IDs команд, факт их отправки и возможность повторить исходный payload. `event_id` опубликованного факта сохраняется при любых повторах доставки. Серверная гарантия receipts действует до истечения сессии или перезапуска Core; при повторном запуске приложения требуется новый session handshake.

## 9. Рекомендуемая организация Unity-клиента

Достаточно четырёх функциональных частей; названия компонентов приведены для ясности, структура проекта может быть иной:

1. **Transport** — один постоянный TCP-сокет, чтение UTF-8 до `\n`, очередь исходящих кадров, reconnect, корреляция `command_id`.
2. **Session** — `hello`/`resume`, хранение `session_id` и `session_token`, pending commands, reconciliation запросов по `request_id`/`revision`.
3. **Game ingress** — доставка сообщений игрока и игровых фактов в Timeline, полные `GameRuntimeContext` snapshots с растущими revision.
4. **Result/audio controller** — push-обработка генерации, выполнение подтверждённых сегментов на Unity main thread, скачивание WAV, playback/PTT reports и `acknowledge_result`.

Сетевой reader может выполняться вне Unity main thread. Любые операции с `GameObject`, `Animator`, `AudioSource`, сценой и прочими Unity API должны передаваться на основной поток. `acknowledge_result` отправляется после того, как клиент реально определил исход применения полученного результата.

### Минимальный сценарий: сообщение игрока → ответ Миты

```text
Unity                                Rust GameLink / Core
  |---- TCP connect ------------------------>|
  |---- hello ------------------------------>|
  |<--- hello_ack (session + token) ---------|
  |---- get_game_settings ------------------>|
  |<--- game_settings -----------------------|
  |---- update_context(revision=1) -------->|
  |<--- context_applied ---------------------|
  |---- publish_message(event_id=A) ------->|
  |<--- published(event_id=A) ---------------|
  |---- reply(character=Crazy,message_id=A)>|
  |<--- request_accepted(request_id=R) -----|
  |<--- request_updated(R,revision=...) -----|
  |<--- request_completed(R,event_id=B) ----|
  |---- acknowledge_result(R,B,applied) --->|
  |<--- result_acknowledged -----------------|
  |---- read_audio_chunk(R,offset=0) ------>|  [если audio_asset]
  |<--- audio_chunk -------------------------|
  |---- report_playback(R,started) -------->|
  |<--- playback_accepted -------------------|
  |---- report_playback(R,completed) ------>|
  |<--- playback_accepted -------------------|
```

### Восстановление после разрыва

```text
TCP disconnect
  → connect
  → resume(session_id,session_token)
  ← session_resumed
  ← session_state(context_revision,ptt,playback,deliveries)
  ← snapshots всех сохранившихся request_id
  → reconcile по request_id + максимальной revision
  → повторить неполучившие квитанцию команды с прежними command_id/payload
  → get_game_settings (получить актуальное состояние)
  → update_context с новой revision при необходимости
```

Если `resume` отклонён, Unity создаёт новую сессию, повторно публикует актуальный игровой snapshot и принимает решение по ещё неподтверждённым пользовательским операциям с учётом доступных канонических фактов. Существующий протокол не даёт межпроцессной гарантии exactly-once для новых команд после полного restart Core.

## 10. Приёмочные сценарии Unity ↔ Rust

- [ ] Подключение `hello` и настройка адреса без хардкода порта.
- [ ] `publish_message` → `reply` → push terminal, применение `segments` и `acknowledge_result`.
- [ ] `publish_events` → `react` для наблюдавшего событие персонажа; проверка независимости `observers`.
- [ ] `update_context` → генерация использует snapshot данной сессии; следующий snapshot меняет только новые запросы.
- [ ] `game_settings_updated` доставляется автоматически после изменения `in_game`/`volume`/`voice`; стартовое состояние через `get_game_settings`.
- [ ] `request_completed` с WAV: все чанки собраны, размер и SHA-256 совпадают, звук воспроизводится в Unity, отправлены `report_playback`.
- [ ] `outcome:"voice_error"`: текст отображён, отсутствует попытка читать WAV.
- [ ] PTT started/ended/cancelled, корректный `capture_id` и generation; возврат feedback после reconnect.
- [ ] Reconnect в ходе генерации: `resume`, актуальный snapshot и отсутствие повторного вызова LLM.
- [ ] Дублирование команды с тем же ID и payload; конфликт при изменённом payload.
- [ ] Два игровых клиента: изоляция RequestHandles, аудио, world snapshots и подтверждений.
- [ ] Старый сокет закрывается после `resume` нового соединения.
- [ ] Ошибки неизвестного персонажа, неверного `event_id`, устаревшего context revision, неверного токена, слишком большого кадра.
- [ ] TCP отключается во время воспроизведения: применённые сегменты и playback-report согласованы с восстановленной сессией.

## 11. Реализовано на сервере / границы следующей интеграции

**Есть в Rust на указанном коммите:** команды и push-кадры из таблицы, session resume, внутренний Request lifecycle, admission-time GameContext, канонический event ingress, voice routing по Game origin, chunked WAV, PTT/playback state, result acknowledgment, настройки игры, проверки лимитов. Серверные контрактные тесты находятся в `tests/local_control.rs`.

**Работы на стороне Unity:** новый клиент GameLink, перенос игровых producers на `publish_message`/`publish_events`, сбор и обновление GameContext, привязка typed segments к действиям Unity, загрузка/воспроизведение WAV, ASR/PTT feedback, reconnect и игровые E2E-тесты.

**Явные серверные границы:** GameLink допускает только loopback; авторизация заявленного actor/observers конкретными игровыми правами ещё требует отдельного правила; состояние GameLink-сессий transient; отсутствуют negotiated protocol versions, межпроцессный durable resume и универсальная серверная маршрутизация физических Unity действий. Текущий feedback `report_ptt`/`report_playback` фиксирует состояние сессии и требует привязки к реальной policy исполнения/захвата звука при интеграции.
