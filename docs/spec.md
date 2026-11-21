# Language Reader — специфікація v0.1 і план реалізації

**Статус:** архітектурний baseline, рішення користувача зафіксовані 21.11.2026  
**Ціль:** автономний застосунок для вивчення іноземних мов через читання, переклад, озвучення, контекстну граматику та повторення лексики.  
**Платформи:** Windows + macOS (перші), Android + iOS (наступні).  
**Формат документа:** Markdown; призначений для використання як вихідний технічний документ і backlog для реалізації.

> **Розрізнення статусів:** [DECIDED] — прямо погоджена вимога або вибір; [PROPOSED] — моя технічна рекомендація, яку можна змінити під час реалізації; [GATE] — умова, без якої етап не вважається завершеним.

---

## 1. Архітектурні рішення (ADR)

| ADR | Статус | Рішення | Наслідок |
|---|---|---|---|
| ADR-001 | DECIDED | Спочатку для власного використання | Жодного облікового запису, платежів, аналітики чи обов'язкового backend |
| ADR-002 | DECIDED | Перші 10–20 мов, потім розширення до 100 без російської | Каталог і capability registry розширювані; стартовий пакет — 12 мов (PROPOSED) |
| ADR-003 | DECIDED | Дозволений переклад через проміжну мову | Directed translation graph; можливий маршрут `source → pivot → target` |
| ADR-004 | DECIDED | Базові функції на пристроях із 4 ГБ RAM; AI необов'язковий | Детерміноване ядро, lazy load, контроль пам'яті, жодної вимоги мати Qwen |
| ADR-005 | DECIDED | Спочатку Windows і macOS | Desktop vertical slice + desktop-реліз; mobile adapters пізніше |
| ADR-006 | DECIDED | Основний Reader із reflow; оригінал окремо | Структурований Document AST, окреме представлення оригінальної сторінки |
| ADR-007 | DECIDED | Повністю локальна історія та навчальний прогрес | SQLite та локальні assets; інтернет не потрібен після інсталяції ресурсів |
| ADR-008 | DECIDED | OCR для фотографій і сканованих PDF | Native OCR та fallback, збереження координат і provenance |
| ADR-009 | DECIDED | TTS обробляє по одному реченню | Речення — одиниця навігації, кешу та прогресу; підсвічування слів |
| ADR-010 | DECIDED | Вбудовані граматичні знання й сталі вирази | Версійні локальні мовні бази та правила; LLM не критична для reader |
| ADR-011 | DECIDED | Контекстний семантичний і орфографічний аналіз | Окремий Linguistic Engine з необов'язковими ML providers |
| ADR-012 | DECIDED | Особистий словник, forward/reverse квізи | Окремі навчальні стани за напрямками перекладу |
| ADR-013 | DECIDED | Системні Share In/Out, подібно до Telegram | Native adapters з імпортом тексту, URL, файлів, фото й експортом |
| ADR-014 | DECIDED | Програма сама завантажує Qwen3.5-0.8B, Qwen3.5-2B, Multilingual E5 Small | Model Manager з pinned manifest, hash verification, resume, rollback |
| ADR-015 | PROPOSED | Angular + Tauri 2 + Rust + SQLite | Один frontend + core, нативні адаптери ОС |
| ADR-016 | PROPOSED | llama.cpp для Qwen, ONNX Runtime для E5 | Leleka — потенційний додатковий backend; її код менеджера моделей можна перевикористати після аудиту |

### 1.1 Незмінні продуктні принципи

1. **Offline-first:** після встановлення необхідних мовних ресурсів режим Airplane Mode / вимкненої мережі не блокує імпорт, читання, прослуховування, словник, квізи та локальний прогрес. Доступність конкретного перекладу залежить від встановлених мовних пакетів.
2. **Дані користувача — локальні:** програма не надсилає його тексти та історію до хмари, якщо користувач явно не ввімкне майбутню додаткову функцію.
3. **Language pair — довільна:** джерело й переклад обираються незалежно, із прозорим індикатором доступності локального маршруту.
4. **Речення — головна навчальна одиниця:** блок/абзац зберігає контекст, токен — конкретне слово, expression — багатослівну конструкцію.
5. **AI необов'язкова:** неможливість завантажити або запустити Qwen/E5 не руйнує основні сценарії.
6. **Немає фальшивих обіцянок:** заявлена підтримка 100 мов не дорівнює 100 мовам із повноцінним NLP+OCR+TTS+NMT. Кожна можливість має окремий підтверджений статус.

---

## 2. Сфера функціональності

### 2.1 Основний Reader

- Імпорт `.txt`, `.docx`, `.pdf` і фото (`.jpg`, `.png`, `.webp`, `.heic` за наявності декодера ОС).
- Текстові PDF: пряме витягування текстового шару; фото/скани PDF: OCR для потрібних сторінок, включно зі змішаними PDF.
- Два синхронізовані подання `Source / Translation`. На desktop — дві колонки; на вузьких екранах — адаптивний вибір «поруч / один під одним / перемикання», не втрачаючи зв'язку сегментів.
- Оригінальний вигляд PDF/фотографії в окремому режимі. OCR-токени мають координати для виділення поверх зображення.
- Керування: play/pause, повторити, попереднє/наступне речення, швидкість, голос, мова перекладу; підсвітити активне речення і поточне слово (за наявності точних меж слів).
- Натискання на слово або частину сталого виразу → словникова картка; підтримка збереження до особистого словника.
- Переклад із можливим pivot-route; збереження контексту між реченнями та явний індикатор використання проміжної мови.
- Локальна історія документів, закладки, відновлення позиції та аудіо.

### 2.2 Vocabulary + Learning

- Запис одиниці: `language`, `lemma/expression`, `sense`, `surface`, `translations`, `gender`, `partOfSpeech`, `source sentence`, `document`, `accepted variants`.
- Дедуплікація лем із можливістю зберігати різні значення та граматичні особливості.
- Окремі картки `forward` і `reverse` зі своїми станами повторення.
- Режими MVP: показ відповіді + self-rating, typing, multiple-choice (неоднозначні синоніми враховуються).
- Інтервальні повторення, локальна історія відповідей і добірки за документами.
- Експорт/імпорт словника та прогресу до версійного переносимого архіву (CSV — додатково, з можливим втратою розширених полів).

### 2.3 Native Share

- `Share In`: текст, URL, фото, PDF, DOCX, TXT, кілька файлів; збереження тимчасових дескрипторів/URI до приватного storage до обробки.
- `Share Out`: вибраний фрагмент, пара «оригінал + переклад», словникова картка, колекція або файл експорту.
- Windows/macOS — спочатку відкриття через File/Open With, drag-and-drop, share-out; повноцінні shell Share Targets/Extensions — окремі завдання з тестами реєстрації в ОС.
- Android/iOS — після desktop release; Android intent targets, iOS Share Extension/App Group.

### 2.4 Не входить до першого релізу

Серверна синхронізація; облікові записи; оплата; соціальна мережа; AI-голосовий діалог; повноцінний Word-редактор; вимога піксельно точного відтворення DOCX/PDF у reflow; гарантія глибокого NLP для 100 мов; тренування власних LLM із нуля.

---

## 3. Початкові мови та правило розширення

**PROPOSED, перші 12:** англійська (`en`), українська (`uk`), французька (`fr`), німецька (`de`), іспанська (`es`), італійська (`it`), португальська (`pt`), польська (`pl`), нідерландська (`nl`), чеська (`cs`), румунська (`ro`), шведська (`sv`).

**Перші три для end-to-end quality gates:** `fr`, `uk`, `en`. Вони дозволяють перевірити прямий і pivot-переклад, різні граматичні структури та потрібні користувачу сценарії.

**Наступні кандидатури:** японська, китайська (мандаринська, з окремою обробкою систем письма), турецька, арабська. Це окремий тест на сегментацію, RTL і складнішу морфологію.

**Політика:** у каталозі пізніше до 100 мов без російської. У кожної мови окремі `ocr`, `tts`, `translationOut`, `translationIn`, `morphology`, `syntax`, `idioms`, `examples` з рівнями `unavailable | available | installed | validated`. Для мов і діалектів використовуються точні ідентифікатори, а не припущення, що кожен варіант підтримується однаково.

### Translation route policy

1. Шукати встановлену пряму модель `A→B`.
2. Якщо її немає — знайти найкоротший дозволений маршрут через одну проміжну мову (типово англійську) і показати його користувачу.
3. Якщо маршрутів кілька — вибрати найкращий за протестованою якістю, а не просто кількістю ребер.
4. Якщо локального маршруту немає — чітко показати, який пакет потрібно встановити; не виконувати непомітні online requests.
5. Для кожного перекладу кешувати `source segment hashes`, `context hash`, `target`, `route`, `model revisions`, `settings`.
6. Forward/reverse квізи формуються з перевірених значень словника; не слід трактувати машинний переклад як єдину правильну відповідь.

---

## 4. Рекомендована архітектура

```text
                  ┌──────────────────────────┐
                  │ Angular Reader UI        │
                  │ Library / Reader / Quiz  │
                  └────────────┬─────────────┘
                               │ Typed Tauri IPC
                  ┌────────────▼─────────────┐
                  │ Rust Application Core    │
                  │ Use cases + Job Scheduler│
                  ├────────────┬─────────────┤
                  │ Document   │ Linguistic   │
                  │ OCR        │ Translation  │
                  │ Speech     │ Vocabulary   │
                  │ Share      │ ModelManager │
                  └─────┬──────┴───────┬─────┘
                        │              │
             Native adapters       Inference providers
           Apple/Windows APIs      llama.cpp / ONNX
                        │              │
                  ┌─────▼──────────────▼─────┐
                  │ SQLite + Local Assets    │
                  │ user.db / packs / models │
                  └──────────────────────────┘
```

### 4.1 Важливі модульні межі

| Модуль | Вхід | Вихід | Не відповідає за |
|---|---|---|---|
| Document Engine | файли, фото, OCR regions | Document AST + source spans | переклад і UI |
| Linguistic Engine | речення + контекст + language pack | леми, морфологія, синтаксис, expressions, senses | inference backend |
| Translation Engine | смислові сегменти + route + контекст | переклад + сегментні alignment | TTS |
| Speech Engine | sentence + voice/rate | аудіо або live speech + boundaries | переклад |
| Learning Engine | dictionary entries, review events | картки, повторення, прогрес | мовна модель |
| Model Manager | signed/pinned catalog, target | installed model assets + status | UI |
| Inference Abstraction | generation/embedding jobs | structured result / embeddings | вибір способу навчання |
| Persistence | доменні операції | записи SQLite + assets | семантика |

**Ізоляція:** основна логіка на Rust; frontend не має прямого доступу до файлової системи, SQL чи моделей. Для desktop процесних сервісів не потрібен localhost HTTP-сервер.

### 4.2 Model runtime

- Qwen3.5-0.8B і Qwen3.5-2B: `llama.cpp` через нативний FFI (CPU fallback, Metal на Mac, сумісні прискорювачі на Windows після тестів).
- Multilingual E5 Small: ONNX Runtime, окремий `EmbeddingProvider`.
- Leleka: окремий опціональний inference provider; без hard dependency. Можливість перевикористати download/verify код має бути підтверджена аудитом конкретних crates та ліцензій.
- Системні OCR і TTS — за замовчуванням; додаткові автономні пакети лише там, де системний API не забезпечує потрібної якості/мови.
- `ResourceGovernor` вивантажує моделі при неактивності, забороняє зайву одночасність важких job, пріоритезує інтерактивне TTS/Reader.

### 4.3 Model Manager

**Потрібні дії:** `catalog`, `install`, `resume`, `pause`, `verify`, `activate`, `uninstall`, `rollback`, `capabilities`.

Маніфест кожного пакета: stable model ID, provider ID, architecture, revision/commit, quantization, source URLs, file sizes, SHA-256, platform constraints, min RAM/disk, license, runtime compatibility, tokenizer/assets, tested feature set, manifest signature metadata.

Алгоритм: explicit opt-in → перевірка місця/пам'яті → тимчасові файли → HTTP range resume → SHA-256 + перевірка походження маніфесту → атомарне переміщення → smoke inference → активація → запис у SQLite. У разі помилки попередній робочий пакет не видаляється.

**Примітка:** pinned URL + SHA-256 захищають цілісність від випадкових змін, але не довіру до скомпрометованого каталогу. Потрібна модель довіри до маніфесту (підпис або каталог, включений у підписаний застосунок).

---

## 5. Document AST і відповідність тексту

**Сутності:** `Document`, `DocumentRevision`, `Page`, `Region`, `Block`, `Sentence`, `Token`, `ExpressionOccurrence`, `TranslationSegment`, `TranslationAlignment`, `SourceAnchor`, `Annotation`.

- `SentenceId` стабільний усередині ревізії документа, не залежить від номерів DOM-елементів.
- Зберігати raw source text, OCR output, corrected text, display text і speech text окремо. Трансформації мають provenance.
- Для кожного token: UTF-8 byte offsets у canonical text, позиції grapheme clusters за потреби, індекс токена, координати OCR bbox/quad на сторінці, якщо є.
- На межі з Angular/Apple API конвертувати offsets до UTF-16 (`NSRange`, JS string indices). Не змішувати одиниці виміру.
- `ExpressionOccurrence` містить множину token IDs — не лише один contiguous span. Це дозволяє пропуски/інфлексії всередині сталих виразів.
- Переклад використовує `Alignment` типів 1:1, 1:N, N:1, N:M; не прив'язується механічно до одного номера речення.
- OCR-корекція ніколи не змінює вихідний файл. Зберігати confidence і можливість ручного виправлення.

### Read pipeline (resumable)

`import → persist source → extract/OCR → detect language → normalize/segment → persist AST → Reader available → prioritize translation and linguistic analysis near current sentence → optional audio prefetch`.

Jobs у БД: `pending | running | completed | failed | canceled`, dependency IDs, attempt count, input hash, engine/version, resumable checkpoint. Jobs idempotent. Закриття програми не призводить до втрати прогресу.

### Speech pipeline

`speech.prepare(sentence, voice, rate)` → `speech.play(sentence)` → events `started`, `word_boundary`, `ended`, `failed`; next sentence prefetch. Для системних TTS допускається live synthesis без аудіофайла. Для записаного аудіо потрібні word timings для підсвічування. **[GATE]** Для тих мов/голосів, де заявлено підсвічування слів, перевіряти реальні word-boundary callbacks; інакше позначати capability недоступною та дозволяти лише підсвічування речення, а не імітувати точність таймером.

---

## 6. Локальна схема зберігання

### user.db (read/write SQLite)

| Таблиця | Зміст / основні поля |
|---|---|
| `documents` | id, title, source_language, file_hash, source_asset_id, created_at |
| `document_revisions` | id, document_id, source_hash, parser_version, created_at |
| `blocks` | id, revision_id, parent_id, type, order, text, layout_ref |
| `sentences` | id, revision_id, block_id, ordinal, text, normalized_hash |
| `tokens` | id, sentence_id, ordinal, surface, offsets, lexeme_ref |
| `source_anchors` | token_id, page_id, quads, original_offsets |
| `translations` | id, document_revision_id, target_language, route, model_version, status |
| `translation_segments` | id, translation_id, text, order |
| `translation_alignments` | source span ids, target segment ids, alignment type |
| `linguistic_annotations` | sentence_id, engine_version, language, annotation_blob, confidence |
| `speech_assets` | sentence_id, voice_id, rate, audio_asset_id, timings, cache_version |
| `reading_sessions` | document_id, started_at, ended_at, active_duration |
| `reading_progress` | document_id, revision_id, sentence_id, token_id, audio_position_ms, updated_at |
| `vocabulary_entries` | id, language, lemma_or_expression, sense_id, snapshot, created_at |
| `vocabulary_contexts` | entry_id, document_id, sentence_id, original_example |
| `quiz_cards` | id, entry_id, source_language, target_language, direction, prompt_type |
| `review_states` | card_id, algorithm_version, stability, difficulty, due_at, last_reviewed_at |
| `review_events` | id, card_id, time, grade, answer, elapsed_ms |
| `model_installations` | model_id, revision, runtime, local_path, status, verified_at |
| `processing_jobs` | id, kind, input_hash, deps, status, checkpoint, attempts, error |
| `share_inbox` | id, payload_type, local_asset_id, source_app, status |
| `settings` | key, json_value, updated_at |

Критичні індекси: `(document_id, updated_at)`, `(revision_id, ordinal)`, `(sentence_id, ordinal)`, `(language, lemma_or_expression, sense_id)`, `(due_at)`, `(status, priority)`; ідентифікатори UUID/ULID. Всі користувацькі дані записуються транзакційно. Режим WAL — за умови корректної локальної FS; не копіювати активний `.db` без SQLite backup API.

### knowledge packs (read-only, versioned)

`catalog.sqlite`, `languages/<language-id>/<pack-version>/knowledge.sqlite`, морфологічні FST/dictionaries, grammar rules, compiled expression matcher, validated examples, metadata/license manifest. Заміна пакета атомарна; user.db зберігає immutable snapshot карток для стійкості до оновлень.

### Assets

`assets/documents`, `assets/ocr`, `assets/audio`, `models`, `packs`. Оригінали ніколи не видаляються разом із derived caches; окремий garbage collector для невикористаних cached artifacts. Backup/export включає user DB, оригінали, словник і прогрес, але може не включати величезні перевстановлювані моделі — список необхідних пакетів вкладається в backup manifest.

---

## 7. Репозиторій та межі модулів

```text
language-reader/
  apps/reader/                  # Angular application
  apps/reader/src-tauri/        # Desktop shell / IPC
  crates/domain/                # AST, IDs, language tags, events
  crates/application/           # Use cases, coordinators, scheduling
  crates/document/              # Parsing + OCR regions + text mapping
  crates/linguistic/            # Morphology, syntax, expression matching
  crates/translation/           # Direct + pivot model routing
  crates/speech/                # Sentence TTS, boundary events
  crates/learning/              # Vocabulary, quiz, spaced repetition
  crates/model-manager/         # Catalog, download, verify, activation
  crates/inference-api/         # Generation vs embeddings contracts
  crates/inference-llamacpp/    # Optional feature
  crates/inference-onnx/        # Optional feature
  crates/persistence/           # SQLite repositories, migrations
  crates/platform/              # Native capabilities abstraction
  resources/languages/          # Grammar/lexicon packages
  resources/model-catalog/      # Signed and versioned metadata
  tests/fixtures/               # TXT/DOCX/PDF/image test corpus
  docs/adr/                     # Architecture decision records
```

Це **цільові межі**, а не вимога створити всі crates у першому коміті. Почати з `domain`, `application`, `document`, `speech`, `persistence`, `platform`, `reader UI`; розділяти далі в міру впровадження.

Правило залежностей: `domain` не залежить від IO; `application` залежить від trait interfaces; adapter modules реалізують traits; UI не імпортує persistence або inference безпосередньо. Versioned IPC contracts тестуються з обох боків.

---

## 8. План реалізації: послідовні етапи та критерії готовності

### M0 — Архітектурний bootstrap і технічні ризики

**Роботи:** mono-repo, Angular+Tauri desktop, Rust workspace, CI на Windows/macOS, typed IPC codegen, SQLite migrations; spike на native TTS word boundaries; spike на системному OCR із bbox; зображення/рендеринг PDF на реальному Windows та Mac; перевірка 4-ГБ машини. Аудит Model Manager Leleka на можливість виділення без server/GPU-залежностей.

**[GATE]** Програма запускається на Windows і macOS; базова БД створюється й переживає рестарт; тестова фраза читається локальним TTS; фото розпізнається локально хоча б одним adapter; build reproducible. Невідповідні API зафіксовано в capability matrix, а не приховано.

### M1 — Document Engine + двоколонковий Reader

**Роботи:** `Document AST`, імпорт TXT/DOCX/PDF, OCR image/scanned PDF, stable sentence IDs, original/reflow views, синхронізація колонок, scrolling/selection, save position, прогрес, бібліотека, checksum source files, import from file association.

**[GATE]** Відкрити 1 TXT, 1 DOCX, 1 текстовий PDF, 1 скан PDF і 1 фото; у всіх доступне структуроване читання; рестарт не губить документ, останнє речення або доступ до оригіналу. Сторінка не відправляється до мережі.

### M2 — Speech Engine + відтворення по реченнях

**Роботи:** adapters Windows/macOS, перехід між реченнями, повтор, пауза, швидкість, word boundary, next-sentence prefetch, voice capability detection, локальний кеш metadata.

**[GATE]** Зафіксована синхронізація слова для підтверджених голосів; 20 послідовних переходів вперед/назад без зміщення індикатора; плеєр не губить позицію після закриття/restart; перевірка без мережі.

### M3 — Offline Translation (спочатку fr/uk/en)

**Роботи:** `TranslationProvider`, directed graph/pivot routing, package availability, language chooser, сегментний alignment, кеш та інкрементальне перекладення навколо активного абзацу. Визначити й протестувати NMT-моделі й ліцензії; не прив'язувати архітектуру до неперевіреного рушія.

**[GATE]** Для тестових `fr→uk`, `uk→fr`, `en→fr`, `fr→en` відтворюваний офлайн-маршрут за умови встановлених ресурсів; UI позначає pivot. Тексти, де переклад змінює кількість речень, коректно вирівнюються. Сформований quality report на ручному еталонному корпусі.

### M4 — Linguistic Knowledge + аналіз

**Роботи:** read-only мовні пакети, versioning, POS/lemma/morphology adapter, spell/OCR correction suggestions, expression matcher (у т.ч. non-contiguous expressions), context disambiguation, context dictionary cards, validated examples, FTS indexes.

**[GATE]** Контрольний корпус містить омоніми, інфлексії, різні роди, складні ідіоми й помилки OCR; принаймні для `fr`, `uk`, `en` картки повертають джерело значення й confidence, не змінюють авторський текст, а невідомі аналізи явно позначаються. Ресурси оновлюються без втрати user data.

### M5 — Особистий словник, квізи та повторення

**Роботи:** save lemma/sense/expression, контекст, уникнення дублікатів; independent forward/reverse card states; quiz UI; grades, review history, spaced repetition; collections/document dictionary; export/import.

**[GATE]** Одне слово у двох граматичних формах не створює зайві дублікати; forward і reverse мають незалежні due dates; квіз переживає restart; backup відновлює словник і історію.

### M6 — Model Manager + optional AI

**Роботи:** аудит Leleka, manifest, async download, pause/resume, SHA-256, trusted catalog, disk/RAM preflight, atomic activation, verification; Qwen3.5-0.8B, Qwen3.5-2B через llama.cpp; E5 через ONNX; resource governor; structured prompts і валідація.

**[GATE]** Завантаження можна перервати та продовжити; corrupt file виявляється; rollback працює; всі базові сценарії працюють з усіма AI-моделями видаленими; у 4-ГБ режимі важкий AI не запускається без перевірки бюджетів. E5 використовується лише для семантичних завдань, а не як генератор тексту.

### M7 — Desktop release для власного використання

**Роботи:** доступні перші 12 мов із чесною feature matrix; UX бібліотеки/карток/налаштувань; keyboard shortcuts; file associations; share-out; системний share-in там, де досяжний без крихких інтеграцій; installer; error recovery; offline regression; test hardware matrix.

**[GATE]** Встановлюється на чисті Windows/macOS; проходить real-world reading session; 4-ГБ основні функції не вимагають Qwen; персональні дані не передаються мережею; імпорт, TTS, прогрес, словник, квізи та експорт працюють після відключення інтернету. Підтримка конкретної мови заявляється лише за результатами тестування.

### M8 — Android та iOS

**Роботи:** Kotlin/Swift OCR/TTS adapters, mobile document pickers, Android Sharesheet і Intent ingestion, iOS Share Extension/App Group, phone/tablet layouts, battery-aware resource policy, mobile model downloads, lifecycle persistence, storage/permission handling.

**[GATE]** Той самий local document і vocabulary domain; користувач може share-in фото або PDF, читати й слухати, пройти квіз та відновити прогрес; Background/foreground не пошкоджує БД; AI-пакети встановлюються лише за явним запитом.

### M9 — Розширення з 12 до 100 мов

**Роботи:** ліцензійний аудит корпусів, grammar packs, додаткові OCR/TTS/NMT/morphology/idiom adapters, мовні сценарії для RTL і CJK, вимірювання покриття та контроль якості.

**[GATE]** Кожна мова має machine-readable capability profile, фактичні результати перевірок і явний статус доступних пакетів; жодної декларації «підтримується все» без тестів. Російську не включати.

---

## 9. Перший backlog для виконання (у порядку залежностей)

| ID | Пріоритет | Завдання | Залежність | Результат |
|---|---|---|---|---|
| LR-001 | P0 | Ініціалізувати monorepo Angular/Tauri/Rust | — | Запуск Windows/macOS |
| LR-002 | P0 | Domain IDs, language tags, TextSpan із UTF-8/UTF-16 mapping | LR-001 | Unit tests Unicode |
| LR-003 | P0 | SQLite migrations / asset store / backup API | LR-001 | Persist/restart test |
| LR-004 | P0 | Typed IPC, events, abort/cancel semantics | LR-001 | Integration test |
| LR-005 | P0 | Native TTS Windows/macOS + boundary proof | LR-004 | Working speech spike |
| LR-006 | P0 | Native OCR Windows/macOS + bounding boxes | LR-004 | OCR spike |
| LR-007 | P0 | TXT parser + sentence segmentation + AST | LR-002/003 | Import test |
| LR-008 | P0 | Reader two-column layout, synchronized selection/scroll | LR-007 | UI vertical slice |
| LR-009 | P0 | PDF extraction/render and scanned PDF OCR | LR-006/007 | Scanned PDF fixtures |
| LR-010 | P0 | DOCX structure parser | LR-007 | DOCX fixtures |
| LR-011 | P0 | Sentence player / repeat / next / previous | LR-005/008 | Playback acceptance |
| LR-012 | P0 | Reading sessions/progress auto-save | LR-003/011 | Recovery test |
| LR-013 | P0 | TranslationProvider + routing graph + cache | LR-007 | Offline routing tests |
| LR-014 | P0 | Validate/install initial NMT packs; fr/uk/en | LR-013 | First end-to-end translations |
| LR-015 | P1 | Knowledge pack format + grammar/idioms | LR-002/003 | Versioned packs |
| LR-016 | P1 | Morphology + expression matching + cards | LR-015 | Linguistic fixtures |
| LR-017 | P1 | Vocabulary/save contexts/reverse cards | LR-016/003 | Persistent vocabulary |
| LR-018 | P1 | Quiz UI + independent spaced repetition | LR-017 | Review tests |
| LR-019 | P1 | Model Manager from audited Leleka ideas | LR-003 | Download/verify/rollback |
| LR-020 | P1 | llama.cpp Qwen provider | LR-019 | Optional Qwen analysis |
| LR-021 | P1 | ONNX E5 provider | LR-019 | Semantic retrieval |
| LR-022 | P1 | Resource Governor & model unload policies | LR-020/021 | 4GB tests |
| LR-023 | P1 | Share In/Out desktop adapters | LR-007 | OS integration tests |
| LR-024 | P1 | Export/import backup and dictionary | LR-003/017 | Portable backup |
| LR-025 | P1 | Packaging / clean install / regression | LR-001..024 | Desktop release candidate |
| LR-026 | P2 | Android/iOS native adapters and UI | LR-025 | Mobile release candidate |
| LR-027 | P2 | Scale verified language packs to 100 | LR-025 | Capability coverage report |

**Паралельність:** LR-005, LR-006 та LR-003 можна вести паралельно після базового bootstrap. Не запускати масштабне наповнення 100 мов, доки не стабілізовано AST і схеми пакета.

---

## 10. Якість, безпека і критерії приймання

### 10.1 Обов'язкові автоматизовані тести

- Rust unit tests: parser, normalization, offsets, sentence splitting, morphology/expressions, translation route graph, FSRS calculation, cache keys.
- Integration: imports, SQLite schema migration, job restart, model partial download / corruption / rollback, voice boundary events.
- E2E: Windows/macOS workflow `import → translate → listen → save vocab → reverse quiz → restart → resume`.
- Unicode/property tests: combining marks, apostrophes, Arabic bidi, CJK segmentation, non-BMP code points, word boundaries.
- Offline smoke: мережа заборонена після завантаження пакетів; усі заявлені локальні операції завершуються успішно без запитів до зовнішніх сервісів.
- Performance: профілі 4-ГБ RAM та стандартного desktop, великі PDF, фото, тривале озвучення; збирати реальні peak RSS / CPU / час до першого речення, а не оголошувати недосяжні обіцянки.

### 10.2 Privacy / security

- За замовчуванням немає відправлення вмісту документів у мережу.
- Security of imported content: path traversal prevention, обмеження архівних розпакувань, перевірка MIME, обмеження size/decompression, санітизація HTML/links, no executable content з DOCX/PDF.
- Model Manager: signed/pinned manifests, SHA-256, safe extraction, HTTPS, no arbitrary executable model hooks, модель лише дані.
- Access control: Tauri command capabilities, файлові дозволи найменшого обсягу, Share Inbox із secure staging.
- Backup: SQLite online backup API, manifest версій, перевірка цілісності відновленого архіву.
- License inventory для кожного OCR/TTS/NMT/NLP/grammar package. Особисте використання зараз не знімає майбутніх ліцензійних обмежень при розповсюдженні.

### 10.3 Definition of Done для desktop v0.1

1. Відтворювано збирається для Windows і macOS, створює приватну локальну БД.
2. Імпортує TXT, DOCX, текстовий PDF, сканований PDF і фотографію; дозволяє побачити оригінал.
3. Reader показує текст і доступний переклад, синхронізує вибір/позицію; при відсутньому пакеті відображає чесний статус.
4. Підтримує локальний TTS по реченнях, навігацію, повтор і перевірене word highlight для заявлених голосів.
5. Дозволяє переглядати локальні лексичні й граматичні пояснення та сталі вирази принаймні для core мов.
6. Слова/вирази додаються до персонального словника; forward і reverse мають окремі review states.
7. Прогрес, історія, словник і квізи зберігаються після рестарту; можливий експорт/імпорт.
8. Можна за бажанням завантажити й перевірити всі три вказані AI-моделі; базовий Reader працює без них.
9. Усі **заявлені встановлені** офлайн-функції працюють при заблокованій мережі.
10. Для всіх 12 стартових мов доступна capability matrix, а не необґрунтована гарантія ідентичної якості.

---

## 11. Відкриті технічні дослідження (не блокують bootstrap)

| Тема | Що перевірити | Рішення за замовчуванням |
|---|---|---|
| Перекладач | Наявність ліцензійно сумісних локальних NMT для core пар, якість pivot через en, пам'ять | Абстрактний TranslationProvider, concrete model — після benchmark |
| TTS Windows | Події меж слів для реальних системних голосів | Native adapter + capability detection |
| OCR PDF | OCR з layout-aware reading order, quality of rotated/scanned pages | Native OCR + координати й ручна корекція |
| Leleka | Які саме crates із fetch/verify можуть існувати незалежно від GPU/runtime | Самостійний Model Manager, перевикористання після аудиту |
| Qwen | GGUF/quantization performance на Windows/macOS/4GB | Необов'язкові 0.8B/2B, CPU fallback |
| E5 | Якість retrieval для конкретних мов, memory & tokenizer | ONNX provider, відкладено до реальної потреби |
| Grammar datasets | Покриття, якість і ліцензії для 12 мов | Versioned per-language packs |
| Native Share | Поведінка OS targets/extensions, security scoping | Desktop import/export first, full Share extensions later |

---

## 12. Перевірені зовнішні опори (стан на 21.11.2026)

- Tauri 2, cross-platform architecture: https://v2.tauri.app/ ; https://v2.tauri.app/concept/architecture/
- Tauri platform/plugin development: https://v2.tauri.app/develop/plugins/
- llama.cpp (GGUF inference, Metal/CUDA/Vulkan): https://github.com/ggml-org/llama.cpp
- ONNX Runtime Mobile: https://onnxruntime.ai/docs/tutorials/mobile/
- Leleka repository (потенційне повторне використання, без підтвердження сумісності crates): https://git.vezha.io/para/leleka
- Model origins: https://huggingface.co/ggml-org/Qwen3.5-0.8B-GGUF ; https://huggingface.co/Qwen/Qwen3.5-2B ; https://huggingface.co/intfloat/multilingual-e5-small

---

## 13. Перший конкретний крок розробки

**Починати з M0 і одного наскрізного сценарію, не з AI та не зі 100 мов:**

1. Порожній Tauri+Angular застосунок на Windows/macOS із Rust Core та SQLite.
2. Імпорт TXT, поділ на речення, два текстових стовпці (тимчасово переклад може братися з тестового fixture).
3. Native TTS: play/next/previous/word-boundary та позиція в SQLite.
4. Native OCR одного фото й перетворення його в той самий Document AST.
5. Лише після цього — реальний TranslationProvider та загальні language packs.

**Результат цього кроку:** на обох desktop ОС користувач відкриває документ або фото, бачить структурований текст, чує його реченнями, переходить між реченнями та після перезапуску продовжує з того самого місця. Це найважливіший доказ життєздатності архітектури.
