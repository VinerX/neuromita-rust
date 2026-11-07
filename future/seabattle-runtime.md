# Sea Battle в Rust runtime

Реализация issue #34. Поведение сверено с Python-движком из снимка
`Atm4x/NeuroMita` `36f942106ab04522492477087e7db5ca41f0d626`:
`modules/SeaBattle/seabattle_logic.py`, `seabattle_instance.py`,
`seabattle_gui.py` и текущими вызовами. Python и Unity остаются read-only;
Rust не копирует их GUI.

## Что доступно

- Поле 10x10 и флот `4,3,3,2,2,2,1,1,1,1`; корабли не касаются даже
  диагоналями. Бой начинается только после расстановки обеих сторон.
- Ход игрока первый. Промах передаёт ход защитнику; попадание и потопление
  сохраняют ход атакующего; последний потопленный корабль завершает партию.
  Повторный выстрел, чужой ход и неверная фаза возвращают типизированную
  ошибку и не меняют состояние.
- Ручная и случайная расстановка. Случайная расстановка одной командой
  атомарна: при исчерпании ограниченного числа попыток клонированное поле
  откатывается целиком, частичный флот не публикуется.
- `Close` освобождает партию, `Start` после этого создаёт новую сессию.
  Невалидная новая команда не теряет уже принятую партию.

Терминальный путь — реальная играбельная поверхность:

```
/game start [auto|manual]
/game place A1 4 H
/game random
/game fire J10
/game turn
/game auto on|off
/game view
/game close
```

`/game turn` явно просит Миту расставить флот в фазе расстановки или сделать
ход в фазе боя; поэтому партия играбельна и при выключенных автоматических
реакциях. GUI-оболочки Sea Battle в Rust пока нет; desktop-overlay и Unity-сцены
не являются частью этой реализации.

## Владение и границы

`CharacterHandler` последовательно обслуживает типизированные команды через mailbox.
Единственный канонический источник игры — owner-private `GameStateCaptured` в Timeline;
в handler нет изменяемого поля с авторитетным матчем. Временная копия `GameSlot`
восстанавливается для подготовки команды и отбрасывается после коммита.

Запись содержит версионированный Timeline-факт с одним типизированным payload;
десериализация отклоняет неизвестные поля, неверные координаты, флот, фазу,
очередность и повторные выстрелы посредством replay чистого движка. Проверка
Timeline требует actor персонажа и ровно одного совпадающего observer.
Каждая успешная команда сохраняет снимок, включая placement и переключение
автореакций, которые не обязаны порождать публичный игровой факт.

Контекст читает состояние персонажа и игру из одного префикса. Исторические
`get_context` и генерация доступны во время игры. Fork наследует матч на
checkpoint; следующие изменения принадлежат выбранной ветке. Принятие игровых
эффектов сверяет ID приватной записи игры, revision персонажа и selection/context
epoch. Старое предложение не применяется после изменения игры, close/recreate,
переключения auto, reset или выбора ветки. Исторический reply с неизменившейся
игрой и revision может успешно примениться. Сообщение, игровой снимок,
санитизированные факты, память и reminders записываются одним guarded batch.

## Приватность

Публичные `GameFact` содержат только идентификатор игры, сессию, поколение,
revision, факт выстрела/фазы и результат. Скрытые доски, координаты
расстановки и исполняемые команды в событие не попадают. Принятые сегменты
речи очищаются перед записью в Timeline: `commands`, `start_game` и
`end_game` не сохраняются в сообщении, поэтому соседний персонаж не получает
скрытый флот через историю или prompt. `Debug` игрового слота и проекции
также не печатает доски. В prompt Миты допустима только её собственная доска
и клетки, которые она уже реально наблюдала выстрелами.

## Политика реакций

Владелец проверяет только глобальный `GamePolicy.enabled` для создания новой
партии (включая model-путь и прямой handle). Автоматические приглашения
создаются только при включённой per-match политике; глобальный
`GamePolicy.auto_reactions` проверяют `GameApi` и MessageHandler перед
отправкой авто-реакции. Если per-match флаг выключен, приглашение не
создаётся, а не помечается как «ungated». Ручная команда `/game turn` работает
независимо от автоматики.
Явный `Start`/`Close` также допускает комментарий персонажа согласно
политике, но не запускает синтетический ход.

Каждая авто- или ручная реакция использует существующий `ReactRequest` с
причиной — уже зафиксированным каноническим событием. Реакция публикуется
после commit действия; после `Finished` новых действий нет. Если очередь
полна и авто-реакция не принята, пользовательское действие не откатывается, а
ошибка возвращается в receipt; `/game turn` позволяет запросить ход Миты
позже. То же верно для ошибки приёма follow-up после попадания Миты.

## Хранение и честные границы

При persistent-конфигурации SQLite партия, флот обеих сторон и режим автореакций
переживают штатный shutdown/start Core. Transient Timeline остаётся только в памяти.
Приватный снимок исключён из KnownEvent, in-memory/SQLite индексов знания,
provider/UI world projection и публичных AcceptedResponse.effects; генерация получает
только собственное поле Миты и наблюдаемые ею клетки противника. ID снимка
не может быть причиной reply/react; тест проверяет отказ для обоих персонажей. Это дополнение
к изученному Python-поведению: исторические чтения и ветки теперь имеют явную
каноническую семантику. GUI Sea Battle и восстановление незавершённых запросов
после crash остаются за пределами реализации; терминальный игровой путь доступен.

Известное отличие от Python GUI: подсказка «твой ход» после попадания игрока
иногда ошибочна. Rust строит статус из фактического `turn`/`phase` и стороны
зрителя; терминал показывает «Ход Миты», а не «Твой ход», когда отвечает не
игрок. Случайная расстановка Rust также не оставляет частичный флот после
неудачи, в отличие от наблюдаемого поведения Python.

## Проверка

`tests/seabattle.rs` проходит полный HTTP-fixture сценарий: старт, известная
расстановка Миты, оба поля, легальный выстрел, hit-follow-up, close, новый
старт, ручной placement при auto=false, отмена и историческая генерация до старта матча.
`tests/seabattle_reference_acceptance.rs` (независимый golden) проверяет 63
выстрела и грамматику координат; родительский
`tests/seabattle_core_acceptance.rs` проверяет атомарность, stale, отмену и
gates через настоящие runtime APIs.

`tests/seabattle_history_restart.rs` запускает настоящий Core с SQLite: private
placement, historical get_context/reply, принятие неизменившейся истории,
отклонение старых mixed effects, fork с независимым продолжением, shutdown/start,
историческое чтение после close и отсутствие чужого флота в контексте.
Boundary unit tests отклоняют чужих observers/actor, unknown fields, некорректный
replay и generation; отдельный тест проверяет auto-toggle/context-epoch CAS.

## Acceptance after upstream integration, 2026-11-03

Reviewed baseline: `upstream/main` `15710db73adea787e9ddb23a3ea2550ded96d260`,
also verified against the remote main before final validation. The merge retains
upstream AI, reminders and other application changes. No dependency or vendor
changes relative to upstream were needed.

| Check | Result | Wall time |
|---|---|---|
| Focused actual Core / SQLite history, fork, restart and privacy | 1 passed | 20.05 s build, 1.69 s execution |
| `cargo test --locked --offline` | 458 passed, 2 ignored | 67.37 s |
| `cargo test --locked --offline --no-default-features` | 396 passed, 2 ignored | 58.40 s |
| Default all-target Clippy, `-D warnings` | passed | 12.59 s |
| No-default all-target Clippy, `-D warnings` | passed | 12.68 s |
| `cargo fmt --all -- --check` | passed | |
| `git diff upstream/main --check` | passed | |

All Cargo compilation commands use the same vendor override:
`--config source.vendored-sources.directory="C:/Development/NeuromitaRust/.worktrees/issue-51-current/vendor"`.
Shared target: `C:/Development/NeuromitaRust/target`; development/test debug info
and incremental compilation disabled. Python fixture interpreter is explicitly
`C:/Users/Dmitry/AppData/Local/Programs/Python/Python312/python.exe` via
`NEUROMITA_TEST_PYTHON` and `PYO3_PYTHON`; NASM comes from `C:/Program Files/NASM`.
Test execution and Clippy runs were serialized after the PR #67 slot release.
The final lint-only normalization is followed by a library/SQLite regression rerun.

Full logs are local under the shared target: `pr68-default-test.log`,
`pr68-nodefault-test.log`, `pr68-default-clippy.log`, `pr68-nodefault-clippy.log`,
`pr68-final-regression.log`, `pr68-final-nodefault-regression.log` and
`pr68-timings.txt`; they are not committed. No GUI build, live provider call,
push or publication is claimed by this acceptance record.
