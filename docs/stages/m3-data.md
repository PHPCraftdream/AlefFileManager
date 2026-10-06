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
