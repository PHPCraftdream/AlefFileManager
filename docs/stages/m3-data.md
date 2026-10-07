# M3 — Данные и перевод File Manager

Обзор: `../FRAMEWORK-PLAN.md`. Зависит от M2 (гранты диалогов, `path`).

## Цель

Модули `fs`, `store`, `sqlite`, `crypto` (+ `secrets`). Доказательство достаточности API: Alef File Manager переводится с собственных Rust-команд (`directory.list`, `preferences.get/set`) на `fs` и `store`, `backend/src` удаляется.

## Модули

### `fs` (data)

```ts
fs.readText(path, { encoding? }): Promise<string>
fs.readBytes(path): Promise<Uint8Array>
fs.writeText(path, text, { append?, create? }), fs.writeBytes(path, data, opts)
fs.open(path, { read?, write?, append?, create?, truncate? }): Promise<FileHandle>
  FileHandle: readable: ReadableStream<Uint8Array>, writable: WritableStream<Uint8Array>,
              read(len, position?), write(data, position?), stat(), truncate(len), sync(), close(), [Symbol.asyncDispose]
fs.stat(path) / lstat(path): Promise<Stat>         // size, kind (file|dir|symlink), times, readonly
fs.readDir(path): Promise<DirEntry[]>              // name, path, kind, size
fs.readDirStream(path): AsyncIterable<DirEntry>    // большие каталоги
fs.mkdir(path, { recursive? }), fs.remove(path, { recursive? }), fs.rename(from, to), fs.copy(from, to)
fs.exists(path): Promise<boolean>
fs.watch(path, { recursive? }): AsyncIterable<WatchEvent>   // create|modify|remove|rename
fs.tempFile() / tempDir(): Promise<string>
```

- Права: `permissions.fs.read`/`write` — списки шаблонов (§M1 scope-переменные) + гранты сессии из `dialog`/`file-drop`.
- Канонизация пути до проверки scope (symlink, `..`, UNC/`\\?\` на Windows, регистр на Windows/macOS); проверка повторяется для цели symlink.
- Большие чтения/записи — только потоками/handle (транспорт v2, чанки 256 KiB); `readBytes`/`writeBytes` — с лимитом (например, 64 MiB) и подсказкой использовать `open`.
- `watch` — `notify`, дебаунс, ресурс сессии.
- Ошибки ОС → `NOT_FOUND`, `PERMISSION_DENIED`, `ALREADY_EXISTS`, `NOT_A_DIRECTORY`, `DIRECTORY_NOT_EMPTY`, `BUSY`.

- Подмена (`../FRAMEWORK-PLAN.md` §6.4, `m2b-consent.md`): при решении «подменить» каждый объявленный scope отображается на теневой каталог приложения в данных runtime; чтение видит его содержимое (сначала пусто), запись успешна, но остаётся в тени; пути вне scope — `PERMISSION_DENIED`, как при отсутствии права в манифесте. Приложение не получает ни кода ошибки, ни иных признаков подмены. Выбор файла пользователем в `dialog` даёт реальный доступ к этому файлу (грант сессии).

#### Как устроено сейчас (M3.1)

Команды модуля — `alef-modules/src/data/fs/`: `fs.readFile`, `fs.writeFile`, `fs.stat`, `fs.lstat`, `fs.readDir`, `fs.exists`, `fs.mkdir`, `fs.remove`, `fs.rename`, `fs.copy`, `fs.tempFile`, `fs.tempDir`; JS API — `packages/api/src/data/fs.ts` (`readBytes`, `readText`, `writeBytes`, `writeText` поверх `readFile`/`writeFile`). Дескрипторы (`fs.open`, `FileHandle`), `readDirStream` и `watch` — M3.2, см. ниже.

- **Права проверяет обработчик, не реестр.** Путь сначала приводится к записи, которую он называет (ссылка в конце пути разыменовывается для чтения и `stat`, не разыменовывается для `remove`, `rename` и `lstat`: `Reach::Through` и `Reach::Entry`), потом проверяется по областям `fs.read`/`fs.write`, путям из диалогов (гранты сессии) и решению пользователя. `rename` требует запись у обоих концов, `copy` — чтение источника и запись приёмника. Путь вне областей, относительный, с `..`-обходом или через ссылку наружу — один и тот же `PERMISSION_DENIED`.
- **Подмена.** Где пользователь выбрал «подменить» область, команды работают с тем же путём внутри папки подмены в данных runtime (`<данные runtime>/shadow/<ключ приложения>/<область>/<путь внутри области>`): область — всегда папка, сначала пустая; запись удаётся и остаётся в тени; в ответах пути приложения собственные. Копирование между настоящей папкой и подменой переносит содержимое (подмена → настоящее — настоящий файл). Папка данных runtime недоступна ни одному праву (`with_protected`), в том числе через область `$HOME/**`.
- **Ошибки.** `NOT_FOUND`, `ALREADY_EXISTS`, `NOT_A_DIRECTORY`, `IS_A_DIRECTORY`, `DIRECTORY_NOT_EMPTY`, `BUSY` (три новых кода добавлены в `ErrorCode` и в типы JS); слова сообщений свои, без пути и текста системы.
- **Пределы.** Файл целиком — до 64 MiB (`readFile`, `writeFile`; больше — через `fs.open`, M3.2); `readDir` — до 100000 записей. Запись идёт телом запроса (двоичный транспорт), не JSON.
- **`copy`** копирует файл или папку целиком; папка-приёмник не должна существовать, папка не копируется в саму себя, ссылки внутри папки не копируются (ошибка). `rename` через границу диска — копирование и удаление.
- **`tempFile`/`tempDir`** создают в `$APPCACHE/tmp` и дают документу чтение и запись на этот путь на время его жизни; удаляются приложением.
- **Дескриптор (M3.2).** `fs.open(path, { read, write, append, create, truncate, createNew })` дает `FileHandle` — ресурс сессии документа: закрывается вместе с документом, чужой документ его не видит (`NOT_FOUND`). Права: запись нужна для `write`/`append`/`create`/`truncate`/`createNew`, чтение — для `read` (по умолчанию, если ничего другого не просили); если нужны оба права, решения пользователя для них должны совпадать (файл не может быть настоящим для чтения и подменой для записи — `PERMISSION_DENIED`). `read(length, position?)` (до 16 MiB), `write(data, position?)`, `stat`, `truncate`, `sync`, `close`; без позиции работают с позицией дескриптора, с позицией ее не двигают; файл, открытый для `append`, всегда пишется в конец.
- **Потоки (M3.2).** `handle.readable` / `handle.readStream({ position, length })` и `handle.writable` / `handle.writeStream({ position })` — потоки транспорта с обратным давлением в обе стороны (чанки 256 KiB, окно 1 MiB): файл любого размера идет ограниченным буфером, `await source.readable.pipeTo(target.writable)`. Закрытие `writable` ждет (`fs.settle`), пока записанное дошло до файла, и возвращает ошибку записи, если она была; потом закрывают дескриптор. На дескриптор — один поток за раз (`BUSY`).
- **`readDirStream(path)`** отдает записи большой папки пачками по 500 в порядке диска; неверный путь называется сразу, а не ошибкой потока. **`watch(path, { recursive })`** (`notify`, без возможностей по умолчанию кроме `macos_fsevent`) отдает события `create`/`modify`/`remove`/`rename` (с `to` где система его говорит) и `overflow` («события потеряны, посмотрите заново»); события короткого момента (60 мс) собираются и каждое называется один раз; выход из цикла закрывает поток и конец наблюдения. Пути событий и записей — в написании, которым пользовалось приложение (а не канонические); для подмены события — только о записях самого приложения.
- **Известный предел.** Между проверкой пути и операцией ссылку можно подменить (TOCTOU); защита от этого — только обход, где проверка и операция одним вызовом ОС (`openat`-семейство), в M3.1 не делается.

### `store` (data)

```ts
store.get<T>(key): Promise<T | undefined>
store.set(key, value): Promise<void>
store.delete(key), store.keys(prefix?), store.flush()
store.open(name): Promise<Store>     // отдельные области
```

`fjall` (как сейчас в runtime), база — `$APPDATA/store`; значения — JSON. Права не нужны: у каждого приложения (по `id` манифеста) своя папка данных, чужое хранилище недоступно по построению.

- **Области.** `store.open(name)` — отдельное пространство ключей (keyspace базы): имя из латинских букв, цифр, `_` и `-`, до 64 символов, иначе `INVALID_ARGUMENT`; `store.get/set/...` — область `default`. База открывается при первом обращении; область создается при первом обращении к ней.
- **Ответы.** `get` для ключа, которого нет, отвечает `undefined`; сохраненный `null` — это значение и приходит как `null` (в ответе нет поля `value` только у отсутствующего ключа). `set(key, undefined)` — `INVALID_ARGUMENT` до отправки (для этого есть `delete`); `delete` ключа, которого нет, — не ошибка. `keys(prefix?)` — ключи по порядку байтов, не больше 100000 (больше — `INVALID_ARGUMENT`: спросите с префиксом).
- **Пределы.** Ключ — от 1 до 1024 байт; значение в JSON — до 256 KiB (столько несет один вызов; больше — в `fs` или `sqlite`).
- **Надежность.** Запись попадает в журнал базы сразу (переживает закрытие и аварию программы); `flush()` ждет, пока она дошла до диска (переживает и потерю питания). Вторая копия приложения на той же папке данных получает `BUSY` («хранилище занято другим запуском»).

### `sqlite` (data)

```ts
sqlite.open(path, { readonly?, create? }): Promise<Database>    // путь в scope fs
  Database: exec(sql, params?): Promise<{ changes, lastInsertId }>
            query<T>(sql, params?): Promise<T[]>
            iterate<T>(sql, params?): AsyncIterable<T>
            prepare(sql): Promise<Statement>       // run/all/iterate/finalize
            transaction(async (tx) => ...): Promise<R>
            close(), [Symbol.asyncDispose]
```

- **SQLite** — `rusqlite` с `bundled` (тот же SQLite, что у Servo: в дереве один экземпляр, а не системный — поведение одинаково на всех ОС), с возможностью `hooks` (нужна для запрета, см. ниже).
- **Права и места.** База — файл, путь проходит так же, как у `fs.open`: для записи нужны `fs.read` и `fs.write` на этот путь (оба решения пользователя должны совпадать, иначе `PERMISSION_DENIED`), `readonly: true` — только `fs.read`. Где пользователь выбрал «подменить» область, база лежит в папке подмены, настоящая папка не меняется. Файлы журнала SQLite (`-journal`, `-wal`) создает сам SQLite рядом с базой.
- **Поток на соединение.** Соединение живет на своем потоке (SQLite синхронный); то, что просят у соединения, идет через канал и выполняется по очереди, ответы возвращаются в `oneshot`. База и `Statement` — ресурсы сессии документа: закрываются вместе с ним (поток дожидается завершения, файл освобождается). `create` по умолчанию `true`; нет файла при `readonly` или `create: false` — `NOT_FOUND`; папка — `IS_A_DIRECTORY`; не база — `INVALID_ARGUMENT` сразу при открытии.
- **Что запрещено в SQL.** `ATTACH` файла (иначе приложение читало бы и писало любой файл мимо областей) и `load_extension` — отказ `PERMISSION_DENIED`; `VACUUM INTO` в файл тоже отказ; обычный `VACUUM` работает (временная база без файла разрешена). Включен защитный режим SQLite (`SQLITE_DBCONFIG_DEFENSIVE`).
- **Значения.** JSON, а что JSON не держит — тегом из одного ключа: INTEGER больше 2^53 − 1 → `bigint` (`{"$int": "..."}`), BLOB → `Uint8Array` (`{"$blob": "<base64>"}`), бесконечность → `{"$real": "Infinity"}`; остальное — число (INTEGER и REAL не различаются: оба `number`), строка, `null`. Параметры: `?` по списку, `:name`/`@name`/`$name` по объекту (имя без префикса — `:name`); `boolean` — 1/0; `undefined` не значение (`INVALID_ARGUMENT` до отправки: нужен `null`); число больше 64 бит — REAL (целое такого размера говорят `bigint`). Вызов несет до 256 KiB, значит и BLOB-параметр — до примерно 190 KiB (большее — в `fs`).
- **Команды.** `exec` без параметров принимает несколько операторов (`CREATE ...; INSERT ...`), с параметрами — один; отвечает `{ changes, lastInsertId }` (`changes` — строки последнего оператора, что менял строки, и 0, если этот запуск строк не менял). `query` отдает строки объектами (до 100000 и около 32 MiB в ответе, иначе `INVALID_ARGUMENT`: нужен `iterate`); `exec` оператора, что возвращает строки, и `query` оператора без строк — `INVALID_ARGUMENT`. `iterate` — поток пачек (до 500 строк или около 512 KiB в кадре, окно потока — обратное давление): ошибка оператора до первой строки — ошибка вызова, позже — ошибка потока; выход из цикла останавливает чтение. Пока цикл идет, соединение ничего другого не отвечает (`BUSY` на стороне API; внутри тела цикла к этому соединению обращаться нельзя). `prepare(sql)` проверяет текст сразу и дает `Statement` (`run`/`all`/`iterate`/`finalize`), который идет через кэш подготовленных операторов соединения (64).
- **Транзакция.** `transaction(async tx => ...)` — `BEGIN`, работа, `COMMIT`; при исключении `ROLLBACK` и то же исключение дальше. Остальные вызовы к базе, пока транзакция идет, ждут своей очереди (внутри работы пользуйтесь `tx`, а `await` чужого вызова к базе внутри нее зависнет); `tx.prepare` дает оператор для этой транзакции.
- **Ошибки.** Синтаксис, нет таблицы/колонки, нарушенное ограничение — `INVALID_ARGUMENT` с сообщением SQLite о самом операторе (оно про SQL приложения, не про машину) и `details.sqlite` — имя расширенного кода (`SQLITE_CONSTRAINT_UNIQUE`, `..._PRIMARYKEY`, `..._NOTNULL`, `..._CHECK`, `..._FOREIGNKEY`) или число; база занята другим соединением (ждет до 5 с) — `BUSY`; запись в базу, открытую для чтения, — `PERMISSION_DENIED`; остальное о машине — словами runtime.

### `crypto` (data) и `secrets`

- **Проверка WebCrypto (M3.5).** Сценарий e2e `webcrypto` (`tests/e2e/apps/modules/data/webcrypto`; детали проверок под `--verbose` — таблица поддержки) пробует в origin приложения `crypto.subtle` (SHA-1/256/384/512, HMAC, PBKDF2, HKDF, AES-GCM/CBC/CTR/KW, ECDSA/ECDH P-256/384/521, Ed25519, X25519, RSASSA/PSS/OAEP) и `getRandomValues`/`randomUUID`. **Результат: ничего нет.** `isSecureContext` — `false`, а `crypto` не определён вовсе: у крейта `servo` выключена feature `webcrypto` (так записано и в M0.1), а страница идет с непрозрачного origin `native://app` (вариант A из M0.1 — `https://<id>.alef` — не реализован, это задача #65). Поэтому у приложений нет даже `crypto.getRandomValues`. Сценарий ничего не требует и не падает: когда он начнет писать «supported», решение надо пересмотреть (включить WebCrypto и оставить модулю только то, чего в нем нет).
- **Модуль `crypto` (M3.5).** Дает приложениям то, что страница сама не может: `random(length)` (до 1 MiB), `digest`/`hmac`/`hmacVerify` (sha-1, sha-256, sha-384, sha-512; проверка тега за постоянное время), `hkdf`, `pbkdf2` (до 10 млн итераций), `argon2id` (по умолчанию 19 MiB, 2 прохода, 1 дорожка — совет OWASP; до 1 GiB памяти), `scrypt` (по умолчанию N = 2^17, r = 8, p = 1; память `128·N·r` до 1 GiB), `seal`/`open` (AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305: результат — nonce в 12 байт, шифртекст и тег в 16 байт; nonce новый каждый раз; изменение любой части, другой ключ или другой `aad` — `INVALID_ARGUMENT` «не прошло проверку») и `ed25519Generate`/`Sign`/`Verify` (закрытый ключ — зерно в 32 байта). RSA, ECDSA, ECDH и X25519 не даны: их даст WebCrypto, когда будет включен. Данные идут телом вызова, ключи, соли и теги — base64 в аргументах, ответ — байты; выводимый ключ или хеш — от 1 до 1024 байт. Права не нужны: модуль ничего не трогает в машине. Тяжелые хеши паролей (`pbkdf2`, `argon2id`, `scrypt`) идут не более двух разом, остальные ждут очереди. Реализация — `ring` (в дереве) и `argon2`, `scrypt` (новые зависимости); проверено опубликованными векторами (RFC 4231, 5869, 7914, 8032, GCM) и OpenSSL из Node в e2e (сценарий `crypto`: runner считает ожидаемое, страница сравнивает; что страница запечатала, runner открывает сам).
- `secrets.get(service, account)`, `set`, `delete` — хранилище ОС (Windows Credential Manager, macOS Keychain, Linux Secret Service) через `keyring`; право `permissions.secrets: true`.

## Перевод File Manager

1. Список каталога: `nativeApi.listDirectory` → `fs.readDir` + `fs.stat`; корень — из `app.args()` (`--root`) или `path.home()`; ограничение корнем — scope `fs.read` в манифесте File Manager.
2. Настройки языка: `preferences.get/set` → `store.get/set('language')`; миграция существующих данных Fjall (тот же формат или одноразовый перенос).
3. Приветствие `hello` и событие `backend.greeting` — удалить или заменить на `app.info`.
4. `apps/file-manager/alef.ktav`: окна, `external` закрыт, `fs.read` — корни.
5. Удалить `backend/src`; File Manager запускается генерическим `alef --app apps/file-manager`.

## Структура кода

```
alef-modules/src/data/   mod.rs, fs/ (mod.rs, ops.rs, handle.rs, watch.rs, scope.rs), store.rs, sqlite/ (mod.rs, worker.rs, values.rs), crypto.rs, secrets.rs
packages/api/src/data/   fs.ts, store.ts, sqlite.ts, crypto.ts, secrets.ts (или secrets внутри crypto.ts — по правилу 7 элементов)
```

## Приёмка

| Проверка | Как |
|---|---|
| Чтение/запись текст/байты, handle с потоками, копирование 1 GiB файла `pipeTo` без роста памяти | e2e |
| Выход за scope (`..`, symlink наружу) → `PERMISSION_DENIED` | Rust unit + e2e |
| Грант из `dialog.open` даёт чтение выбранного файла вне scope только в этой сессии | e2e (полуручной) |
| `watch` получает create/modify/remove | e2e |
| `store` переживает перезапуск | e2e |
| `sqlite`: схема, транзакция с откатом, prepared, iterate 100k строк без роста памяти | e2e |
| WebCrypto: таблица поддерживаемых алгоритмов задокументирована; argon2/secrets работают | e2e |
| File Manager на `fs`/`store` работает как раньше; `backend/src` удалён | ручная + e2e смоук |

## Риски

- Нормализация путей и регистр на Windows/macOS — источник обходов scope; тесты на каждую ОС.
- Linux Secret Service может отсутствовать (headless, минимальные DE) → `NOT_AVAILABLE`.
