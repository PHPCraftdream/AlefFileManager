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
- `check`: Oxlint, лимиты структуры, TypeScript и Clippy всего workspace с ошибкой на warnings.
- `lint:structure`: в нашем коде (`frontend/src`, `backend/src`, `backend/crates`, `scripts`, `packages`, `experiments`) не более 7 элементов в папке и не более 700 строк в файле (`scripts/check-structure.mjs`).
- `test:rust`: Rust-тесты приложения и runtime.
- `dev:web`: только asset server; обычный браузер не имеет доступа к native bridge.

Аргументы передаются через `--`, например:

```sh
npm start -- --data-dir ./local-data --root ./files
```

По умолчанию Fjall находится в локальном каталоге данных пользователя `AlefFileManager/fjall`. `--frontend-dir` выбирает production assets; `--frontend-url` разрешает только HTTP frontend на `127.0.0.1` для разработки.

## Структура и повторное использование

- `backend/crates/alef-runtime`: независимый crate `alef-runtime`; Servo window host, приватный bridge, typed async command registry, события backend → browser, window API и универсальный async Fjall facade.
- `backend/src`: только запуск Alef и его команды/настройки.
- `frontend/src/native/runtime.ts`: универсальные `invoke`, `listen`/`Unlisten`, `nativeWindow` без зависимости от React или команд Alef.
- `frontend/src/native/api.ts`: DTO и команды приложения поверх `invoke`.
- `backend/patches/servo-paint-api`: локальный Servo 0.6 fix активации GL context до загрузки GL функций; версия не изменена.
- `backend/patches/surfman`: surfman 0.13.0 без `WS_VISIBLE` у служебного окна `SurfmanFalseWindow` (на Windows оно создаётся при первом GL-контексте для загрузки расширений WGL и мигало на экране ~30 мс при каждом запуске); версия не изменена, лицензии сохранены.
- `backend/patches/winit`: winit 0.30.13 с Windows fixes для packed signed coordinates в `WM_NCLBUTTONDOWN`, чтения maximized state из HWND и drag lifecycle по `WM_ENTERSIZEMOVE`/`WM_EXITSIZEMOVE`. Это устраняет зависший drag guard после maximize/restore; версия не изменена, Apache-2.0 license сохранена. [Win32 LPARAM контракт](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-nclbuttondown).
- `backend/crates/alef-runtime/src/window`: Servo window host (`app`, `state`, `delegate`), синхронный resize (`resize_wait`), ввод (`input/`: wheel, edge hit-testing); `backend/crates/alef-runtime/src/bridge`: приватный protocol bridge и `commands`.
- `backend/crates/alef-runtime/src/window/platform`: Windows/Linux/macOS реализации icon, wheel policy и native resize capability; общий runtime не содержит платформенных FFI.

Другой binary в Rust workspace может зависеть от `alef-runtime`, зарегистрировать собственные команды через `Commands::register`, вызвать async `Bridge::new`, создать `WindowOptions::new(title, icon_png)` и открыть окно через `run`; после закрытия нужно await `Bridge::shutdown`. Default options: 1200×800 logical pixels, `decorations = true`, `resizable = true`; поля можно переопределить перед `run`. Handler получает serde-аргументы и `RuntimeHandle`, возвращает `Future<Output = io::Result<T>>`; код приложения не реализует transport. `Bridge::handle()` позволяет передавать тот же async handle собственным backend задачам. GUI event loop выполняется на main thread; команды — на Tokio workers. При переносе crate в другой workspace нужно сохранить root Cargo patches для `servo-paint-api` и `winit`, а также настройку TLS provider из native entry point.

`Store::open(path, keyspace)`, `get`, `insert`, `remove`, `flush`, `close` — async. Синхронные Fjall handles приватны; операции выполняются через awaited `tokio::task::spawn_blocking`. `insert/remove` не обещают fsync: для durability нужен `flush`. Уже запущенная blocking операция может завершиться после отмены вызывающего Future. Закрывайте владельцев через async `close` либо lifecycle bridge, чтобы освобождение базы не пришлось на UI thread.

## События и подписки

Rust handler может генерировать типизированные события из Tokio:

```rust
commands.register("hello", |(): (), context| async move {
    let payload = serde_json::json!({ "process_id": std::process::id() });
    context.emit("backend.greeting", &payload).await?;
    Ok(payload)
})?;
```

Browser подписывается до вызова команды:

```typescript
const unlisten = listen<{ process_id: number }>("backend.greeting", payload => {
  console.log(payload.process_id);
});
await invoke("hello");
unlisten();
```

`listen` возвращает идемпотентную отписку; можно передать `{ signal }` для автоматической отписки через `AbortController`. Несколько подписчиков независимы. События отправляются в текущую страницу через Servo JavaScript evaluator и DOM `CustomEvent`; polling, timers, HTTP/WebSocket для событий отсутствуют. Payload сериализуется serde JSON, не интерполируется как пользовательский JS source. Это live broadcast без replay/persistence: событие без активного подписчика не сохраняется. `emit().await` подтверждает dispatch, не завершение асинхронной бизнес-логики browser callback.

UI admission ограничен 64 операциями, включая уже переданные в JS и ожидающие callback. Сериализация события начинается после admission; event JSON ограничен 256 KiB. Permit освобождается после ответа или уничтожения callback. После закрытия окна сохранённые backend handles получают `BrokenPipe`. Namespace `runtime.*` зарезервирован для встроенных команд и событий.

## Window API и custom titlebar

`WindowOptions.decorations = false` скрывает системный заголовок. React `TitleBar` демонстрирует собственные кнопки minimize/maximize/restore/close, drag-зону и double-click maximize. В maximized состоянии кнопка restore показывает два перекрывающихся квадрата. Resize edges обрабатывает native host, а не DOM overlays: 8 logical pixels с учётом DPI, corners имеют приоритет. `resizable = false` отключает native edge hit-testing и OS resize; по умолчанию resize включён.

```typescript
const stopWatching = await nativeWindow.watch(state => {
  console.log(state.width, state.height, state.maximized);
});
const snapshot = await nativeWindow.getState();
await nativeWindow.maximize();
await nativeWindow.restore();
await nativeWindow.setDecorations(false);
await nativeWindow.setResizable(false);
stopWatching();
```

Также доступны `minimize`, `toggleMaximize`, `close`, `startDrag`, `startResize(edge)`, `setResizable(enabled)`; Rust использует `RuntimeHandle::window(WindowAction)`. Все запросы проходят capability-защищённый bridge и bounded UI queue; OS/winit/Servo операции выполняются только на native main thread, не из Tokio workers. Если ожидающий Future отменён до исполнения, queued запрос пропускается; уже применённая OS операция не откатывается.

Window state содержит revision, title, client size, outer position, scale factor, focus, maximized/minimized/visible, decorations, resizable/fullscreen и capability `supportsDragResize`. Недоступные платформенные значения — `null`, не выдуманные defaults. `watch` сначала подписывается, затем читает snapshot; revision предотвращает перезапись свежего события устаревшим RPC ответом. Native изменения публикуются событиями `runtime.window.state`; промежуточные состояния coalesced, одновременно отправляется не более одного state event. Отправка начинается после готовности JS-контекста страницы, без polling.

`startDrag`/`startResize` вызываются на `pointerdown` при нажатой основной кнопке мыши. Backend проверяет реальный native input state. На macOS winit не поддерживает `drag_resize_window`: capability false, error не скрывается и fake fallback отсутствует. Linux/macOS runtime запуском здесь не проверены. Завершение window command означает применение native запроса; итоговое OS состояние приходит через `watch`, не предполагается по результату setter.

При resize host берёт текущий physical client size из native окна, а не устаревший размер queued события. Servo rendering surface проверяется на точное совпадение с client size; после resize старый frame не отправляется на новую surface до уведомления Servo о готовности frame. Resize cursor удерживается до завершения native drag. Это не обещание нулевой задержки асинхронного Servo layout/render.

Windows maximize использует доступную OS work area, не весь monitor: client origin совпадает с верхней границей work area. Зарезервированная Windows область над ней не является padding приложения; runtime не меняет глобальную work area. Для window/taskbar задаются обе иконки Windows, SMALL и BIG.

Wheel line deltas нормализуются в device pixels до передачи Servo 0.6: reference step — 76 logical pixels при стандартной line policy, с масштабированием по DPI и чтением Windows scroll lines/chars. Disabled scrolling и page scrolling сохраняются; pixel deltas не масштабируются повторно.

Hello World генерирует настоящее `backend.greeting` при вызове Rust; UI показывает количество событий, позволяет отписаться/подписаться, переключить native titlebar и включить/выключить resizing. Синтетического фонового event generator нет.

## Browser/backend transport

`native://invoke/` обрабатывается зарегистрированным внутри Servo `ProtocolHandler`. Fetch/Promise здесь не означает HTTP transport: запрос остаётся внутри процесса, socket/listener не создаётся. Production assets приходят из `native://app/`; в dev HTTP/WebSocket используются только Rsbuild assets/HMR, не командами приложения.

Servo работает с `multiprocess: false`, а `ipc-channel` — с `force-inprocess`: внутренние каналы также остаются в памяти. Схема не регистрируется как системный URL handler. Другому процессу не предоставляется TCP port, named pipe или иной endpoint для подключения к backend. Вызовы дополнительно требуют случайный 256-bit capability текущего bridge; capability не выводится в startup log. Одновременно допускается 32 native команды, JSON request ограничен 256 KiB. Drop protocol future abort-ит принадлежащую ему async task. `AbortSignal` не следует считать rollback команды: уже начатый Fjall I/O может закончиться после отмены ожидания frontend.

Это не защита от debugger/process injection, чтения памяти с соответствующими правами или изменения доверенного frontend на диске. Production CSP запрещает внешние scripts/frames/connect destinations. Доступ к файловым директориям и assets ограничен их заданными корнями.

Windows smoke: production без Rsbuild, native React/Tailwind, переключение Hebrew RTL → English, восстановление English из Fjall после restart; у native executable наблюдались 0 TCP listeners и 0 дочерних процессов. Native dev smoke подтвердил React HMR и тот же приватный Rust bridge.

## Лицензия

`MIT OR Apache-2.0` — на выбор пользователя, см. `LICENSE-MIT` и `LICENSE-APACHE`. Исключения: `backend/patches/servo-paint`, `backend/patches/servo-paint-api` и файлы `backend/crates/alef-runtime/src/window/` с заголовком MPL-2.0 (на основе примера Servo) — MPL-2.0; `backend/patches/winit` — Apache-2.0; `backend/patches/surfman` — `MIT OR Apache-2.0 OR MPL-2.0` (лицензии оригинала, `LICENSE-*` в каталоге).
