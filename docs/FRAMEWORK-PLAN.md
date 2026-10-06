# Alef Framework — план реализации модулей и API

Документ — рабочий план. Опирается на `API-ROADMAP.md` (каталог и решения) и `TRANSPORT.md` (транспорт и результаты spike).

## 1. Цель и критерий готовности

Alef — десктопный фреймворк: пользователь пишет приложение на HTML/CSS/JS(TS), запускает его во встроенном Servo и получает нативные возможности через `@alef-tron/api`. Rust пользователь не пишет; runtime распространяется готовыми бинарниками через npm.

**MVP готов, когда:**

1. `npm create alef-tron` → `npm run dev` → `npm run build` даёт пакет приложения `.alef`, который запускает установленный runtime (`alef run app.alef`) под Windows, macOS и Linux без Rust. Установщик со встроенным runtime (единая сборка) — необязательная опция (§6.3).
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
- **Runtime ставится отдельно, как JRE (§6.3).** Приложение — пакет `.alef` (zip: `alef.ktav`, ассеты, подпись); лаунчер находит установленный runtime нужной версии (несколько версий стоят рядом). Единая сборка — опция.
- **Три режима одного бинарника:** окно (`windows` не пуст), консоль (`windows: []` + `console: true`: stdin/stdout, код выхода) и служба (без окна и консоли, долгоживущие серверы). Безоконные режимы исполняют документ приложения в скрытом `WebView` на программном контексте отрисовки (подтверждает спайк M0.5).
- **Права подтверждает пользователь и может подменить (§6.4):** разрешить, подменить (приложение не отличает среду-двойник от настоящей) или отказать.

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
| Права | манифест: capabilities + scopes; решение пользователя по каждому праву — разрешить / подменить / отказать (§6.4); гранты от диалогов (выбранный файл/папка доступны, даже если вне scope, и при подменённой `fs`); запрос пользователю для камеры/микрофона | core/security, runtime |
| Пакет и лаунчер | пакет `.alef` (zip + подпись), `alef run`/`install`, выбор версии runtime по манифесту, идентичность приложения (§6.3) | alef, alef-core |
| Манифест | `alef.ktav` (Ktav 0.8): id, имя, версия, окна, иконки, права, внешние ресурсы; типизированная проверка через serde, разделы безопасности обязательны (§6.1) | core/security |
| Ошибки | `io::ErrorKind` и ошибки модулей → коды (`NOT_FOUND`, `PERMISSION_DENIED`, `INVALID_ARGUMENT`, `TIMEOUT`, `CLOSED`, `BUSY`, `NOT_AVAILABLE`, `INTERNAL`) | core/error |
| Потоки исполнения | UI-операции через `UiDispatcher` на главном потоке, остальное на Tokio; блокирующий I/O через `spawn_blocking` | runtime/dispatch |
| Типы JS ↔ Rust | DTO описываются в Rust, TS-типы генерируются (`ts-rs`, MIT) в `packages/api/types/`; ручные типы — только для обёрток | core, api |

### 6.1. Манифест и безопасность

Права объявляет разработчик, а **решает пользователь** (§6.4): он видит каждое право и может его разрешить, подменить или отказать; манифест не навязывает ограничения, а сообщает, чего хочет приложение. Их задача — защитить пользователя, если в документ попадёт чужой код (XSS, скомпрометированная зависимость): такой код получит не больше, чем разработчик явно объявил.

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
    ## Необязательный раздел. Нет раздела = окна в рантайме создавать нельзя (`window.create` — PERMISSION_DENIED).
    window: {
        create: true
    }
}

## Необязательный раздел. Нет раздела = приложение не принимает аргументов.
arguments: {
    options: [
        {
            name: port
            short: p
            type: number
            description: Port to listen on
        }
    ]
    positional: {
        name: files
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
- `arguments` (необязателен): `options` (обязателен внутри раздела) — `name` (строчные буквы, цифры, дефис; `help` и `version` генерируются), необязательные `short` (один ASCII-символ; `h` и `V` заняты) и `description`, `type` — `string`/`number`/`boolean`; `positional` — `name` и `description`, без него лишний аргумент — ошибка использования. Разбор: `--name value`, `--name=value`, `-s value`, `-s=value`; флаг значения не берёт (`--flag`, `--flag=false`); `--` заканчивает опции; повтор — побеждает последнее значение; кластеров коротких опций нет. Ошибка → код выхода 2, `--help`/`-h`/`--version`/`-V` печатают текст и выходят с кодом 0 до открытия окна.

### 6.2. Размер и положение окна (MVP)

Одинаково в манифесте (`windows`) и в JS (`window.create`, `setSize`, `setPosition`):

| Параметр | Значения |
|---|---|
| `width`, `height` | число — логические пиксели (не зависят от DPI); `N%screen` — от всего дисплея; `N%work` — от рабочей области дисплея (без панели задач, Dock, панелей Linux) |
| `minWidth`, `minHeight`, `maxWidth`, `maxHeight` | те же единицы |
| `monitor` | `primary` (по умолчанию) или `cursor` — монитор под курсором; от него считаются проценты |
| `position` | `center` (по умолчанию — по центру рабочей области) или явные `x`, `y` в тех же единицах |
| `restore` | `true` — запомнить размер, положение и maximize между запусками; если сохранённое положение вне доступных мониторов, окно открывается по правилам выше (M2.4; пока манифест с `restore: true` отклоняется) |
| `title`, `decorations`, `resizable` | необязательные: заголовок (по умолчанию имя приложения), рамка и кнопки ОС (по умолчанию да), изменение размера пользователем (по умолчанию да) |

Размеры окна — размеры клиентской области. `x`, `y`: пиксели — координаты рабочего стола, проценты отсчитываются от начала выбранного дисплея (`%screen`) или его рабочей области (`%work`); `center` — по центру рабочей области выбранного дисплея с учётом рамки. Логические пиксели дисплея — физические, делённые на его масштаб: на рабочем столе с дисплеями разного масштаба прямоугольники дисплеев состыкованы неточно, а положение, переданное в пикселях, приблизительно. Размер и положение, которые окно получило, читаются через `state()` (`WindowInfo`, те же единицы).

Проценты вычисляются при создании окна и при вызове `setSize`/`setPosition`, а не отслеживаются постоянно. TS-тип: `` number | `${number}%screen` | `${number}%work` ``.

Позже, вне MVP: физические пиксели, `%parent`, `mm/in/pt`, `em`, `auto` (по содержимому), `clamp/min/max`, `aspectRatio`, другие точки выравнивания.

### 6.3. Модель поставки: runtime отдельно, приложение — пакет

- **Runtime** (`alef`) ставится в систему один раз своим установщиком (для разработки — ещё и npm-пакетами `@alef-tron/runtime-*`). Версии лежат рядом: `<корень Alef>/<версия>/alef`; в системе один лаунчер `alef`, он выбирает версию по манифесту (как `javaw`/`py`). Обновление runtime не ломает установленные приложения: старые версии остаются, лишние убирает `alef runtime prune`.
- **Пакет `.alef`** — zip: `alef.ktav` в корне, ассеты, `icon.png`, `META-INF/` (список хешей файлов и подпись ed25519 разработчика). Ассеты читаются из архива без распаковки на диск (источник ассетов моста — каталог или архив; каталог остаётся для `dev`).
- **Совместимость.** Манифест задаёт `runtime: ^X.Y`; лаунчер запускает подходящую установленную версию или сообщает, что поставить; `runtime.hello` проверяет протокол и API. Несколько версий рядом — потому что до 1.0 minor может ломать API.
- **Запуск и установка.** `alef run app.alef -- <аргументы>` (и двойной клик по `.alef`) — запуск на месте. `alef install app.alef` (или согласие при первом запуске) копирует пакет в каталог приложений и создаёт идентичность приложения при общем бинарнике: Windows — ярлык с AppUserModelID; macOS — тонкая обёртка `.app` (`Info.plist`: id, имя, значок, описания TCC), запускающая `alef run`; Linux — `.desktop`. Ассоциации файлов, deep links и автозапуск регистрируются как `alef run <пакет> ...`.
- **Единая сборка** (необязательная): `alef-tron bundle` кладёт runtime рядом с пакетом и собирает обычный установщик; приложение ставится одним файлом и не зависит от общего runtime.
- **Режимы** — окно, консоль, служба (§2): определяются манифестом. Службы ОС (Windows Service, systemd, launchd) регистрирует установка или автозапуск (M5); сам runtime в режиме службы не требует консоли и корректно завершается по сигналам.

Подробности — `stages/m7-distribution.md`.

### 6.4. Подтверждение и подмена прав

- Права объявляет разработчик, **решает пользователь**. Перед первым запуском runtime показывает встроенное окно согласия (страница runtime, код приложения в нём не исполняется) со всеми правами из манифеста по отдельности: каждый путь `fs`, адрес `net`, слушающий порт, объявленная команда `cli`, переменная `app.env`, прочие права.
- Три исхода для каждого права: **разрешить** (настоящий доступ), **подменить** (приложение получает правдоподобную среду и не может отличить её от настоящего поведения), **отказать** (`PERMISSION_DENIED`). За права, которых нет в манифесте, по-прежнему `PERMISSION_DENIED` — это ошибка разработчика, а не решение пользователя. Приложение должно переносить любую подмену.
- Решения хранятся у пользователя по приложению: `id` и отпечаток ключа подписи (пакет с тем же `id`, но другим ключом чужих решений не наследует). Новые права в обновлении спрашиваются заново. Менять решения можно в любой момент: `alef permissions list|set|reset <app>` и экран настроек runtime.

Подмена по модулям (что приложение наблюдает):

| Право | Подмена |
|---|---|
| `net` (`http`, `socket`, `websocket`) и **собственные загрузки Servo** (`fetch`, изображения и т. д. из `external`) | соединение висит до таймаута, как при мёртвой сети |
| `listen` (серверы: `http.serve`, `websocket.serve`, `socket.listen`, MCP) | `listen()` успешен и выдаёт порт, но сокет не открыт и входящих нет |
| `fs` | виртуализация: каждый объявленный scope отображается на теневой каталог приложения; чтение видит его содержимое (сначала пусто), запись успешна, но остаётся в тени; пути вне scope — как прежде `PERMISSION_DENIED` |
| `cli` | приложение **заранее объявляет** фиксированные команды (`permissions.cli.commands`: имя, программа, шаблон аргументов, описание); для каждой пользователь выбирает разрешить / подменить (зависает до таймаута) / отказать; вне списка команд нет; `*` (любые) — отдельное подтверждение с предупреждением |
| `clipboard.read`, `app.env`, `secrets`, `shortcut.global` | пустой буфер; переменная не задана; теневое хранилище секретов; регистрация «успешна», но не срабатывает |

Границы (честно):

- подмена не даёт криптографической неотличимости: тайминги и сравнение каналов её выдают (например, сеть подменена, а разрешённая команда ходит в сеть по-настоящему);
- разрешённая команда `cli` видит настоящую систему — окно согласия говорит это прямо;
- явный выбор пользователя в диалоге файлов (`dialog`, M2.3) даёт реальный доступ к выбранному файлу даже при подменённой `fs`: это действие пользователя, а не право из манифеста.

Механика: `PermissionSet::check` возвращает `Allow | Substitute | Deny`, решение попадает в `CallContext`, у каждого модуля есть ветка подмены (тестируется без Servo). Порядок работ и приёмка — `stages/m2b-consent.md`.

## 7. Каталог модулей

Обозначения: **UI** — нужен главный поток; размер S/M/L — относительная трудоёмкость.

| Группа | Модуль | Ключевой API | Реализация | Право | Размер |
|---|---|---|---|---|---|
| desktop | `app` | `info()`, `quit()`, `relaunch()`, `requestSingleInstance()`, события `ready/before-quit/second-instance`; аргументы запуска (`args()` с разбором по схеме манифеста, `--help`/`--version`), env, cwd, аргументы второго экземпляра; stdin/stdout/stderr приложения потоками, `exit(code)`, режим без окна (консольные утилиты на Alef) | winit, named mutex, std::env, tokio stdio, `AttachConsole` на Windows | — | M |
| desktop | `window` | `create(opts)` с размерами в px/`%screen`/`%work`, `monitor`, `position: center`, `restore` (§6.2), `current()`, `all()`, bounds, min/max size, center, fullscreen, always-on-top, focus, show/hide, title, icon, decorations, resizable, drag/resize, zoom; события moved/resized/focus/blur/close-requested (отменяемое), drop файлов | runtime/window, мультиоконность | `window.create` (`permissions.window.create`) | L |
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
| net | `http` | `request` без CORS: потоковые тела, таймауты, прокси, редиректы, cookies; download/upload с прогрессом; **сервер** `serve`: запросы потоком, upgrade в WebSocket, статика | `hyper` + `rustls` (в дереве) | `net.http` + scope хостов | M |
| net | `socket` | TCP client/server, UDP, TLS | tokio, `rustls` | `net.socket` + scope host:port; серверы — scope `listen:host:port` (по умолчанию только loopback) | M |
| net | `websocket` | клиент и сервер (`serve`, либо `upgrade` из `http.serve`) | `tokio-tungstenite` | `net.http` scope (клиент), `listen` (сервер) | S |
| net | `mcp` (`@alef-tron/mcp`, JS-пакет поверх модулей) | MCP-сервер (tools/resources/prompts; stdio и Streamable HTTP) и клиент | JSON-RPC в JS поверх `http`, `websocket`, `cli` | права `http.serve`/`listen`, `cli`, `net` | M |
| devices | `camera`, `microphone`, `audio` | см. решение в M6 | GStreamer-backend Servo **или** `nokhwa`/`cpal` | `media.*` + запрос пользователю | L |
| platform | `log`, `devtools`, `updater` | логи в файл, crash-репорты; Servo devtools; обновления | `tracing`, Servo devtools, позже | — | S/S/L |

## 8. Этапы

Подробное описание каждого этапа — отдельный файл в `stages/` (цель, объём, API, структура кода, порядок работ, приёмка, риски). Каждый этап закрывается только с доказательствами: Rust unit-тесты модулей (без Servo), сценарии `tests/e2e` на реальном окне, TS-типы, CI на трёх ОС.

| Этап | Файл | Суть | Зависит от |
|---|---|---|---|
| M0 | `stages/m0-spikes.md` | 5 spike: origin приложения (M0.1), мультиоконность (M0.2), трей/меню/хоткеи/диалоги с циклом winit (M0.3), CI на Windows/macOS/Linux (M0.4), безоконный режим Servo (M0.5) | — |
| M1 | `stages/m1-core.md` | crates, транспорт v2, сессии и ресурсы, реестр с правами, `alef.ktav`, ts-rs, `@alef-tron/api` core, бинарник `alef` | M0 |
| M2 | `stages/m2-desktop.md` | `app`, `path`, `window` (§6.2), `dialog`, `shell`, `clipboard`, `os`, `screen`, `notification` | M1 |
| M2b | `stages/m2b-consent.md` | подтверждение и подмена прав (§6.4): исход «подменить», хранилище решений, окно согласия, `alef permissions` | M2 |
| M3 | `stages/m3-data.md` | `fs` (с виртуализацией), `store`, `sqlite`, `crypto`/`secrets`; перевод File Manager, удаление `backend/src` | M2b |
| M4 | `stages/m4-net-cli.md` | `http` (клиент и сервер), `socket`, `websocket` (клиент и сервер), `cli` (командная строка ОС, объявленные команды), режимы `app`: консоль и служба | M3, M0.5 |
| M4b | `stages/m4b-mcp.md` | серверы и MCP: `@alef-tron/mcp` (сервер и клиент), безопасность локальных серверов | M4 |
| M5 | `stages/m5-integration.md` | `menu`, `tray`, `shortcut`, автозапуск, deep links | M2 |
| M6 | `stages/m6-media.md` | медиа — **отложено**, вне MVP | — |
| M7 | `stages/m7-distribution.md` | M7a runtime и лаунчер (установщики, версии рядом), M7b пакеты `.alef` (подпись, `alef install`, `@alef-tron/tools`, `updater`), M7c единая сборка (опция) | M3, M2b, M0.4 |

## 9. Тестирование

| Уровень | Что | Инструмент |
|---|---|---|
| Rust unit | протокол, credit, сессии, права/scopes, каждый модуль | `cargo test` (`alef-core`, `alef-modules` — без Servo) |
| TS unit | разбор кадров, ack, abort, ошибки, обёртки | встроенный `node --test` (Node 24 исполняет TS без зависимостей) |
| e2e | сценарии на реальном окне, отчёт в stderr, автозакрытие | `tests/e2e` + раннер; режим `ALEF_TEST=1` |
| Структура | ≤ 7 элементов, ≤ 700 строк | `npm run lint:structure` |

Тестовые запуски окон не должны мешать разработчику. На Windows и macOS раннер e2e включает **тихий режим** (`ALEF_E2E_QUIET=1`, только вместе с `ALEF_E2E=1`): окна создаются и грузятся, но система их не показывает и фокус не отдаёт; размер, положение, пределы, заголовок, zoom, события и закрытие — настоящие, а видимость, фокус, maximize/minimize и fullscreen — собственный учёт рантайма (иначе `set_maximized` скрытого окна показало бы его). Раннер не запускает бинарник без тихого режима. `ALEF_E2E_VISIBLE=1` показывает окна. Сценарий `startup` смотрит на настоящие окна, поэтому на Windows запускается только с `ALEF_E2E_VISIBLE=1`. На Linux окна живут на `xvfb` и настоящие.

## 10. Решения

Приняты (2026-10-05):

1. **Медиа — отложено.** M6 вне MVP; к выбору GStreamer/нативный API вернуться позже.
2. **Платформы MVP — все популярные:** Windows x64, macOS (arm64, x64), Linux x64 (X11 и Wayland). Следствия: CI собирает и тестирует на трёх ОС начиная с M1; платформенный код только в `window/platform` и модулях за `cfg`; resize-синхронизация и интеграция трея/меню проверяются на каждой ОС отдельно (у macOS свой live resize, у Linux — X11/Wayland).
3. **Внешние ресурсы и права определяет разработчик** в манифесте (§6.1): раздел `external` (обращения и подключение внешних ресурсов → CSP) и `permissions` обязательны, по умолчанию закрыты; без них приложение не запускается. Транспорт Alef от `external` не зависит.
4. **Генерация TS-типов из Rust — делаем** (`ts-rs`, MIT) с M1: DTO команд и событий описываются в Rust, типы `@alef-tron/api` генерируются, CI проверяет, что сгенерированное совпадает с закоммиченным.
5. **npm-scope — `@alef-tron/*`**: `@alef-tron/api`, `@alef-tron/tools` (команды `dev`/`build`/`bundle`; не `cli`, чтобы не путать с модулем API `cli` — командной строкой ОС), `@alef-tron/runtime-<platform>-<arch>`, шаблон — `create-alef-tron` (`npm create alef-tron`). Пакетов с такими именами в npm нет (проверено 2026-10-05); организацию `alef-tron` нужно зарегистрировать до публикации.
6. **Формат конфигурации — Ktav 0.8** (`alef.ktav`), см. §6.1.

Приняты (2026-10-06):

7. **Поставка как у Java:** runtime ставится отдельно (несколько версий рядом, лаунчер выбирает по `runtime` в манифесте), приложение — пакет `.alef`; единая сборка — необязательная опция (§6.3).
8. **Пакет — zip с расширением `.alef`**, подпись ed25519 над списком хешей внутри; ассеты читаются из архива.
9. **Запуск на месте и установка:** `alef run` и `alef install` (ярлык, идентичность приложения, ассоциации).
10. **Права подтверждает пользователь и может подменить любое** (§6.4): подмена, а не отказ; решения хранятся по `id` и ключу подписи; для `cli` приложение заранее объявляет фиксированные команды. Ядро согласия — отдельный этап M2b перед `fs`.
11. **Серверы и MCP:** `http.serve`, `websocket.serve`, scope `listen:host:port` (по умолчанию только loopback, проверка `Host`/`Origin`); MCP — JS-пакет `@alef-tron/mcp` поверх модулей (M4b).
12. **Безоконные режимы** (консоль, служба) — через скрытый `WebView` на программном контексте; сначала спайк M0.5.

## 11. Риски

- **Сборка Servo** — часы с нуля; CI-кэш и sccache обязательны; патчи upstream (servo-paint, winit) нужно поддерживать при обновлениях.
- **Совместимость веба в Servo 0.6** — части web API нет или они неполные; заранее проверять на реальном File Manager и шаблоне.
- **Мультиоконность и GL** — подтверждается в M0.
- **Безопасность** — API доступен только документам приложения (origin/токен), не удалённому контенту; scopes канонизируются (symlink, `..`).
- **Размер приложения** — Servo-бинарник ~118 MB (release); медиа может добавить ещё 100+ MB.
- **Идентичность приложения без своего бинарника:** на macOS нужна `.app`-обёртка на каждое приложение (подпись, нотаризация); на Windows AUMID — только через ярлык (при запуске на месте уведомления общие). См. M7b.
- **Подмена прав** не даёт полной неотличимости (тайминги, сравнение каналов); обещаем «нет кода ошибки и нет разницы в данных». Приложение, не переносящее подмену, — ответственность разработчика.
- **Безоконный режим на машинах без дисплея** (Linux-сервер без X/Wayland, служба Windows в session 0) не проверен: спайк M0.5.
- **Серверы в приложении** идут через мост Servo: подходят для API, MCP и локальных инструментов, не для высокой нагрузки.
