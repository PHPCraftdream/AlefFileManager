# Alef Framework — план реализации модулей и API

Документ — рабочий план. Опирается на `API-ROADMAP.md` (каталог и решения) и `TRANSPORT.md` (транспорт и результаты spike).

## 1. Цель и критерий готовности

Alef — десктопный фреймворк: пользователь пишет приложение на HTML/CSS/JS(TS), запускает его во встроенном Servo и получает нативные возможности через `@alef-tron/api`. Rust пользователь не пишет; runtime распространяется готовыми бинарниками через npm.

**MVP готов, когда:**

1. `npm create alef-tron` → `npm run dev` → `npm run build` даёт работающее приложение под Windows, macOS и Linux без установленного Rust.
2. Из JS доступны модули этапов M1–M5 (§8) с типами, правами и тестами на всех трёх ОС.
3. Alef File Manager переписан на `@alef-tron/api` без собственного Rust-кода — это доказательство, что API достаточно для реального приложения.

**Не цели MVP:** Node.js внутри runtime, мобильные платформы, печать, медиа (камера, микрофон, аудио — M6 отложен), обновление Servo с 0.6.

## 2. Архитектура

```
┌──────────────────────────── приложение пользователя (JS/TS) ────────────────────────────┐
│                         @alef-tron/api  (типизированные модули)                               │
│        core: transport · streams · events · resources · errors · handshake               │
└───────────────▲─────────────────────────────────────────────────────────────▲───────────┘
   native://call, native://stream (fetch)                       события (поток документа)
┌───────────────┴──────────────── alef-runtime (Servo + winit) ───────────────┴───────────┐
│ bridge: протокол native://, перехват https-origin приложения, CSP                        │
│ windows: окна/webview, resize, ввод · ui-dispatcher: выполнение на main thread          │
└───────────────▲──────────────────────────────────────────────────────────────────────────┘
                │ trait UiDispatcher, Registry
┌───────────────┴──────────────── alef-modules ───────────────────────────────────────────┐
│ desktop · system · data · net · devices  (реализации API, без Servo)                    │
└───────────────▲──────────────────────────────────────────────────────────────────────────┘
┌───────────────┴──────────────── alef-core (без Servo) ──────────────────────────────────┐
│ протокол кадров · credit · сессии документов · таблица ресурсов · реестр команд        │
│ права и scopes · манифест · коды ошибок · версия протокола                              │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

Ключевые решения:

- **`alef-core` и `alef-modules` не зависят от Servo.** Их тесты собираются и идут за секунды, а не после сборки Servo; модули проверяются без окна.
- **Всё, что требует главного потока** (окна, меню, трей, глобальные хоткеи), модули делают через `UiDispatcher` — runtime выполняет замыкание в цикле winit и возвращает результат. Остальное — на Tokio.
- **Генерический бинарник `alef`** читает манифест и ассеты приложения; ни одно приложение не компилирует Rust.

## 3. Раскладка репозитория (с учётом правила «≤ 7 элементов, ≤ 700 строк»)

```
backend/
  Cargo.toml            workspace + [patch.crates-io]
  patches/              servo-paint, servo-paint-api, winit (как сейчас)
  crates/
    alef-core/          src/{lib.rs, protocol/, session/, security/, registry/, error.rs}
    alef-modules/       src/{lib.rs, desktop/, system/, data/, net/, devices/}
    alef-runtime/       src/{lib.rs, bridge/, window/, ui/, dispatch/}   ← бывший backend/runtime
    alef/               src/main.rs  — генерический бинарник runtime
packages/
  api/                  @alef-tron/api
  tools/                @alef-tron/tools: dev, build, bundle
  create-alef-tron/      шаблон проекта
apps/
  file-manager/         Alef File Manager как обычное JS-приложение + alef.ktav
tests/
  e2e/                  страницы-сценарии + раннер (заменит experiments/transport-spike)
```

`backend/src` (команды File Manager на Rust) исчезает после миграции в M3. Группировка модулей одинакова в Rust и JS: `desktop`, `system`, `data`, `net`, `devices`.

## 4. Контракт модуля (Rust)

```rust
pub trait Module: Send + Sync + 'static {
    /// Пространство имён в JS, например "fs".
    const NAMESPACE: &'static str;
    fn register(registry: &mut Registry) -> Result<(), RegistryError>;
}

// Регистрация команды: имя, требуемое право, обработчик.
registry
    .command("fs.readFile")
    .permission(Permission::FsRead)          // scope извлекается из аргументов
    .handler(|ctx: CallContext, args: ReadFile| async move { ... });
```

`CallContext` даёт обработчику:

- `session()` — документ-владелец (окно + загрузка); ресурсы, открытые в вызове, принадлежат ему;
- `scope_check(permission, target)` — проверка scope (путь, хост, программа) по манифесту и выданным диалогами грантам;
- `streams()` — открыть поток наружу (`StreamWriter`) или внутрь (`StreamReader`) с credit-контролем;
- `resources()` — положить/взять ресурс (файл, сокет, БД, процесс) по id;
- `events()` — отправить событие документу, окну или всем;
- `ui()` — `UiDispatcher` для операций главного потока.

Ответ обработчика: `Json(T)`, `Bytes(Vec<u8>)` (отдаётся чанками) или `Stream(id)`.

## 5. Контракт `@alef-tron/api` (JS)

```
packages/api/src/
  index.ts
  core/        transport.ts, stream.ts, events.ts, resource.ts, errors.ts, handshake.ts
  desktop/     app, window, dialog, shell, menu, tray, shortcut
  system/      clipboard, notification, screen, os, path, cli
  data/        fs, store, sqlite, crypto
  net/         http, socket, websocket
  devices/     camera, microphone, audio
```

Соглашения:

- импорт по модулю: `import { fs, window } from '@alef-tron/api'`;
- **всё асинхронное, без исключений:** каждая функция возвращает `Promise`, поток или `AsyncIterable`; синхронных геттеров нет даже для «статичных» данных (`os.platform()`, `app.args()`) — всё идёт через транспорт. Rust-обработчики — `async`, блокирующий I/O только в `spawn_blocking`, UI-операции — через асинхронный `UiDispatcher`. Проверяется автоматически: тест `@alef-tron/api` обходит все экспортируемые функции модулей и требует `Promise`/`AsyncIterable`;
- последний аргумент — `{ signal?: AbortSignal }`;
- потоки — стандартные `ReadableStream`/`WritableStream` и `AsyncIterable`;
- ресурсы — классы с `close()` и `Symbol.asyncDispose` (`await using file = await fs.open(...)`);
- события — `module.on(name, callback, { signal }) → unlisten`;
- ошибки — `AlefError { code, message, details }`, `code` из единого списка;
- рукопожатие при старте: `runtime.hello` → версия протокола, платформа, доступные модули и права; несовместимая версия — понятная ошибка, отсутствующий модуль — `NOT_AVAILABLE`.

Пример целевого API:

```ts
import { dialog, fs, http, window } from '@alef-tron/api';

const [path] = await dialog.open({ filters: [{ name: 'Text', extensions: ['txt'] }] });
const text = await fs.readText(path);                 // путь выдан диалогом → грант
await using out = await fs.open(`${path}.bak`, { write: true, create: true });
const response = await http.request('https://example.com/big.bin');
await response.body.pipeTo(out.writable);             // поток с backpressure
await window.current().setTitle(`Saved ${path}`);
```

## 6. Сквозные механизмы (делаются один раз, используются всеми модулями)

| Механизм | Суть | Где |
|---|---|---|
| Транспорт v2 | `native://call/<cmd>` (JSON/бинарный, ответ чанками), `native://stream/<id>` (кадры, credit 1 MiB), `stream.write/end/close/ack` | core/protocol, runtime/bridge |
| Сессии документов | токен на загрузку документа; при новой загрузке/навигации/закрытии окна — закрытие всех ресурсов и потоков | core/session, runtime/window |
| Ресурсы | таблица id → объект с владельцем-сессией, явное и автоматическое закрытие | core/session |
| События | поток событий документа; адресация: документ, окно, broadcast | core, runtime |
| Права | манифест: capabilities + scopes; гранты от диалогов (выбранный файл/папка доступны, даже если вне scope); запрос пользователю для камеры/микрофона | core/security |
| Манифест | `alef.ktav` (Ktav 0.8): id, имя, версия, окна, иконки, права, внешние ресурсы; типизированная проверка через serde, разделы безопасности обязательны (§6.1) | core/security |
| Ошибки | `io::ErrorKind` и ошибки модулей → коды (`NOT_FOUND`, `PERMISSION_DENIED`, `INVALID_ARGUMENT`, `TIMEOUT`, `CLOSED`, `BUSY`, `NOT_AVAILABLE`, `INTERNAL`) | core/error |
| Потоки исполнения | UI-операции через `UiDispatcher` на главном потоке, остальное на Tokio; блокирующий I/O через `spawn_blocking` | runtime/dispatch |
| Типы JS ↔ Rust | DTO описываются в Rust, TS-типы генерируются (`ts-rs`, MIT) в `packages/api/types/`; ручные типы — только для обёрток | core, api |

### 6.1. Манифест и безопасность

Приложение целиком собирает и поставляет сам разработчик, поэтому права — его решение, а не навязанные фреймворком ограничения. Их задача — защитить пользователя, если в документ попадёт чужой код (XSS, скомпрометированная зависимость): такой код получит не больше, чем разработчик явно объявил.

Принципы:

- **Разделы безопасности обязательны.** `permissions` и `external` должны присутствовать в манифесте явно. Неявных значений по умолчанию в runtime нет: если раздела нет, приложение не запускается — ошибка `MANIFEST_INVALID` с путём к недостающему полю.
- **По умолчанию закрыто.** Пустой список = запрещено. Шаблон `create-alef-tron` содержит эти разделы с закрытыми значениями, чтобы разработчик видел их и открывал осознанно.
- **Внешние ресурсы** — и обращения (`fetch`, `WebSocket`, `EventSource`), и подключение (скрипты, стили, изображения, шрифты, медиа, фреймы) — задаются в `external`; runtime строит из них CSP документов приложения. Транспорт Alef (`native://call`/`stream`) от `external` не зависит.
- **Нативные возможности** (`fs`, `cli`, `net`, `shell`, `clipboard.read`, `shortcut.global`, `secrets`, …) — в `permissions` со scopes. Сетевой доступ из нативного `http`/`socket` тоже ограничен своими scopes, независимо от CSP.

```
## alef.ktav — манифест приложения (Ktav 0.8)
id: com.example.notes
name: Notes
version:: 1.0.0

windows: [
    {
        label: main
        url: /
        width: 70%work
        height: 80%work
        minWidth: 800
        minHeight: 600
        monitor: cursor
        position: center
        restore: true
    }
]

## Обязательный раздел. Пусто = закрыто.
external: {
    connect: []
    load: {
        scripts: []
        styles: []
        images: []
        fonts: []
        media: []
        frames: []
    }
}

## Обязательный раздел. Пусто/false = закрыто. Все подразделы обязательны.
permissions: {
    fs: {
        read: []
        write: []
    }
    cli: {
        exec: []
    }
    net: {
        http: []
        socket: []
    }
    shell: {
        openExternal: []
    }
    clipboard: {
        read: false
    }
    shortcut: {
        global: false
    }
    secrets: false
    app: {
        env: []
    }
}
```

Открытие — перечислить разрешённое (непустые списки в Ktav всегда многострочные):

```
external: {
    connect: [
        https://api.example.com
    ]
    load: { ... }
}
permissions: {
    cli: {
        exec: [
            git
            ffmpeg
        ]
    }
    ...
}
```

Проверено разбором (`alef-core`, `tests/fixtures/minimal.ktav`, `open.ktav`):

- **Запятых в Ktav нет.** `{ label: main, url: / }` разбирается без ошибки, но запятая становится частью значения (`"main,"`) — один ключ на строку, у окна — многострочный объект.
- Точечные ключи (`cli.exec: []`) допустимы, но смешивать их с вложенной формой для одного корня нельзя (`duplicate key`); в шаблоне используется вложенная форма.
- `version: 1.0` и `version: 1.0.0` читаются как строки; `::` нужен для значений, которые Ktav выведет как число/ключевое слово.
- `windows: 3` — ошибка типа (`expected array`), `restore: yes` — не bool.

`*` в `cli.exec` — любые программы, только явно.

Формат и проверка манифеста:

- Файл `alef.ktav` в корне приложения, формат **Ktav 0.8** (crate `ktav = "=0.8.0"`, npm `@ktav-lang/ktav@0.8.0`; MIT OR Apache-2.0).
- Runtime читает манифест через serde (`ktav::from_str`) в типизированную структуру: `deny_unknown_fields`, у разделов безопасности нет `#[serde(default)]` — отсутствующий раздел даёт `MANIFEST_INVALID` с именем поля (`missing field ...`). Строка и колонка (0-based байтовая колонка) есть только у синтаксических ошибок Ktav; у serde-ошибок типа полного пути поля нет — ограничение `ktav 0.8`.
- Та же структура — источник TS-типов манифеста (`ts-rs`); `@alef-tron/tools` проверяет манифест при `dev`/`build` через `@ktav-lang/ktav` до запуска runtime.
- Версии и прочие значения, похожие на числа/ключевые слова, пишутся через `::` (`version:: 1.0.0`), иначе Ktav выведет тип.

### 6.2. Размер и положение окна (MVP)

Одинаково в манифесте (`windows`) и в JS (`window.create`, `setSize`, `setPosition`):

| Параметр | Значения |
|---|---|
| `width`, `height` | число — логические пиксели (не зависят от DPI); `N%screen` — от всего дисплея; `N%work` — от рабочей области дисплея (без панели задач, Dock, панелей Linux) |
| `minWidth`, `minHeight`, `maxWidth`, `maxHeight` | те же единицы |
| `monitor` | `primary` (по умолчанию) или `cursor` — монитор под курсором; от него считаются проценты |
| `position` | `center` (по умолчанию — по центру рабочей области) или явные `x`, `y` в тех же единицах |
| `restore` | `true` — запомнить размер, положение и maximize между запусками; если сохранённое положение вне доступных мониторов, окно открывается по правилам выше |

Проценты вычисляются при создании окна и при вызове `setSize`/`setPosition`, а не отслеживаются постоянно. TS-тип: `` number | `${number}%screen` | `${number}%work` ``.

Позже, вне MVP: физические пиксели, `%parent`, `mm/in/pt`, `em`, `auto` (по содержимому), `clamp/min/max`, `aspectRatio`, другие точки выравнивания.

## 7. Каталог модулей

Обозначения: **UI** — нужен главный поток; размер S/M/L — относительная трудоёмкость.

| Группа | Модуль | Ключевой API | Реализация | Право | Размер |
|---|---|---|---|---|---|
| desktop | `app` | `info()`, `quit()`, `relaunch()`, `requestSingleInstance()`, события `ready/before-quit/second-instance`; аргументы запуска (`args()` с разбором по схеме манифеста, `--help`/`--version`), env, cwd, аргументы второго экземпляра; stdin/stdout/stderr приложения потоками, `exit(code)`, режим без окна (консольные утилиты на Alef) | winit, named mutex, std::env, tokio stdio, `AttachConsole` на Windows | — | M |
| desktop | `window` | `create(opts)` с размерами в px/`%screen`/`%work`, `monitor`, `position: center`, `restore` (§6.2), `current()`, `all()`, bounds, min/max size, center, fullscreen, always-on-top, focus, show/hide, title, icon, decorations, resizable, drag/resize, zoom; события move/resize/focus/close-requested (отменяемое), drop файлов | runtime/window, мультиоконность | `window.create` | L |
| desktop | `dialog` | `open`, `save`, `pickFolder`, `message`, `confirm` | `rfd` (async), родитель — HWND окна | — (выдаёт гранты) | S |
| desktop | `shell` | `openExternal(url)`, `openPath`, `showInFolder`, `trash` | `opener`, `trash` | `shell.*` + scope | S |
| desktop | `menu` | меню окна/приложения, контекстное меню, accelerators, события | `muda` (UI) | — | M |
| desktop | `tray` | иконка, tooltip, меню, клики | `tray-icon` (UI) | — | M |
| desktop | `shortcut` | глобальные хоткеи | `global-hotkey` (UI) | `shortcut.global` | S |
| system | `clipboard` | text, html, image (read/write) | `arboard` (уже в дереве) | `clipboard.read` | S |
| system | `notification` | показ, действия, клики | WinRT toast / `notify-rust` | — | S |
| system | `screen` | мониторы, work area, scale, курсор | winit (UI) | — | S |
| system | `os` | platform, arch, version, locale, hostname, тема + событие смены, sleep/resume | платформенный код | — | S |
| system | `path` | appData, config, cache, temp, home, documents, downloads, desktop, exe | `dirs` (уже есть) | — | S |
| system | `cli` | Командная строка ОС из JS (цепочка JS → Servo → фреймворк → ОС): `exec(commandLine, { shell, cwd, env, timeout })` → `{ stdout, stderr, code }`; `spawn(program, args)` → процесс со stdin/stdout/stderr потоками, `kill`, `wait`, коды выхода; PTY для терминальных приложений; sidecar-бинарники приложения | tokio process, `portable-pty` | `cli.exec` + scope разрешённых программ (`"*"` — только явным opt-in в манифесте) | M |
| data | `fs` | read/write text/bytes/stream, `open` (handle), stat, readDir, mkdir, rm, rename, copy, exists, watch, temp | tokio fs, `notify` | `fs.read/write` + scope путей | L |
| data | `store` | key-value настроек приложения | `fjall` (уже есть) | — (своя область) | S |
| data | `sqlite` | open/close, exec, query, prepared, transaction, batch | `rusqlite` (уже в дереве), отдельный поток на БД | `sqlite` + scope путей | M |
| data | `crypto` | WebCrypto Servo — проверить покрытие; добавить: argon2/scrypt, ed25519/x25519, random; `secrets` — хранилище ОС | `ring`/`aws-lc-rs` (в дереве), `keyring` | `secrets` | M |
| net | `http` | `request` без CORS: потоковые тела, таймауты, прокси, редиректы, cookies; download/upload с прогрессом | `hyper` + `rustls` (в дереве) | `net.http` + scope хостов | M |
| net | `socket` | TCP client/server, UDP, TLS | tokio, `rustls` | `net.socket` + scope host:port | M |
| net | `websocket` | клиент | `tokio-tungstenite` | `net.http` scope | S |
| devices | `camera`, `microphone`, `audio` | см. решение в M6 | GStreamer-backend Servo **или** `nokhwa`/`cpal` | `media.*` + запрос пользователю | L |
| platform | `log`, `devtools`, `updater` | логи в файл, crash-репорты; Servo devtools; обновления | `tracing`, Servo devtools, позже | — | S/S/L |

## 8. Этапы

Подробное описание каждого этапа — отдельный файл в `stages/` (цель, объём, API, структура кода, порядок работ, приёмка, риски). Каждый этап закрывается только с доказательствами: Rust unit-тесты модулей (без Servo), сценарии `tests/e2e` на реальном окне, TS-типы, CI на трёх ОС.

| Этап | Файл | Суть | Зависит от |
|---|---|---|---|
| M0 | `stages/m0-spikes.md` | 4 spike: origin приложения (M0.1), мультиоконность (M0.2), трей/меню/хоткеи/диалоги с циклом winit (M0.3), CI на Windows/macOS/Linux (M0.4) | — |
| M1 | `stages/m1-core.md` | crates, транспорт v2, сессии и ресурсы, реестр с правами, `alef.ktav`, ts-rs, `@alef-tron/api` core, бинарник `alef` | M0 |
| M2 | `stages/m2-desktop.md` | `app`, `path`, `window` (§6.2), `dialog`, `shell`, `clipboard`, `os`, `screen`, `notification` | M1 |
| M3 | `stages/m3-data.md` | `fs`, `store`, `sqlite`, `crypto`/`secrets`; перевод File Manager, удаление `backend/src` | M2 |
| M4 | `stages/m4-net-cli.md` | `http`, `socket`, `websocket`, `cli` (командная строка ОС), консольный режим `app` | M3 |
| M5 | `stages/m5-integration.md` | `menu`, `tray`, `shortcut`, автозапуск, deep links | M2 |
| M6 | `stages/m6-media.md` | медиа — **отложено**, вне MVP | — |
| M7 | `stages/m7-distribution.md` | `@alef-tron/tools`, npm-бинарники, `create-alef-tron`, установщики, `updater` | M3, M0.4 |

## 9. Тестирование

| Уровень | Что | Инструмент |
|---|---|---|
| Rust unit | протокол, credit, сессии, права/scopes, каждый модуль | `cargo test` (`alef-core`, `alef-modules` — без Servo) |
| TS unit | разбор кадров, ack, abort, ошибки, обёртки | встроенный `node --test` (Node 24 исполняет TS без зависимостей) |
| e2e | сценарии на реальном окне, отчёт в stderr, автозакрытие | `tests/e2e` + раннер; режим `ALEF_TEST=1` |
| Структура | ≤ 7 элементов, ≤ 700 строк | `npm run lint:structure` |

## 10. Решения

Приняты (2026-10-05):

1. **Медиа — отложено.** M6 вне MVP; к выбору GStreamer/нативный API вернуться позже.
2. **Платформы MVP — все популярные:** Windows x64, macOS (arm64, x64), Linux x64 (X11 и Wayland). Следствия: CI собирает и тестирует на трёх ОС начиная с M1; платформенный код только в `window/platform` и модулях за `cfg`; resize-синхронизация и интеграция трея/меню проверяются на каждой ОС отдельно (у macOS свой live resize, у Linux — X11/Wayland).
3. **Внешние ресурсы и права определяет разработчик** в манифесте (§6.1): раздел `external` (обращения и подключение внешних ресурсов → CSP) и `permissions` обязательны, по умолчанию закрыты; без них приложение не запускается. Транспорт Alef от `external` не зависит.
4. **Генерация TS-типов из Rust — делаем** (`ts-rs`, MIT) с M1: DTO команд и событий описываются в Rust, типы `@alef-tron/api` генерируются, CI проверяет, что сгенерированное совпадает с закоммиченным.
5. **npm-scope — `@alef-tron/*`**: `@alef-tron/api`, `@alef-tron/tools` (команды `dev`/`build`/`bundle`; не `cli`, чтобы не путать с модулем API `cli` — командной строкой ОС), `@alef-tron/runtime-<platform>-<arch>`, шаблон — `create-alef-tron` (`npm create alef-tron`). Пакетов с такими именами в npm нет (проверено 2026-10-05); организацию `alef-tron` нужно зарегистрировать до публикации.
6. **Формат конфигурации — Ktav 0.8** (`alef.ktav`), см. §6.1.

## 11. Риски

- **Сборка Servo** — часы с нуля; CI-кэш и sccache обязательны; патчи upstream (servo-paint, winit) нужно поддерживать при обновлениях.
- **Совместимость веба в Servo 0.6** — части web API нет или они неполные; заранее проверять на реальном File Manager и шаблоне.
- **Мультиоконность и GL** — подтверждается в M0.
- **Безопасность** — API доступен только документам приложения (origin/токен), не удалённому контенту; scopes канонизируются (symlink, `..`).
- **Размер приложения** — Servo-бинарник ~118 MB (release); медиа может добавить ещё 100+ MB.
