# Alef: JS API фреймворка — план

> Детальный план реализации (архитектура, контракт модулей, этапы M0–M7, приёмка) — `FRAMEWORK-PLAN.md`. Этот файл — исходный каталог и открытые решения.

## Модель

Приложение = web-ассеты + манифест. Весь код пользователя работает в JS внутри встроенного Servo. Runtime (Rust) предоставляет нативный API через приватный bridge `native://invoke` и проверяет права из манифеста. Пользователь Rust не пишет; runtime распространяется предкомпилированными бинарниками через npm (пакет на платформу через `optionalDependencies`, как у esbuild).

Пакеты:

- `@alef-tron/api` — типизированный клиент (TS), тонкая обёртка над транспортом;
- `@alef-tron/tools` — `dev`, `build`, упаковка/инсталлятор;
- `@alef-tron/runtime-<platform>-<arch>` — бинарник runtime.

## Что установлено в коде (на 2026-10-05)

- Транспорт: `fetch('native://invoke/')`, JSON, capability-токен в Bearer, лимит тела 256 KiB, ответ только целиком (`ResponseBody::Done`), события из Rust — через `evaluate_javascript` (только JSON).
- CSP приложения: `default-src native:; connect-src native:` — **любой** `fetch`/`WebSocket` наружу из страницы заблокирован. Сетевой API должен быть нативным (или CSP должна задаваться манифестом).
- Servo 0.6 содержит DOM для `SubtleCrypto`, `WebSocket`, `MediaDevices`, `AudioContext`, но в сборке используется `servo-media-dummy` (GStreamer не подключён): **камера, микрофон, Web Audio, `<audio>/<video>` в браузере не работают**.
- В дереве зависимостей уже есть `rusqlite`, `arboard` (clipboard), `hyper`, `rustls`, `ring`/`aws-lc-rs`, `fjall` — часть API реализуется без новых тяжёлых зависимостей.
- Команды приложения сейчас регистрируются Rust-кодом (`backend/src/backend.rs`); для фреймворка их заменит встроенный API runtime.

## Фаза 0 — фундамент (до любых новых API)

Без этого fs, сокеты, http и медиа не сделать корректно.

1. **Бинарный транспорт.** `invoke` принимает/возвращает `ArrayBuffer`; лимиты на команду, а не глобальные 256 KiB.
2. **Потоки и каналы.** Подписки с backpressure для сокетов, fs.watch, чтения/записи больших файлов, http-стриминга, медиа. Варианты: streaming-ответ протокольного handler'а (проверить поддержку в Servo 0.6 `servo-net`), либо канал через long-poll `native://`. Выбор — по замеру.
3. **Ресурсы-дескрипторы.** Открытые файлы, сокеты, БД, процессы — id в runtime; автоматическое закрытие при перезагрузке/закрытии документа или окна.
4. **Ошибки.** Типизированные коды (`ENOENT`, `EACCES`, `PERMISSION_DENIED`, `TIMEOUT`, …), одинаковые во всех namespace.
5. **Права (manifest).** Capabilities + scopes: fs-пути, сетевые хосты/порты, разрешённые процессы, камера/микрофон (с системным запросом пользователю). Проверка только в Rust.
6. **Мультиоконность.** Токен на окно, события между окнами (`broadcast`/адресно).
7. **Манифест** `alef.ktav` (Ktav 0.8): имя, id, версия, иконки, окна по умолчанию, права, CSP.

## Каталог API

Соглашения: namespace = модуль `@alef-tron/api`, всё асинхронное, `AbortSignal` последним аргументом, события через `on(name, cb) → unlisten`.

| Namespace | Содержимое | Реализация |
|---|---|---|
| `app` | info/version, quit, relaunch, single-instance, lifecycle-события, autostart, deep links (custom protocol), file associations | winit + платформенный код |
| `path` | appData, appConfig, cache, home, temp, documents, downloads, desktop, exe | `dirs` (уже есть) |
| `window` | create/close/list/getCurrent, bounds, min/max size, center, fullscreen, always-on-top, focus, show/hide, title, icon, decorations, resizable, drag/resize, zoom, события (move, resize, focus, close-requested с отменой) | расширение текущего `runtime.window` |
| `dialog` | open file(s)/folder, save, message/confirm | `rfd` |
| `shell` | openExternal (URL), openPath, showItemInFolder, moveToTrash | `opener`/`trash` |
| `menu` | меню приложения, контекстное меню, accelerators | `muda` |
| `tray` | иконка, tooltip, меню, клики | `tray-icon` |
| `globalShortcut` | регистрация системных горячих клавиш | `global-hotkey` |
| `clipboard` | text, html, image | `arboard` (уже есть) |
| `notification` | системные уведомления, клики | `notify-rust`/WinRT |
| `screen` | мониторы, work area, scale, позиция курсора | winit |
| `os` | platform, arch, version, locale, hostname, тема (тёмная/светлая) + событие смены, sleep/resume, idle | платформенный код |
| `dnd` | перетаскивание файлов из ОС в окно | winit `DroppedFile` |
| `fs` | read/write (text/bytes/stream), stat, readDir, mkdir, rm, rename, copy, exists, watch, temp-файлы — в пределах scopes | tokio fs, `notify` |
| `sqlite` | open/close, exec, query, prepared statements, транзакции, batch | `rusqlite` (уже есть) |
| `store` | простое key-value хранилище настроек | `fjall` (уже есть) |
| `http` | request без CORS: стриминг тела, таймауты, прокси, редиректы, cookies; download/upload с прогрессом | `hyper` + `rustls` (уже есть) |
| `net` | TCP client/server, UDP, TLS, WebSocket client | tokio, `rustls`, `tokio-tungstenite` |
| `crypto` | WebCrypto уже в Servo (`crypto.subtle`) — проверить покрытие; дополнительно: argon2/scrypt, ed25519/x25519 при пробелах; **secure storage** (Keychain/Credential Manager) | `ring`, `keyring` |
| `cli` | командная строка ОС: exec через оболочку, spawn с stdio-потоками, env, cwd, kill, exit code; sidecar-бинарники приложения | tokio process |
| `media` | камера, микрофон, воспроизведение и запись аудио | см. «Открытые вопросы» |
| `updater` | проверка/загрузка/установка обновлений | позже |
| `log` | логирование в файл, crash-репорты | `tracing` |
| `devtools` | отладка страницы | Servo devtools (Firefox protocol) |

## Порядок фаз

1. **Фаза 0** — транспорт, потоки, дескрипторы, ошибки, права, манифест, `@alef-tron/api` каркас.
2. **Фаза 1 — ядро десктопа:** `app`, `path`, `window` (мультиоконность), `dialog`, `shell`, `clipboard`, `os`, `screen`, `dnd`.
3. **Фаза 2 — данные:** `fs`, `sqlite`, `store`, `crypto` + secure storage.
4. **Фаза 3 — сеть и процессы:** `http`, `net`, `cli`.
5. **Фаза 4 — системная интеграция:** `menu`, `tray`, `globalShortcut`, `notification`, autostart, deep links.
6. **Фаза 5 — медиа:** камера, микрофон, аудио.
7. **Фаза 6 — дистрибуция:** `@alef-tron/tools`, npm-пакеты с бинарниками по платформам, инсталляторы, `updater`.

Критерий готовности каждого API: TS-типы в `@alef-tron/api`, проверка прав в Rust, unit-тесты Rust, e2e-проверка из страницы в реальном окне.

## Открытые вопросы

> Решения от 2026-10-05 (медиа отложено, все популярные платформы, CSP задаёт пользователь, генерация типов из Rust) — в `FRAMEWORK-PLAN.md` §10. Ниже — исходная формулировка.

1. **Медиа.** (а) Подключить GStreamer-backend Servo — тогда работают стандартные `getUserMedia`, Web Audio, `<video>`, но приложение тянет GStreamer runtime (~100+ MB на Windows); (б) нативно (`nokhwa` для камеры, `cpal` для аудио) с передачей кадров/сэмплов в страницу через бинарный канал — легче, но нестандартный API.
2. ~~Лицензия~~ — решено: весь репозиторий `MIT OR Apache-2.0`; патчи Servo (MPL-2.0) и winit (Apache-2.0) сохраняют свои лицензии.
3. **Платформы.** Сейчас патчи (winit, resize) — под Windows. Какие ОС в первом релизе?
4. **CSP.** Разрешать ли приложению через манифест прямой `fetch`/`WebSocket` наружу (стандартные web API) в дополнение к нативному `http`/`net`.
5. **Web storage.** У `native://` opaque origin — `localStorage`, `sessionStorage`, IndexedDB в приложении не работают (см. `TRANSPORT.md`). Патчить Servo ради tuple origin или ограничиться `store`/`sqlite`.
