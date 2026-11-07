# Semantic retrieval: рабочий план

- [x] Объединить PR #55–58 с актуальным сервером и schema 2; исправить первый config launcher.
- [x] Проверить интеграцию и отправить объединённый main; GitHub подтвердил merged для всех PR #55–58.
- [x] Независимый EmbeddingProvider и действующий Gemini HTTP adapter для моделей 001/2, проверенный локальными HTTP fixtures. Живой Gemini без credentials не проверялся.
- [x] Производный векторный индекс (точное cosine-сканирование до 256 memories) по зафиксированному Context/Timeline prefix; cache не владеет фактами.
- [x] Snapshot membership при Update/Delete/Merge, fork/historical reads, cache rebuild и отмене; bounded FIFO без stale search results.
- [x] Асинхронный semantic ranking до LLM, отдельный явно выбираемый lexical режим, единые лимиты контекста.
- [x] Синтетическое сравнение Recall@1 и реальных количеств HTTP requests/texts, локальные тесты и документация.
- [ ] Настоящий live quality/cost benchmark на явной выбранной пользователем модели/ключе и representative corpus; без этого не объявлять подтверждённую точность Gemini.

Источник истины — принятые факты Timeline. Индекс получает точный набор воспоминаний персонажа на выбранном префиксе; membership не определяется содержимым cache. Изменение текста требует нового embedding, удалённые записи не возвращаются в результат, исторический запрос использует свои версии записей. Внешний backend не вызывается под lock Timeline/Character/cache. Ошибка embeddings не включает скрытый fallback.

## Остановка и точка продолжения

Слияния опубликованы в main как b862c22. Все четыре исходных head входят в историю; PR закрыты GitHub как merged. Вошли также предшествующие исправления ядра и полноценная система миграций. Проверка Windows: 231 passed с SQLite и 180 без default features; в обоих режимах 2 ignored (live Gemini и launcher helper), Clippy всех targets и rustfmt прошли. Использованы только безопасные launcher helper-процессы, игра/Explorer не запускались. Исправлена первая запись launcher config без candidate, импорт/экспорт работает с schema 2 и общей миграцией 1 → 2. Сервер сохраняет publish_message/reply/react/initiate, каталог включает 11 команд.

Работа остановлена при остатке 7% пятичасового лимита по правилу пользователя. **В исходной ревизии semantic retrieval ещё не был реализован**, никаких embedding backend, векторного индекса или результатов измерения качества в этой ревизии нет. Следующий этап — именно незакрытые пункты выше, не новый обзор общего roadmap.

Предлагаемое продолжение, требующее реализации и проверки:

1. Независимый EmbeddingProvider (query/document tasks), проверяемые конечные векторы/размерность и явная идентичность embedding space: backend/model/dimensions/protocol. Gemini — один из адаптеров, секреты не входят в cache keys/logs. Локальные HTTP fixtures и opt-in реальная проверка.
2. Индекс строить из CharacterContext.memories на ContextAnchor; точный набор кандидатов всегда берётся из Timeline projection. Cache embeddings ограничить и адресовать по персонажу, space, MemoryId и содержимому. Update/Merge меняет ключ, Delete меняет membership, historical/fork читают свои версии. Сброс/восстановление cache не меняет память: индексы восстанавливаются из тех же фактов. Не держать mutex во время embedding await.
3. Вынести общий query/ranking из синхронного generation::conversation в явный асинхронный этап до LLM, используя тот же query для semantic и lexical сравнения. Сейчас LlmConversation::from_context собирает query из guidance, focus, target и последних трёх сообщений, затем memory_context::render_memory_context выполняет lexical ranking. При интеграции не переупорядочивать semantic shortlist повторно лексическим renderer. Сохранить предел 12 записей/6000 Unicode-символов и чистые провайдерские wire adapters.
4. Реальный Gemini adapter сверять с [API](https://ai.google.dev/api/embeddings) и [руководством](https://ai.google.dev/gemini-api/docs/embeddings). gemini-embedding-001 поддерживает RETRIEVAL_QUERY/RETRIEVAL_DOCUMENT; gemini-embedding-2 использует инструкции вместо taskType. Пространства этих моделей несовместимы. Выбирать протокол явно, а не отправлять один и тот же payload любой модели.
5. Проверить Update/Delete/Merge, смену space, приоритет, fork/historical reads и SQLite reopen, отмену/поздний embedding результат и отсутствие сетевых await под locks. Сравнить semantic и lexical на одном наборе данных; отдельно пометить синтетические fixtures и реальное качество модели. Стоимость учитывать в реальных измеримых единицах (requests/texts/tokens при наличии метаданных), не придумывать USD/токены.


## Реализованный срез

См. [SEMANTIC_RETRIEVAL.md](../src/generation/SEMANTIC_RETRIEVAL.md). Новая версия включает embedding provider + Gemini 001/2 HTTP wire, асинхронный отбор, производный bounded cache, а также fixture-бенчмарк. Остаток — live-model качество, реальные биллинговые единицы, прикладная настройка пресетов и UI.
