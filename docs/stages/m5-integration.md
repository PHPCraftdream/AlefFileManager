# M5 — Системная интеграция

Обзор: `../FRAMEWORK-PLAN.md`. Зависит от M2 (окна, `app`) и результатов M0.3.

## Цель

`menu`, `tray`, `shortcut`, автозапуск, deep links; доводка single-instance (передача аргументов deep link во второй экземпляр).

## Модули

### `menu` (desktop, UI)

```ts
menu.setApplicationMenu(items: MenuItem[]): Promise<void>     // macOS — строка меню; Windows/Linux — меню окна
menu.setWindowMenu(window, items): Promise<void>
menu.popup(items, { x?, y? }): Promise<string | null>         // контекстное меню, id выбранного пункта
menu.on('click', ({ id }) => ...)
MenuItem: { id, label, accelerator?, enabled?, checked?, kind?: 'normal'|'check'|'separator'|'submenu', items? }
// предопределённые роли: copy, paste, cut, undo, redo, selectAll, quit, about, minimize, …
```

`muda`; на macOS обязательны роли стандартного меню приложения.

### `tray` (desktop, UI)

```ts
tray.create({ icon: Uint8Array /* PNG */, tooltip?, menu?: MenuItem[] }): Promise<Tray>
  Tray: setIcon, setTooltip, setMenu, destroy, on('click'|'double-click'|'right-click', ...)
```

`tray-icon`; Linux — AppIndicator (зависимость `libayatana-appindicator3`), без неё → `NOT_AVAILABLE`.

### `shortcut` (desktop, UI)

```ts
shortcut.register('CommandOrControl+Shift+K'): Promise<Shortcut>   // on('pressed'), unregister()
```

`global-hotkey`; право `permissions.shortcut.global: true`; Wayland — глобальные хоткеи ограничены (portal), → `NOT_AVAILABLE` где невозможно.

### Автозапуск (`app`)

```ts
app.autostart.isEnabled(): Promise<boolean>; enable(); disable()
```

Windows — `HKCU\...\Run`; macOS — Login Item (`SMAppService`); Linux — `~/.config/autostart/<id>.desktop`. Реализация — `auto-launch` или собственная по платформам.

### Deep links (`app`)

```ts
app.on('open-url', ({ url }) => ...)       // myapp://path?x=1
```

- Схемы в манифесте: `deepLinks: [ myapp ]`.
- Регистрация схемы: при упаковке (M7: Info.plist, `.desktop`, реестр в инсталляторе); в dev — `app.registerDeepLinks()` для текущего пользователя (Windows/Linux).
- Если приложение запущено — ссылка доставляется через single-instance в первый экземпляр.

## Идентичность приложения

Бинарник runtime общий, поэтому автозапуск, deep links, ассоциации файлов и уведомления приложения опираются на идентичность, которую создаёт установка (`alef install`, `m7-distribution.md` M7b): ярлык с AppUserModelID (Windows), тонкая обёртка `.app` (macOS), `.desktop` (Linux). Все регистрации указывают на `alef run <пакет> ...`. Запуск на месте (без установки) даёт деградацию с понятной ошибкой `NOT_AVAILABLE` там, где нужна идентичность.

## Структура кода

```
alef-modules/src/desktop/   menu.rs, tray.rs, shortcut.rs (+ app/ autostart.rs, deeplink.rs, single_instance.rs при росте app)
packages/api/src/desktop/   menu.ts, tray.ts, shortcut.ts
```

По правилу 7 элементов `desktop/` в Rust к этому этапу превышает лимит — разделение: `desktop/app/` (app, autostart, deeplink, single_instance), `desktop/shell/` (dialog, shell), `desktop/ui/` (window, menu, tray, shortcut).

## Приёмка

| Проверка | Как |
|---|---|
| Меню приложения/окна, контекстное меню, accelerator; клики доходят | e2e + ручная (macOS строка меню) |
| Трей: иконка, меню, клики; удаление при выходе | ручная на трёх ОС |
| Глобальный хоткей при окне не в фокусе; без права — отказ | e2e (Windows/macOS/X11) |
| Автозапуск включается/выключается и переживает перезагрузку ОС | ручная |
| Deep link запускает приложение / доставляется в запущенное | ручная после M7-упаковки; dev-регистрация — Windows/Linux |

## Реализация и статус

**Как сделано (M5.2, `shortcut`)** — `alef-core/src/registry/window/shortcut.rs`, `alef-modules/src/desktop/ui/shortcut.rs`, `alef-runtime/src/ui/integration/{shortcut,forward}.rs`, `packages/api/src/desktop/shortcut.ts`:

- Команды `shortcut.register` и `shortcut.unregister`, право `shortcut.global` (с подменой). Регистрация — ресурс документа: ответ `{ id, owner, token }`, где `id` — `s<сессия>:r<ресурс>`, а `token` — нативный идентификатор события (`null` при подмене, подмена инертна и ОС не трогает). Снять регистрацию может только документ-владелец, и только по handle ровно в этом виде; перезагрузка документа и закрытие окна снимают его регистрации.
- Нативный `GlobalHotKeyManager` и таблица регистраций принадлежат UI-потоку; единственный forwarder читает события крейта и будит winit. Событие уходит только живому документу-владельцу, только на нажатие, не на отпускание. Пределы: 128 регистраций, акселератор 1..256 байт без управляющих символов, токены не повторяются и не выходят из диапазона Win32 (1..0xBFFF); занятая комбинация — `ALREADY_EXISTS` (на macOS так отображается и отказ Carbon).
- `Shortcut.on('pressed')` фильтрует `owner` и `token`; событие неверной формы пропускается, а не роняет обработчик.
- e2e `shortcut`, `shortcut-denied`, `shortcut-substitute`: настоящая регистрация **Ctrl+Alt+Shift+F20** (на Linux и macOS — **Ctrl+Alt+Shift+F12**: F20 нет в их раскладках, CI Linux это показал), повтор → `ALREADY_EXISTS`, неверный акселератор, снятие и повторная регистрация, чужой handle из дочернего документа → `NOT_FOUND`, закрытие дочернего окна освобождает комбинацию; отказ и подмена — отдельными запусками с собственным каталогом согласий. Сценарий не создаёт реестровых записей и файлов вне временного каталога.
- Проверки (Windows): 810 тестов Rust по всему рабочему пространству, 207 JS, весь e2e (49 сценариев) и загрузка File Manager прошли; `clippy --workspace --all-targets -D warnings`, tsc, oxlint, структура, fmt и `gen:types --check` чистые. Мутации: 28 в Rust (`alef-modules` и таблица `alef-runtime`) и 14 в JS-обёртке, пойманы все. Первый прогон отпустил настоящих: различие модификаторов, предел токенов Win32, повторное освобождение при sweep, закрытие окна, отображение «комбинация занята», жизнь владельца, длину акселератора, тело `unregister`, форму handle и событие с пустым payload — под них добавлены тесты, `live_owner` и `Table::release_window` вынесены, чтобы их можно было проверить без цикла событий; проверка handle сведена к одному сравнению с каноническим видом (две другие были её следствием).
- Не сделано: меню и tray (следующие части M5). Настоящее нажатие ОС остаётся ручным (сценарий пишет `MANUAL shortcut-os-pressed-not-tested`; синтетических событий нет). Linux — только X11 с `DISPLAY` (Wayland → `NOT_AVAILABLE`), Linux и macOS локально не запускались: их ветки проверяет только CI. Подмена не проверена внешним конкурентом за комбинацию. Активный spike M0.3 владеет источником событий hotkey и исключает одновременную работу.

## Риски

- Wayland: трей и глобальные хоткеи зависят от окружения рабочего стола.
- macOS: многое требует подписанного бандла (M7).
