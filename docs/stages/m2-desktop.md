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
  close, startDrag, startResize(edge), setZoom, state(), on('moved'|'resized'|'focus'|'blur'|'close-requested'|'file-drop', ...)
```

- Геометрия — `../FRAMEWORK-PLAN.md` §6.2: px, `%screen`, `%work`, min/max, `monitor: primary|cursor`, `position: center|x,y`, `restore`.
- `close-requested` отменяемое (`event.preventDefault()` в JS, runtime ждёт ответа документа с таймаутом).
- Drop файлов: `file-drop` с путями; пути получают грант на чтение в текущей сессии.
- Мультиоконность — по результату M0.2; `window.create` требует права `window.create`.
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

- Уведомления и deep integration требуют идентичности приложения (AUMID на Windows, подпись на macOS) — полноценно только после упаковки (M7); в dev — деградация с понятной ошибкой `NOT_AVAILABLE`.
- `close-requested` с ожиданием ответа документа — не блокировать цикл winit (асинхронный ответ + таймаут).
