# M2 — Десктоп

Обзор: `../FRAMEWORK-PLAN.md`. Зависит от M1.

## Цель

Базовые модули десктопного приложения: `app`, `path`, `window`, `dialog`, `shell`, `clipboard`, `os`, `screen`, `notification`. После этапа на Alef можно сделать обычное однооконное/многооконное приложение с диалогами и системными интеграциями первого уровня.

Все функции асинхронные, последний аргумент — `{ signal? }`. Типы DTO генерируются из Rust.

## Модули

### `app` (desktop)

```ts
app.info(): Promise<{ id, name, version, runtimeVersion }>
app.quit(code?: number): Promise<void>
app.relaunch(): Promise<void>
app.args(): Promise<{ raw: string[]; parsed: Record<string, unknown>; positional: string[] }>
app.env(name?: string): Promise<string | Record<string, string> | undefined>
app.cwd(): Promise<string>
app.requestSingleInstance(): Promise<boolean>     // true — мы первый экземпляр
app.on('second-instance', ({ args, cwd }) => ...)
app.on('before-quit', (event) => event.preventDefault())
```

- Разбор аргументов по разделу `arguments` манифеста (флаги, опции с типами string/number/boolean, позиционные, `--help`/`--version` генерируются); реализация — небольшой разборщик на основе `lexopt` либо `clap` builder, выбор по размеру зависимости.
- `app.env` — право `app.env` со списком имён (разделы `permissions.app.env: [HOME, LANG]`); без права — `PERMISSION_DENIED`.
- Single-instance: Windows — именованный mutex + named pipe для передачи аргументов; macOS/Linux — lock-файл + Unix socket в `$APPCACHE`. Это локальный endpoint пользователя, доступ только для владельца.

### `path` (system)

```ts
path.appData() | appConfig() | appCache() | temp() | home() | documents() | downloads() | desktop() | executable(): Promise<string>
path.join(...parts), path.normalize(p), path.dirname(p), path.basename(p) — тоже Promise (принцип «всё асинхронное»)
```

Реализация — `dirs`. Каталоги приложения — `<системный>/<app.id>`.

### `window` (desktop, UI)

```ts
window.current(): Promise<AppWindow>
window.all(): Promise<AppWindow[]>
window.create(options: WindowOptions): Promise<AppWindow>
AppWindow: label, setTitle, setSize, setPosition, center, minimize, maximize, restore, toggleMaximize,
  setFullscreen, setAlwaysOnTop, setResizable, setDecorations, setMinSize, setMaxSize, show, hide, focus,
  close, destroy, startDrag, startResize(edge), setZoom, state(), on('moved'|'resized'|'focus'|'blur'|'close-requested'|'file-drop', ...)
```

- Геометрия — `../FRAMEWORK-PLAN.md` §6.2: px, `%screen`, `%work`, min/max, `monitor: primary|cursor`, `position: center|x,y`, `restore`.
- `close-requested` отменяемое (`event.preventDefault()` в JS, runtime ждёт ответа документа с таймаутом).
- Drop файлов: `file-drop` с путями; пути получают грант на чтение в текущей сессии.
- Мультиоконность — по результату M0.2; `window.create` требует права `window.create` (в манифесте — `permissions.window.create: true`).
- `restore`: состояние хранится во внутреннем `store` приложения, проверка доступности монитора.

### `dialog` (desktop)

```ts
dialog.open({ title?, filters?, multiple?, directory?, defaultPath? }): Promise<string[]>   // [] — отмена
dialog.save({ title?, filters?, defaultPath? }): Promise<string | null>
dialog.message({ title?, message, kind?: 'info'|'warning'|'error' }): Promise<void>
dialog.confirm({ title?, message, okLabel?, cancelLabel? }): Promise<boolean>
```

- `rfd` async, родитель — окно вызова; поведение проверено в M0.3.
- Выбранные пути получают гранты сессии: `open` — чтение (для папки — рекурсивно), `save` — запись файла. Гранты не переживают закрытие сессии.

### `shell` (desktop)

```ts
shell.openExternal(url): Promise<void>      // право shell.openExternal + scope URL-шаблонов
shell.openPath(path): Promise<void>         // scope fs.read
shell.showInFolder(path): Promise<void>
shell.trash(path): Promise<void>            // scope fs.write
```

Реализация — `opener`, `trash`.

### `clipboard` (system)

```ts
clipboard.readText(): Promise<string>      // право clipboard.read
clipboard.writeText(text): Promise<void>
clipboard.readHtml / writeHtml, readImage(): Promise<Uint8Array /* PNG */>, writeImage(png)
```

Реализация — `arboard` (уже в дереве зависимостей Servo).

### `os` (system)

```ts
os.info(): Promise<{ platform, arch, version, locale, hostname }>
os.theme(): Promise<'light' | 'dark'>
os.on('theme-changed' | 'suspend' | 'resume', ...)
```

### `screen` (system, UI)

```ts
screen.monitors(): Promise<Monitor[]>       // name, bounds, workArea, scaleFactor, primary
screen.cursorPosition(): Promise<{ x, y }>
```

### `notification` (system)

```ts
notification.show({ title, body, icon? }): Promise<void>
notification.on('click', ...)
```

Windows — WinRT toast (нужен AppUserModelID, задаётся при упаковке; в dev — запасной вариант); macOS — `UNUserNotificationCenter` (требует подписанного бандла — в dev ограничения); Linux — D-Bus (`notify-rust`).

## Структура кода

```
alef-modules/src/
  desktop/   mod.rs, app.rs, window.rs (+ window/geometry.rs при росте), dialog.rs, shell.rs
  system/    mod.rs, path.rs, clipboard.rs, os.rs, screen.rs, notification.rs
packages/api/src/
  desktop/   app.ts, window.ts, dialog.ts, shell.ts
  system/    path.ts, clipboard.ts, os.ts, screen.ts, notification.ts
```

UI-операции (`window`, `screen`, часть `dialog`) — через `UiDispatcher` из `alef-core`.

## Порядок работ

1. `path`, `os`, `app` (без single-instance) — простые, проверяют связку модуль → реестр → JS.
2. `window` (геометрия §6.2, события, мультиоконность), `screen`.
3. `dialog` + гранты сессии; `shell`; `clipboard`.
4. `notification`, single-instance, `restore`.
5. Демо-приложение `apps/demo-desktop` (или страница в `tests/e2e`) — по сценарию на модуль.

## Приёмка

| Модуль | e2e-сценарий |
|---|---|
| app | `info`, `args` с разбором по схеме, `env` с правом и без, `quit` с кодом |
| path | все каталоги существуют/корректны для платформы |
| window | создание второго окна; `70%work`/`%screen` дают ожидаемые размеры на текущем мониторе; `center`; min/max; `close-requested` отмена; события resize/move/focus; `restore` после перезапуска |
| dialog | (полуручной) открыть файл → грант → чтение через `fs` в M3; отмена → `[]` |
| shell | `openExternal` разрешённого/запрещённого URL |
| clipboard | запись/чтение текста; без права `clipboard.read` — отказ |
| os, screen | значения непустые и согласованы с `window.state()` |
| notification | (ручная) показ и клик |

Плюс Rust unit-тесты модулей без Servo, `npm run check`, CI на трёх ОС.

## Риски

- Уведомления и deep integration требуют идентичности приложения (AUMID на Windows, подпись на macOS) — полноценно только после установки приложения (`alef install`, M7b: ярлык с AUMID, обёртка `.app`, `.desktop`); в dev — деградация с понятной ошибкой `NOT_AVAILABLE`.
- `close-requested` с ожиданием ответа документа — не блокировать цикл winit (асинхронный ответ + таймаут).

## Статус реализации

Сделано (проверено `test`/`clippy -D warnings`/`fmt --check`/`lint:structure`/`npm run test:api`/`check:types`, e2e на реальном Servo через `alef`):

- **M2.1 `app`, `path`, `os`.** Крейт `alef-modules` (`desktop/{app,args}.rs`, `system/{path,os}.rs`), вход — `alef_modules::register_all(registry, host, &ModuleContext)`; `alef` передаёт его в `BridgeOptions.modules`, поэтому `alef-runtime` модулей не знает (File Manager работает без них, как раньше). Всё, что модулям нужно от процесса, идёт через `alef_core::registry::host::Host` (`quit(code)`, `theme()`), его реализует `RuntimeHandle`: запрос выхода запоминается (`exit_code()`), будит цикл winit, `alef` возвращает его кодом процесса; тема берётся у окна и обновляется по `ThemeChanged` (событие `os.theme-changed`, полезная нагрузка `{ theme }`).
  - Команды: `app.{info,args,env,envAll,cwd,quit,relaunch}`, `path.{appData,appConfig,appCache,temp,home,documents,downloads,desktop,executable,join,normalize,dirname,basename}`, `os.{info,theme}`. JS: `app`, `path`, `os` в `@alef-tron/api` (последний аргумент — `{ signal? }`, у `path.join` — только части).
  - `app.args()` — разбор командной строки по новому необязательному разделу манифеста `arguments` (грамматика и проверка — `FRAMEWORK-PLAN.md` §6.1); разборщик — свой, без зависимостей (`desktop/args.rs`: ~150 строк против новой зависимости; clap/lexopt ради двух форм записи не нужны). `parsed` типизирован схемой (`boolean | number | string`), ключи — длинные имена. У универсального запуска аргументы приложения идут после `--` (`alef --app dir -- --port 80`); `--help`/`-h` и `--version`/`-V` печатают сгенерированный текст и завершают процесс с кодом 0 до открытия окна, ошибка использования — код 2.
  - `app.env(name)` требует права `app.env` со списком имён: `PERMISSION_DENIED` для имени не из списка (в ответе только имя права); перечисленная, но не заданная переменная — `null` (в JS `undefined`). `app.envAll` (JS: `app.env()` без имени) отдаёт перечисленные переменные, которые заданы; пустой список в манифесте — тот же отказ, что и для неперечисленного имени. Права проверяет реестр до обработчика (`Permission::AppEnv`, цель — имя).
  - `app.quit(code?)`: код `0..=255`, иначе `INVALID_ARGUMENT`; ответ может не дойти — окно закрывается. `app.relaunch()` запускает ещё один экземпляр с теми же аргументами процесса (потоки и окружение наследуются) и завершает текущий с кодом 0; единственный экземпляр (`requestSingleInstance`) — M2.4.
  - `path.*`: каталоги приложения — `<системный>/<app.id>` и **не создаются**; `join` склеивает непустые части и нормализует (абсолютная часть путь не сбрасывает), `normalize`/`dirname`/`basename` — лексические, без обращения к диску, разделитель платформы, без хвостового разделителя; `..` не поднимается выше корня.
  - `os.info()`: `platform`/`arch` — имена `target_os`/`target_arch` Rust (`windows`, `x86_64`), `version` — версия ОС (Windows — из `cmd /C ver`, macOS — `sw_vers`, Linux — `VERSION_ID` из `/etc/os-release`, иначе ядро; читается один раз, при неудаче `unknown`), `locale` — BCP 47 (`und`, если система не сообщает), `hostname`. Зависимости: `gethostname`, `sys-locale` (уже были в дереве Servo, новых пакетов в `Cargo.lock` нет, кроме рёбер).
  - Типы ответов (`AppInfo`, `ParsedArgs`, `ArgValue`, `OsInfo`, `Theme`) генерируются ts-rs: новая группа `modules.ts` (в `gen-types` добавлены крейт `alef-modules` и группа; DTO не должен ссылаться на тип другой группы).
  - e2e (`npm run test:e2e -- --only app,quit,relaunch,system,arguments`): `app` — info, разбор `-p 8080 --verbose --label=… y.txt`, env с правом/без/неустановленная/список, cwd, отказ `quit` на плохом коде; `quit` — код процесса равен коду `app.quit(7)`; `relaunch` — второй экземпляр (другой pid, те же аргументы, общий конвейер лога) присылает вердикт и сам завершается; `system` — каталоги абсолютны, `temp`/`home`/`executable` существуют на диске, `executable` — запущенный бинарник, `os.info` согласован с Node (`platform`, `arch`, `hostname`), арифметика путей; `arguments` — `--help`/`-h`/`--version`, неизвестная опция, число не из цифр, опция без значения, приложение без схемы, всё — до открытия окна.

Не сделано в M2.1 (по плану дальше; `requestSingleInstance`, `second-instance` и `before-quit` сделаны в M2.4): события `os.on('suspend'|'resume')` — нужны уведомления ОС о питании, пока не приходят (подписка принимается только на `theme-changed`).

Не проверено на этой машине (проверит CI): `os.info` на macOS/Linux (чтение версии), нормализация путей Unix (юнит-тесты `cfg(unix)` есть), `app.relaunch` вне Windows.

- **M2.2 `window`, `screen`.** Контракт — `alef-core::registry::window`: `UiCall` (что модули просят у потока UI), `WindowCall { label?, op }` и `WindowOp` (25 операций), DTO `WindowInfo`, `MonitorInfo`, `Rect`, `Point`, `ResizeEdge` (группа `core.ts`), чистая геометрия §6.2 (`geometry`: px, `%screen`, `%work`, пределы, `monitor`, `position`). Хост выполняет запрос через `Host::ui(caller, call)` (его реализует `RuntimeHandle`: запрос уходит в очередь потока UI и будит цикл winit). Модуль — `alef-modules`: `desktop/window.rs` (разбор аргументов и право, команды `window.<операция>`, `window.create`, `window.all`), `system/screen.rs` (`screen.monitors`, `screen.cursorPosition`). Исполнение — `alef-runtime/src/window/host/` (`manage.rs` — окна и закрытие, `ops.rs` — операции над одним окном, `displays.rs` — дисплеи, `events.rs` — события) и `state/` (слот окна).
  - **Окна.** Все окна манифеста открываются при старте; первое — главное, его документ — `entry` моста. Остальные берут путь `url` от того же origin (адрес другого хоста — отказ) с тем же capability. Метка уникальна (`ALREADY_EXISTS`), неизвестная — `NOT_FOUND`. Окно создаётся скрытым и показывается с первым непустым кадром (как в «Старт окна» ниже). Процесс заканчивается вместе с последним окном (`app.quit` — сразу). Один `Servo` на все окна; закрытие окна — drop webview → контекста → окна, сессии закрытого окна закрываются.
  - **Команды.** `window.{state,setTitle,setSize,setPosition,center,minimize,maximize,restore,toggleMaximize,setFullscreen,setAlwaysOnTop,setResizable,setDecorations,setMinSize,setMaxSize,show,hide,focus,close,destroy,startDrag,startResize,setZoom}` с необязательным `label` (без него — окно вызывающего документа); имя команды решает операцию, поле `op` в теле игнорируется. `window.create(WindowDef)` требует `permissions.window.create: true` (иначе `PERMISSION_DENIED` до разбора аргументов), `window.all`. Служебные `window.closeIntercept`/`window.closeAnswer` — только для своего окна. Новые необязательные поля окна манифеста: `title`, `decorations`, `resizable`; `restore: true` по-прежнему отклоняется (M2.4).
  - **Геометрия.** Размеры — клиентская область, логические пиксели; `state()` отдаёт `WindowInfo` в тех же единицах (положение — внешней рамки). Позиция `center` — по центру рабочей области выбранного дисплея с учётом рамки (она известна только после создания окна, поэтому окно центрируется сразу после создания). Рабочая область — `GetMonitorInfoW` (Windows); на macOS и Linux рабочая область равна дисплею, а положение курсора неизвестно: `screen.cursorPosition` — `NOT_AVAILABLE`, `monitor: cursor` падает на основной дисплей. `setMinSize`/`setMaxSize` запоминаются окном, `setSize` удерживается в них рантаймом, а окно за пределами нового лимита подтягивается: Windows применяет лимиты только к ресайзу мышью (программный `SetWindowPos` их не знает — установлено наблюдением).
  - **События.** `window.moved`, `window.resized`, `window.focus`, `window.blur` — всем документам, с `label` в нагрузке (`AppWindow.on` отбирает свои); их рождает разность снимков окна, поэтому они приходят и тогда, когда ОС сама сдвинула или изменила окно. `runtime.window.state` (снимок для `nativeWindow.watch` File Manager) сохранён и несёт `WindowInfo`. `window.close-requested { label, id }` приходит только документу своего окна, и только после `window.closeIntercept`; ответ — `window.closeAnswer { id, prevent }`; без ответа 3 с — окно закрывается. `close()` — как закрытие пользователем (документ может отказать), `destroy()` — без вопроса. Если сессия, взявшая закрытие на себя, уже сменилась (перезагрузка документа), окно закрывается сразу.
  - **File Manager** модули не подключает (M3): `window.apply` остался адаптером над теми же операциями, `nativeWindow` не изменился.
  - **Особенности ОС** (установлены наблюдением на Windows): при первом показе ОС сдвигает окно, заходящее под панель задач, в рабочую область (положение 30 при рабочей области от 158 стало 151 = 158 − 7 невидимой рамки), поэтому позиции в e2e — внутри рабочей области; событие `moved` приходит и для такого сдвига.
  - **Spike multiwindow удалён** вместе с хуками в `window/` и `experiments/multiwindow-spike`; нестабильность его оракула закрыта удалением (проверка мультиоконности — сценарий `window`). Спайки `origin` и `integration` остаются (`origin` обслуживает продакшн-код).
  - e2e (`npm run test:e2e -- --only window,system,manifest`): `window` — два окна манифеста, размер `70%work`/`60%work` и `50%screen`/`40%screen`, центр, позиции в px и `%work`, пределы min/max, `maximize`/`restore`, события `moved`/`resized` с меткой (чужое окно их не получает), заголовок, zoom, `hide`/`show`, правила меток и url, отказ и разрешение закрытия документом окна, закрытие по истечении 3 с без ответа, `destroy` без ожидания, завершение процесса вместе с последним окном; `system` — `screen` согласован с `state()` окна, `window.create` без права отказан; `manifest` — отказы: `restore`, повтор метки, url чужого хоста, минимум больше максимума, лишнее поле в `permissions.window`.

  - **Тихие тестовые запуски.** На Windows и macOS раннер e2e задаёт `ALEF_E2E_QUIET=1`: рантайм создаёт и грузит окна, но не показывает их и не берёт фокус (`window/state`: `Pretended` — собственный учёт видимости, фокуса, maximize/minimize, fullscreen; геометрия, заголовок, zoom, события и закрытие настоящие; кадры скрытых окон рисуются из `tick`). `assertQuiet` в раннере отказывается запускать бинарник, не знающий режима, `ALEF_E2E_VISIBLE=1` показывает окна. Сценарий `startup` смотрит на настоящие окна: на Windows он запускается только с `ALEF_E2E_VISIBLE=1`. Первая попытка — отдельный рабочий стол Windows — не удалась: WGL-контекст там не создаётся (`MakeCurrentFailed`), см. M0.5.
  - **Заголовок окна** хранит рантайм (`State.title`): winit не читает его на X11 и Wayland (пустая строка), поэтому окна, которые договаривались через заголовок, на Linux друг друга не видели.

  Не сделано в M2.2: `file-drop` (нужны гранты сессии и OS-перетаскивание — после `dialog`, M2.3), `restore` и положение между запусками (M2.4), значок окна из JS. Не проверено на этой машине (проверит CI): macOS и Linux (на Linux e2e идёт под `xvfb` + `openbox`; положение окон и лимиты там зависят от оконного менеджера), рабочий стол с дисплеями разного масштаба (логические прямоугольники не стыкуются точно, положение в px приблизительно), события `focus`/`blur` в живом e2e (покрыты юнит-тестом разности снимков).

- **M2.3 `dialog`, `shell`, `clipboard`.** Контракт диалогов — `alef-core::registry::dialog` (`OpenOptions`, `SaveOptions`, `MessageOptions`, `ConfirmOptions`, `FileFilter`, `DialogCall`; пределы и проверка опций) и `UiCall::Dialog`; модули — `alef-modules`: `desktop/dialog.rs`, `desktop/shell.rs`, `system/clipboard.rs`; показ диалогов — `alef-runtime/src/window/host/dialogs.rs` (`rfd`). Буфер и оболочка стоят за трейтами `ClipboardBackend` и `ShellBackend` (`ModuleContext.backends`): системные (`SystemClipboard`, `SystemShell`) и «притворяющиеся» (`MemoryClipboard`, `PretendShell`) — те же, что станут «подменой» права в M2b.
  - **dialog.** `dialog.{open,save,message,confirm}`; документ получает путь/пути, `[]`/`null` при отмене, `true`/`false` на `confirm`. Опции проверяются до показа (`INVALID_ARGUMENT`, диалог не открывается и ничего не берёт из сценария): заголовок ≤ 256 знаков, сообщение ≤ 8192 (переводы строк можно, прочие управляющие знаки нет), подписи ≤ 64, фильтров ≤ 32, расширение — буквы/цифры/`_`/`-`/`+` без точки или `*`, `defaultPath` — абсолютный, неизвестные поля отклоняются, у выбора папки не бывает фильтров, подписи `okLabel` и `cancelLabel` (с учётом `OK`/`Cancel` по умолчанию, когда задана одна из них) не совпадают. Диалог открывается поверх окна вызывающего документа; поток UI не блокируется (диалог ждёт `rfd` на Tokio, ответ уходит оттуда), диалоги приложения идут по одному (очередь), запрос, от которого отказались (`signal`) в очереди, не показывается, уже открытый остаётся до закрытия пользователем. Фильтры с `*` не передаются диалогу (без фильтра он и так показывает всё). Выбор даёт **грант сессии** документа: `open` — чтение (папка — со всем внутри), `save` — запись именно этого файла (его может ещё не быть); отмена, ошибка и странный ответ хоста ничего не дают; гранты кончаются со своей сессией (перезагрузка документа их не наследует). Путь, который не удалось сделать каноническим (пропал между выбором и грантом), — `INVALID_ARGUMENT`.
  - **shell.** `openExternal(url)` — по области `permissions.shell.openExternal`, адрес уходит в систему той строкой, что проверена; `openPath`/`showInFolder` — чтение пути, `trash` — запись; система получает **канонический** путь, который вернула проверка (не строку документа); нет такого пути — `NOT_FOUND` на любом бэкенде (путь вне области — отказ раньше, существование не угадать). **`openPath` не запускает программы**: расширения исполняемых и сценарных файлов (Windows: `exe com bat cmd ps1 vbs js msi lnk url hta jar …`, macOS: `app command sh pkg …`, `desktop`) и файлы с битом исполнения на Unix — `PERMISSION_DENIED`; иначе «читать и открыть» было бы обходом `cli.exec` (показать в папке и в корзину такой файл можно). Системная оболочка: `opener` 0.9 без `reveal` (D-Bus не нужен), `trash` 5.2; «показать в папке» сделано напрямую: Windows — `explorer /select,"путь"`, macOS — `open -R`, Linux — открывается папка файла (выделить файл нечем).
  - **clipboard.** `clipboard.{readText,writeText,readHtml,writeHtml,readImage,writeImage}`; чтение требует `clipboard.read`, запись права не требует. Содержимое идёт **байтами** (текст и HTML — UTF-8, картинка — PNG): тело одного вызова JSON ограничено 256 КиБ, байтовое — 128 МиБ; нет текста/HTML — пустая строка, нет картинки — `null`. Буфер держит что-то одно: запись текста вытесняет картинку и наоборот (как настоящий). `writeImage` принимает PNG до 8192×8192 пикселей и 256 МиБ после разбора, лимиты проверяются до распаковки; не-PNG, обрезанный PNG, не-UTF-8 текст и вызов без тела — `INVALID_ARGUMENT`, буфер остаётся как был. Системный буфер — `arboard` 3.6 (уже был в дереве Servo). Дескриптор один на всё время жизни бэкенда: на X11 содержимое отдаёт тот, кто его записал, и без менеджера буфера оно пропадало бы вместе с дескриптором, открытым на один вызов (это показал настоящий тест на Linux в CI, а не догадка).
  - **Выбор бэкендов.** `Backends::from_environment()`: при `ALEF_E2E=1` — притворяющиеся (буфер в памяти, оболочка ничего не открывает и дописывает запросы строками JSON в файл `ALEF_E2E_SHELL_LOG`), иначе системные. В `alef-runtime` при `ALEF_E2E=1` диалоги отвечают **только по сценарию** `ALEF_E2E_DIALOGS` (`[{"open": [...]}, {"save": null}, {"message": null}, {"confirm": true}]`, по записи на диалог, порядок как у вызовов); сценарий не задан или исчерпан — ошибка, не ожидание; запись не того вида — ошибка. Поэтому ни один e2e-запуск не может показать диалог, открыть браузер, тронуть буфер или корзину пользователя.
  - **JS.** `dialog`, `shell` (`@alef-tron/api`, `desktop/`), `clipboard` (`system/`); последний аргумент `{ signal? }`; у `dialog.*` опции — первый аргумент. Типы опций генерируются из Rust (`core.ts`).
  - **Зависимости.** Новые пакеты в `Cargo.lock`: `opener` 0.9.0, `trash` 5.2.9, `normpath`, `urlencoding`; `arboard` 3.6.1 и `image` (png) теперь и прямые зависимости `alef-modules`; `rfd` из флага спайка стал обычной зависимостью `alef-runtime`.
  - **Тесты.** Unit — контракт диалогов (7), исполнитель диалогов в runtime (9: сценарий, очередь, `confirmed`, фильтры, старт-каталог, сборка диалогов каждого вида без окна); интеграционные `alef-modules`: `dialog` (10: опции, гранты чтения/записи/папки/нескольких файлов, отмена, конец сессии, ошибки и странные ответы хоста), `shell` (область URL и пути, канонический путь, `NOT_FOUND`, запрет программ, бит исполнения, неверные формы), `clipboard` (права, текст любого размера, HTML, картинка пиксель в пиксель, лимиты и не-PNG); JS (`dialog-shell-clipboard.test.mjs`); e2e — сценарий `desktop` (18 проверок на реальном Servo: диалоги по сценарию, оболочка-журнал сверяется раннером с точностью до канонического пути, буфер в памяти, текст 1 МиБ, PNG туда и обратно) и проверка отказов в `system`. Тесты с настоящим буфером и корзиной помечены `#[ignore]` и идут только с `ALEF_TEST_DESKTOP=1` (в CI — шаг «Desktop backends» на чистых раннерах трёх ОС; на этой машине они не запускались, чтобы не трогать буфер и корзину пользователя; показ и открытие системой не пробуются нигде).
  - **Мутационная проверка** (33 правки: гранты на чтение/запись/ничего, проверки опций, запрет запуска программ, права и канонический путь, лимиты и UTF-8 буфера, порядок и вид ответов сценария, очередь и «отказавшийся» запрос) — все пойманы; одна правка поначалу ускользнула (право на корзину проверяют реестр и обработчик, ослаблен был один слой) — с обоими слоями поймана.
  - **Особенности и пробелы.** На Windows без манифеста с common controls v6 `rfd` показывает стандартные кнопки: подписи `okLabel`/`cancelLabel` не действуют, ответ при этом верный (`Ok`/`Cancel`); манифест и флаг `common-controls-v6` — упаковка, M7. Настоящие диалоги на этой машине не показывались (по договорённости тесты не открывают окон): их показ и привязка к родителю — вручную; `xdg-portal` на Linux и `NSOpenPanel` на macOS не проверены. `file-drop` остаётся на M2.4.

- **M2.4 один экземпляр, `before-quit`, положение окон, `file-drop`, `notification`.** Событие из модуля в документы — новый метод `Host::emit(window, name, payload)` (его реализует `RuntimeHandle` через шину событий; в тестах — запись в `FakeHost.events`).
  - **Один экземпляр** (`alef-modules/src/desktop/instance.rs`). `app.requestSingleInstance()` даёт `true` первому экземпляру приложения пользователя и `false` остальным; повторный вопрос даёт тот же ответ. Первый экземпляр владеет локальной точкой: Windows — именованный канал `\\.\pipe\alef-instance-<ключ>` (`first_pipe_instance`, отказ удалённым клиентам), Linux/macOS — Unix-сокет в приватной подпапке `$APPCACHE/instance` (папка 0700, сокет 0600); ключ — хэш домашнего каталога и `app.id`, поэтому приложения и пользователи не встречаются. Следующий экземпляр находит точку занятой, шлёт одну строку JSON `{args, cwd}` (аргументы — уже разобранные по схеме манифеста, как у `app.args()`), получает `ok` и отвечает `false`; первый поднимает `app.second-instance { args, cwd }` во всех окнах. Устойчивость: сокет, оставшийся от умершего процесса, снимается (подключение не удалось → перезахват); мусор, пустое подключение, сообщение больше 256 КиБ и более 4096 аргументов события не рождают (5 с на соединение); точка, в которой никто не отвечает, — `NOT_AVAILABLE`, а не молчаливый «второй». Известный предел: два экземпляра, одновременно снимающие один и тот же протухший сокет, теоретически могут оба стать первыми.
  - **`before-quit`** (`app.rs`). Документ, подписавшийся через `app.on('before-quit', ...)`, регистрируется командой `app.quitIntercept`; `app.quit(code)` и `app.relaunch()` сначала спрашивают его (`app.before-quit { id }`, ответ `app.quitAnswer { id, prevent }`), на ответ 3 с, молчание разрешает выход. Опрашиваются только подписавшиеся окна, один отказ завершает раунд сразу (не ждёт остальных), подписка умершей сессии (окно перезагрузилось) отбрасывается, ответ чужого окна или на чужой номер ничего не меняет. Выход по закрытию последнего окна идёт через `window.close-requested`, не через это событие.
  - **Положение окон** (`alef-runtime/src/window/state/restore.rs`, геометрия — `alef-core::registry::window::geometry::{reachable, restore}`). Окно с `restore: true` в манифесте открывается там, где было: размер и положение внешней рамки (логические px, как `window.state()`), развёрнутость. Хранилище — `window-state.json` в `$APPDATA` приложения (его путь задаёт `alef`: `WindowOptions::remembering_windows_in`), версия 1; пишется не позже секунды после первого изменения, при закрытии окна и при выходе. Свёрнутое и полноэкранное окно ничего не говорит о месте; развёрнутое запоминается как развёрнутое с прежним размером (по `restore()` оно вернётся к нему). Положение берётся, только если окно ещё можно взять мышью: верхний край в рабочей области дисплея (допуск 16 px) и в ней не меньше 100×40 px окна; иначе остаётся положение из манифеста (центр). Размер не больше самой большой рабочей области. Файл отсутствует, повреждён, другой версии или с абсурдными числами — как будто его нет. Отказ `restore: true` в `alef` снят. Работает и у окон, созданных `window.create`, по метке.
  - **`file-drop`** (`window/host/drops.rs`). `WindowEvent::DroppedFile` копится в окне и за проход цикла отдаётся одним событием `window.file-drop { label, paths }` документу этого окна; пути, которые существуют, абсолютны и в Unicode, получают грант чтения в сессии окна (папка — со всем внутри), остальные в событие не попадают; окно без документа ничего не получает; не более 500 путей за раз. JS: `AppWindow.on('file-drop', ...)`. e2e дроп не умеет, поэтому есть команда `e2e.dropFiles` (только при `ALEF_E2E=1`), идущая тем же путём внутри рантайма; само превращение события winit в запись очереди (три строки) локально не проверено.
  - **`notification.show({ title, body?, icon? })`** (`alef-modules/src/system/notification.rs`). Заголовок до 128 знаков без управляющих, текст до 1024 (переводы строк и табуляция можно), неизвестные поля и не-объект отклоняются; `icon` — путь файла, который документ может читать (`fs.read`, как у `shell`), нужен существующий файл, система получает канонический путь. Права на сам показ нет. За показ отвечает `NotificationBackend`: системный (`SystemNotifications`) и притворяющийся (`PretendNotifications`, при `ALEF_E2E=1`, журнал JSON-строк в `ALEF_E2E_NOTIFICATION_LOG`). Линукс — `notify-send` (аргументы по одному, `--` перед текстом; нет программы — `NOT_AVAILABLE`); Windows и macOS показывают уведомления только приложению с идентичностью (AppUserModelID, подписанный бандл), у запущенного из папки её нет — там `NOT_AVAILABLE` с объяснением, пока `alef install` (M7b) её не даст. Отступление от плана: `notification.on('click')` не сделан — событие нечем доставить без идентичности (Windows/macOS) и без слушателя D-Bus (Linux); вернётся в M7b. `notify-rust` отвергнут: десятки пакетов D-Bus и требование Rust 1.89 при заявленных 1.88.
  - **e2e** (все на реальном Servo в тихом режиме): `instance` — два процесса (первый ждёт второго, второй узнаёт, что не первый и выходит с 0; первый отклоняет один выход и разрешает второй, код 9), `restore` — пять запусков (положение и размер, развёрнутость, место на отсутствующем дисплее, битый файл; между запусками раннер читает и подменяет файл), `window` получил проверку `file-drop` (грант и событие), `desktop` — уведомления (журнал сверяется раннером). Тесты `alef-modules/tests` сгруппированы в два крейта (`desktop`: `dialog`, `lifecycle`, `shell`, `window`; `system`: `clipboard`, `notification`) из-за предела в 7 записей на каталог; реальные тесты буфера и корзины запускаются как `--test system --test desktop -- --ignored`.
  - **Не проверено на этой машине:** одно из событий winit (`DroppedFile`) и drop мышью; Unix-ветка единственного экземпляра и права папки (покрыты тестами для Unix, идут в CI на Linux/macOS); `notify-send` на живом рабочем столе (покрыт тестом со вставным скриптом на Linux в CI); вложенные дисплеи с разным масштабом для `restore`.

### Старт окна (исправление дефектов запуска)

Два видимых дефекта при запуске (Windows): перед окном приложения на ~30 мс мелькало другое окно, а само окно сначала было белым с чёрной полосой снизу. Причины установлены наблюдением (перехват событий окон процесса через WinEvent-hook и снимки окна `PrintWindow`), а не догадкой:

- **Мелькающее окно** — `SurfmanFalseWindow` 640×480 в (0,0): `surfman` 0.13 при первом WGL-контексте создаёт его с `WS_VISIBLE` ради загрузки расширений WGL и тут же уничтожает. Исправлено локальной копией `backend/patches/surfman` (в `src/wgl/context.rs` убран `WS_VISIBLE`; версия и лицензии не менялись; `[patch.crates-io]` в `backend/Cargo.toml`).
- **Белое окно с полосой** — winit показывал окно сразу (через ~60 мс), а первый кадр рисуется через 2–3 с (debug); окно создаётся с размером по умолчанию (1203×728), затем вырастает до заказанного (1200×800), дорисованная часть — чёрная. Теперь окно создаётся скрытым (`with_visible(false)`) и показывается из `State::try_reveal`: когда страница загрузилась (`LoadStatus::Complete`), каждый объявленный ею кадр рисуется вне экрана и считывается (`read_to_image`); первый кадр, не состоящий из одного цвета, презентуется, окно показывается и получает фокус. Загрузка заканчивается раньше, чем приложение что-то нарисует (React монтируется уже после `Complete`), поэтому одного `Complete` мало — первый кадр после него был белым. Страница, которая остаётся однотонной, показывается через 600 мс после загрузки, не загрузившаяся — через 10 с (приложение не должно остаться невидимым).
- Проверка: сценарий e2e `startup` (только Windows; скрипт `tests/e2e/apps/startup/startup-probe.ps1 -Assert`): ни одно окно, кроме главного, не показывается; первый снимок клиентской области главного окна содержит ≥ 16 цветов (не белый и не «белый с полосой»). На бинарнике до исправления сценарий падает с обоими нарушениями (`SurfmanFalseWindow` на 2718 мс, снимок с 1 цветом), после — проходит. Скрипт работает и вручную (`-OutDir` — снимки, список событий окон в выводе). Сценарий `core` проверяет, что окно в итоге показано (`window-is-shown-once-it-has-content`).
- Известное: на Linux/macOS поведение скрытого старта проверено только сборкой и e2e на Linux (xvfb); настоящий показ окна на этих системах — на CI/вручную.
