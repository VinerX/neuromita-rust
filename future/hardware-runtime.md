# Выбор устройства и готовность AI (#51)

Продолжение draft PR #70 поверх upstream/main 15710db7. Предыдущий вариант со вторым AiSupervisor и каталогом environments/bases заменён. Ниже описан текущий контракт; точные результаты проверок добавляются после выполнения.

## Владение и применение

Production installer по-прежнему готовит единый backends/ и versioned overlays/. Hardware не устанавливает пакеты и не создаёт альтернативный runtime. AiApplicationConfig::activate_with_hardware использует тот же путь запуска и AiSupervisor, что обычное приложение. ApplicationConfig.ai_hardware задаёт явный путь к сохранённому выбору; без явно заданного ai_runtime запуск с таким выбором отвергается.

Для изменения в работающем приложении HardwareRuntime заимствует RunningAiApplication. Он не может пережить shutdown владельца и не создаёт второй Supervisor. Замена сохраняет полный набор сервисов исходного worker, включая shared topology. Перед активацией проверяются выбранные plugin/settings и backend/revision в установленной composition.

Кандидат доказывает доступность backend/device, выполнение операции на устройстве и фактический runtime загруженной модели. Пока кандидат готовится, исходный worker остаётся рабочим. Ошибка загрузки, неподтверждённые параметры и изменение сохранённого выбора оставляют прежний worker. Проверка revision и публикация защищены одним файловым lock; запись новой конфигурации использует тот же lock. Ожидание занятого lock не бесконечное: возвращается явная ошибка, которую можно повторить.

## Пользовательские команды

```text
neuromita hardware --help
neuromita hardware select --config hardware.json --backend torch_cpu --revision REV --device cpu --precision fp32 --service asr --plugin gigaam --settings model-settings.json
neuromita hardware inspect --config hardware.json --data AI_DATA --python PYTHON_EXE --sdk PYTHON_SDK --plugins PLUGINS
neuromita hardware apply --config hardware.json --composition installed-plan.json --models model-plan.json --data AI_DATA --python PYTHON_EXE --sdk PYTHON_SDK --plugins PLUGINS
```

select сохраняет выбор, не подменяя пропущенные поля неявными defaults: при первом запуске все поля обязательны. Последующие изменения наследуют остальные подтверждённые поля текущего документа. Некорректная конфигурация сохраняемый файл не изменяет; oversized документы отвергаются. Значения plugin settings не печатаются в диагностике.

inspect запускает одноразовый процесс инвентаризации и завершает его без загрузки модели. Наличие драйвера/пакета/файлов не обозначается готовностью inference.

apply — явная проверочная сессия обычного владельца AI: разрешает уже установленные backend/overlay из installed-plan.json, загружает модель с подтверждёнными параметрами, сообщает результат и корректно завершает владельца. Это не команда изменения другого запущенного Core. Для живого Core применяется типизированный HardwareRuntime над его владельцем.

installed-plan.json — массив ServiceSelection существующего production composition: service, backends (LockedBackend) и overlays (LockedOverlay), с точными Python/package/model revision. Подменять его списком каталогов или угадывать совместимость по расширению .onnx нельзя. Если supplemental backend нужен модели, он входит в тот же план. model-plan.json — массив ModelActivation: service, plugin_id, settings и asset_revision (явная revision установленных model assets либо null для plugin без assets). Для shared worker указываются все его сервисы. Выбранные plugin/settings должны совпадать с hardware.json; пропущенный model plan не заменяется угадыванием весов.

## Worker SDK

initialize без runtime сохраняет существующий контракт. Для явного runtime plugin объявляет runtime_capabilities(), реализует initialize_runtime(settings, runtime) и runtime_status(). Host сверяет фактически загруженный backend/device/precision с запросом. Простое эхо запроса, обычный discovery available или наличие весов не принимаются за подтверждение. Неподдержанный plugin или сочетание параметров даёт явную ошибку.

CPU/CUDA/DirectML имеют канонические идентификаторы cpu, cuda:N, dml:N. CTranslate2 и remote backend различаются явно; remote не имеет локального device execution probe. FP16 разрешается только поддержанным устройством и plugin. Для CTranslate2 execution probe требуются пригодная модель Translator и входные tokens; неподдержанное семейство модели отвергается, скрытого CPU fallback нет. ONNX probe проверяет фактический execution provider; наличие provider в списке недостаточно.

## Приёмка и ограничения

Управляемый fixture-worker используется для отказов устройства/загрузки, rollback, stale switch и persistence. Python tests отдельно проверяют SDK и реальные adapter classes через управляемые зависимости. Они не доказывают загрузку пользовательских моделей на реальном GPU.

Реальные модельные веса, платные API и аудиоустройства при разработке не запускались. GUI выбора устройства не добавляется этим PR; доступен терминал и типизированный API. Неподдержанные plugin/runtime-сочетания сообщаются явно. Установка и обновление AI-компонентов остаются существующим production installer.

Python reference проверен повторно: Atm4x_NeuroMita 36f942106ab04522492477087e7db5ca41f0d626, voice_device_selection/cuda_precision_policy/hardware_inventory_service и worker runtime probe. Python/Unity исходники не изменялись.

## Независимая приёмка 2026-11-03

База upstream/main15710db73adea787e9ddb23a3ea2550ded96d260. Полные Rust tests выполнены последовательно с --test-threads=1: default433 passed/2ignored, no-default372 passed/2ignored. Предыдущий параллельный прогон получил четыре timeout при одновременных Python workers; он не является финальным доказательством. Логи находятся локально в корневом target/validation-51-final, не коммитятся.

Семь focused hardware acceptance tests прошли: сохранение/невалидный device/precision; candidate failure с сохранением старого worker; stale switch; фактический Core startup/load failure/recovery; actual SDK hardware.probe JSON через строгие Rust DTO; сохранение и повторная проверка подтверждённого runtime соседнего shared service; stale split startup после инициализации второго worker. Fixture composition проходит обычный installed backend/distribution resolver, обходного публичного конструктора не осталось.

После независимого ревью исправлены cuda_error/luid/compute_types в wire DTO, принудительный GigaAM CUDA autocast при explicit fp32, потеря параметров соседнего shared service и окно stale split startup. Полный Python unittest81 passed. Реальные классы адаптеров и bundled GigaAM forward/infer проверены с контролируемыми ML modules; физических GPU/реальных весов это не доказывает. Поддержанные комбинации перечислены в issue-51-python-runtime-support.md.

Parent Windows CLI smoke пересобранного exe: help, полное initial select, ошибка несовместимого device/backend, сохранение исходной revision после ошибки; значения settings не печатаются. Clippy/fmt и итоговый commit фиксируются после завершения финальной проверки.

Финальные проверки после lint-only нормализации: default/no-default all-target Clippy с -D warnings PASS; rustfmt PASS; повторные 7 hardware acceptance tests PASS. Default433/2ignored и no-default372/2ignored остаются полными проверенными наборами; Python81PASS. Vendor и dependency lock относительно main не менялись этим hardware-срезом. Финальные логи: target/validation-51-final/{default-test,nodefault-test,default-clippy,nodefault-clippy,hardware-final-regression}.log.
