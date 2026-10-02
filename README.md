# AlefFileManager

Самостоятельный проект: встроенный Servo 0.6, Rust/Tokio backend, TypeScript/React, Rsbuild, Tailwind, Oxlint, i18next и Fjall. Frontend — небольшой Hello World с реальным вызовом Rust и сохранением языка (ru/en/he, RTL для иврита).

## Запуск

Проверено на Windows x64: Rust 1.97.0 (MSVC), Node.js 24.12.0, npm 11.13.0. Для native сборки нужны MSVC C++ Build Tools/Windows SDK и зависимости сборки Servo (CMake, Ninja, Python, LLVM). Linux/macOS используют кроссплатформенные API Servo/winit, но здесь не проверены запуском.

```sh
npm ci
npm run dev
```

`dev` собирает Rust, запускает Rsbuild на `127.0.0.1:3000`, затем открывает native окно. React обновляется через HMR. После изменений Rust перезапустите `dev`. Закрытие окна останавливает принадлежащий launcher процесс Rsbuild; занятый порт — ошибка, не автоматическая замена сервера.

```sh
npm run build
npm start
npm run check
npm run test:rust
```

- `build`: TypeScript/Rsbuild production frontend и Rust workspace.
- `start`: ранее собранный native executable; Rsbuild не нужен.
- `check`: Oxlint, TypeScript и Clippy всего workspace с ошибкой на warnings.
- `test:rust`: Rust-тесты приложения и runtime.
- `dev:web`: только asset server; обычный браузер не имеет доступа к native bridge.

Аргументы передаются через `--`, например:

```sh
npm start -- --data-dir ./local-data --root ./files
```

По умолчанию Fjall находится в локальном каталоге данных пользователя `AlefFileManager/fjall`. `--frontend-dir` выбирает production assets; `--frontend-url` разрешает только HTTP frontend на `127.0.0.1` для разработки.

## Структура и повторное использование

- `backend/runtime`: независимый crate `alef-runtime`; Servo window host, приватный bridge, typed async command registry, универсальный async Fjall facade.
- `backend/src`: только запуск Alef и его команды/настройки.
- `frontend/src/runtime.ts`: универсальный Promise API `invoke<T>(command, arguments, signal)` без зависимости от React или команд Alef.
- `frontend/src/api.ts`: DTO и команды приложения поверх `invoke`.
- `backend/patches/servo-paint-api`: локальный Servo 0.6 fix активации GL context до загрузки GL функций; версия не изменена.

Другой binary в Rust workspace может зависеть от `alef-runtime`, зарегистрировать собственные команды через `Commands::register`, вызвать async `Bridge::new`, передать название/размер/icon в `WindowOptions`, открыть окно через `run` и после его закрытия await `Bridge::shutdown`. Команды принимают/возвращают serde-типы и `Future<Output = io::Result<T>>`; код приложения не реализует transport. GUI event loop выполняется на main thread; команды — на Tokio workers. При переносе crate в другой workspace нужно сохранить root Cargo patch для `servo-paint-api` и настройку TLS provider из native entry point.

`Store::open(path, keyspace)`, `get`, `insert`, `remove`, `flush`, `close` — async. Синхронные Fjall handles приватны; операции выполняются через awaited `tokio::task::spawn_blocking`. `insert/remove` не обещают fsync: для durability нужен `flush`. Уже запущенная blocking операция может завершиться после отмены вызывающего Future. Закрывайте владельцев через async `close` либо lifecycle bridge, чтобы освобождение базы не пришлось на UI thread.

## Browser/backend transport

`native://invoke/` обрабатывается зарегистрированным внутри Servo `ProtocolHandler`. Fetch/Promise здесь не означает HTTP transport: запрос остаётся внутри процесса, socket/listener не создаётся. Production assets приходят из `native://app/`; в dev HTTP/WebSocket используются только Rsbuild assets/HMR, не командами приложения.

Servo работает с `multiprocess: false`, а `ipc-channel` — с `force-inprocess`: внутренние каналы также остаются в памяти. Схема не регистрируется как системный URL handler. Другому процессу не предоставляется TCP port, named pipe или иной endpoint для подключения к backend. Вызовы дополнительно требуют случайный 256-bit capability текущего bridge; capability не выводится в startup log. Одновременно допускается 32 native команды, JSON request ограничен 256 KiB. Drop protocol future abort-ит принадлежащую ему async task. `AbortSignal` не следует считать rollback команды: уже начатый Fjall I/O может закончиться после отмены ожидания frontend.

Это не защита от debugger/process injection, чтения памяти с соответствующими правами или изменения доверенного frontend на диске. Production CSP запрещает внешние scripts/frames/connect destinations. Доступ к файловым директориям и assets ограничен их заданными корнями.

Windows smoke: production без Rsbuild, native React/Tailwind, переключение Hebrew RTL → English, восстановление English из Fjall после restart; у native executable наблюдались 0 TCP listeners и 0 дочерних процессов. Native dev smoke подтвердил React HMR и тот же приватный Rust bridge.
