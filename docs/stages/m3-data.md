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

`fjall` (как сейчас в runtime), область — `$APPDATA/store`. Значения — JSON. Без права: каждое приложение видит только своё хранилище.

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

- `rusqlite` (уже в дереве Servo; проверить features — bundled SQLite нужен для одинакового поведения на всех ОС).
- Отдельный поток на соединение (rusqlite синхронный), команды через канал; `Statement` — ресурс сессии.
- Типы: INTEGER → number/bigint (по размеру), REAL, TEXT, BLOB → Uint8Array, NULL.

### `crypto` (data) и `secrets`

- Сначала проверить WebCrypto Servo 0.6 (`crypto.subtle`, `getRandomValues`): SHA-*, HMAC, AES-GCM/CBC/CTR, PBKDF2, HKDF, ECDSA/ECDH (P-256/384), Ed25519/X25519, RSA. Работает ли в нашем origin (secure context, M0.1).
- `@alef-tron/api/crypto` — только то, чего нет в WebCrypto: `argon2id`, `scrypt`, при пробелах — Ed25519/X25519; реализация — `argon2`, `scrypt`, `ring`/`aws-lc-rs` (в дереве).
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
