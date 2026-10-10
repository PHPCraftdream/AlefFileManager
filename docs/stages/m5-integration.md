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

**Как сделано (M5.2, `menu`)** — `alef-core/src/registry/window/menu.rs`, `alef-modules/src/desktop/ui/menu.rs`, `alef-runtime/src/ui/integration/menu/{mod,model,native,unavailable}.rs`, `packages/api/src/desktop/menu.ts`:

- Валидация до передачи UI: до 256 узлов, глубина до 8, уникальные id до 128 байт, метки до 256 байт, акселераторы до 128 байт, модуль координат popup до 1e6; роль несовместима с собственным id. `x` и `y` popup задаются только вместе. Акселератор разбирает парсер `muda` (Windows/macOS) или `global-hotkey` (Linux, где `muda` нет), поэтому запись, принятая на одной ОС, на другой не отвергается по синтаксису.
- Меню — ресурс одного документа-владельца; нативные объекты и таблица живут на UI-потоке. Единственный forwarder получает события `muda`, подавляет устаревшие и отдаёт только живому владельцу; закрытие окна и перезагрузка документа освобождают его меню.
- Windows: меню приложения/окна (подкласс окна `muda`), замена и очистка; popup (`menu.popup`) показывает контекстное меню модально на UI-потоке, возвращает id выбранного пункта или `null`, `x`/`y` — логические пиксели клиентской области окна, без них — позиция курсора; выбор ждётся не дольше 500 мс после закрытия меню. Роли `copy`, `cut`, `paste`, `selectAll`, `undo`, `redo`, `minimize` — предопределённые пункты `muda` (они не отключаются и не получают id события); `quit` и `about` → `NOT_AVAILABLE`: `quit` завершил бы процесс мимо `before-quit`, а `about` не знает, как называется приложение, — пункт страницы с `app.quit()` делает то же.
- macOS: меню приложения (корни — только подменю), меню окна и popup → `NOT_AVAILABLE`; локально не проверено, ветка собирается только в CI.
- Linux: `muda` требует GTK и `libxdo`, которых нет ни у winit-цикла, ни в CI, поэтому зависимость `muda` объявлена только для Windows/macOS, а на Linux любая нативная операция → `NOT_AVAILABLE` (`menu/unavailable.rs`), валидация работает.
- e2e `menu` честно проверяет: отказы на неверные деревья, дубли id, акселераторы, роли с id, координаты и состав popup, принятие вложенного дерева с акселератором, замену и очистку меню, роли редактирования, `NOT_AVAILABLE` для `quit`/`about`, пустой popup → `null`, отсутствующее окно → `NOT_FOUND`, очистку при выходе. **Клик по пункту, акселератор, действие роли и непустой popup автоматически не проверяются** (нет синтетического ввода; сценарий пишет `MANUAL menu-click-accelerator-role-and-popup-not-tested`) — только вручную. Для пункта с акселератором в манифесте нужно право `shortcut` (`global: false`).
- Мутации: 33 в Rust (`alef-core`, `alef-modules`, таблица и модель `alef-runtime`) и 14 в JS-обёртке пойманы; выжившие первого прогона закрыты тестами или удалением лишнего кода (флаг `closed`, проверка роли в `actionable`, `Array.isArray` в обёртке).
- Зависимость `muda` закреплена на **0.19.3** (уже была в lockfile), версий не поднималось.
- Проверки (Windows): полный gate — tsc, oxlint, 224 теста JS, fmt, структура, `gen:types --check`, clippy по рабочему пространству и с `spike-integration`, тесты Rust, сборка, весь e2e и загрузка File Manager — зелёный; сам `menu` прошёл 340 запусков подряд.
- Не объяснено: в первых ~70 запусках `menu` дважды случился сбой — один раз `alef.exe` вышел с кодом 0xC0000409 (аварийное завершение процесса), один раз после снятия процесса не освободились процесс и каналы; текст паники тогда не сохранили, на том же бинарнике 340 последующих запусков (в том числе три параллельных цикла с `RUST_BACKTRACE=full`) прошли чисто, и в коде `muda` 0.19.3 подозрительного заимствования не найдено. Харнесс e2e теперь запускает приложение с `RUST_BACKTRACE=1` и при неожиданном выходе или неосвобождённых каналах печатает последние строки его вывода, чтобы следующий сбой оставил улики.

**Как сделано (M5.2, `shortcut`)** — `alef-core/src/registry/window/shortcut.rs`, `alef-modules/src/desktop/ui/shortcut.rs`, `alef-runtime/src/ui/integration/{shortcut,forward}.rs`, `packages/api/src/desktop/shortcut.ts`:

- Команды `shortcut.register` и `shortcut.unregister`, право `shortcut.global` (с подменой). Регистрация — ресурс документа: ответ `{ id, owner, token }`, где `id` — `s<сессия>:r<ресурс>`, а `token` — нативный идентификатор события (`null` при подмене, подмена инертна и ОС не трогает). Снять регистрацию может только документ-владелец, и только по handle ровно в этом виде; перезагрузка документа и закрытие окна снимают его регистрации.
- Нативный `GlobalHotKeyManager` и таблица регистраций принадлежат UI-потоку; единственный forwarder читает события крейта и будит winit. Событие уходит только живому документу-владельцу, только на нажатие, не на отпускание. Пределы: 128 регистраций, акселератор 1..256 байт без управляющих символов, токены не повторяются и не выходят из диапазона Win32 (1..0xBFFF); занятая комбинация — `ALREADY_EXISTS` (на macOS так отображается и отказ Carbon).
- `Shortcut.on('pressed')` фильтрует `owner` и `token`; событие неверной формы пропускается, а не роняет обработчик.
- e2e `shortcut`, `shortcut-denied`, `shortcut-substitute`: настоящая регистрация **Ctrl+Alt+Shift+F20** (на Linux и macOS — **Ctrl+Alt+Shift+F12**: F20 нет в их раскладках, CI Linux это показал), повтор → `ALREADY_EXISTS`, неверный акселератор, снятие и повторная регистрация, чужой handle из дочернего документа → `NOT_FOUND`, закрытие дочернего окна освобождает комбинацию; отказ и подмена — отдельными запусками с собственным каталогом согласий. Сценарий не создаёт реестровых записей и файлов вне временного каталога.
- Проверки (Windows): 810 тестов Rust по всему рабочему пространству, 207 JS, весь e2e (49 сценариев) и загрузка File Manager прошли; `clippy --workspace --all-targets -D warnings`, tsc, oxlint, структура, fmt и `gen:types --check` чистые. Мутации: 28 в Rust (`alef-modules` и таблица `alef-runtime`) и 14 в JS-обёртке, пойманы все. Первый прогон отпустил настоящих: различие модификаторов, предел токенов Win32, повторное освобождение при sweep, закрытие окна, отображение «комбинация занята», жизнь владельца, длину акселератора, тело `unregister`, форму handle и событие с пустым payload — под них добавлены тесты, `live_owner` и `Table::release_window` вынесены, чтобы их можно было проверить без цикла событий; проверка handle сведена к одному сравнению с каноническим видом (две другие были её следствием).
- Не сделано: tray. Настоящее нажатие ОС остаётся ручным (сценарий пишет `MANUAL shortcut-os-pressed-not-tested`; синтетических событий нет). Linux — только X11 с `DISPLAY` (Wayland → `NOT_AVAILABLE`), Linux и macOS локально не запускались: их ветки проверяет только CI. Подмена не проверена внешним конкурентом за комбинацию. Активный spike M0.3 владеет источником событий hotkey и исключает одновременную работу.

**Как сделано (M5.1, `app`: автозапуск и deep links)** — `alef-core/src/security/{manifest,permissions}.rs`, `alef-modules/src/desktop/app/{autostart,deeplink,args,instance}.rs`, `alef-modules/src/desktop/app/integration/{mod,registry,files,deeplink_platform}.rs`, `alef/src/{args,main,plan}.rs`, `packages/api/src/desktop/app.ts`:

- Манифест: `permissions.app.autostart` (по умолчанию `false`) и `deepLinks` — не больше 8 уникальных схем, каждая 1–64 байта, строчные, без зарезервированных (`http`, `https`, `file`, `ftp`, `ws`, `wss`, `data`, `blob`, `javascript`, `about`, `mailto`, `tel`). Права: `app.autostart` и `app.deepLinks` по схеме, оба с подменой — при подмене вызов уходит в запоминающий бэкенд, ОС не трогается; отказ по любой схеме набора отменяет весь набор до обращения к бэкенду.
- Команды `app.autostart.enable|disable|isEnabled` и `app.registerDeepLinks|unregisterDeepLinks` не принимают аргументов (схемы берутся из манифеста). Событие `open-url` доставляет адреса запуска и второго экземпляра: очередь 32 адреса по 8192 байта (переполнение сбрасывает самый старый), доставка начинается, когда обработчик подтвердил готовность через `app.openUrlIntercept`; адрес чужой схемы, с управляющим символом или пробелами по краям отвергается целиком вместе с пачкой. Сообщение второго экземпляра — одна строка до 256 КиБ; лаунчер берёт из командной строки за адрес только то, что разбирается как URL без управляющих символов, остальные слова остаются аргументами приложения.
- Нативный слой: Windows — `HKCU\...\Run` и `HKCU\Software\Classes\<схема>` с меткой владельца `AlefOwner`; Linux — `~/.config/autostart/*.desktop`, `~/.local/share/applications/*.desktop` и `xdg-mime` с копией прежнего обработчика; macOS — LaunchAgent для автозапуска, deep links отвечают `NOT_AVAILABLE` до подписанного бандла M7. Чужую запись (другая команда, чужая метка, лишние значения и ключи) не заменяют и не удаляют: дерево ключей схемы убирается снизу вверх только пока ключи пусты. Имя записи — `alef-` + hex идентификатора, команда — `exe --app папка` с обычным цитированием Windows и Desktop Entry.
- Выбор бэкендов: не-e2e запуск всегда настоящий (`Backends::system`); e2e (`ALEF_E2E=1`) притворный, а настоящие автозапуск и deep links только по `ALEF_E2E_APP_INTEGRATION=1`. Агент оставил в `Backends::from_environment` ошибку — вне e2e все бэкенды были запоминающими; исправлено двумя тестами.
- e2e `autostart` и `deeplink` пишут в настоящий реестр Windows, независимо читают его через PowerShell, ведут журнал созданных записей для очистки после сбоя и проверяют, что после запуска реестр чист. Страница читает файл раннера через `api.app.env` (в манифесте `permissions.app.env`), потому что `$APP` в путях API не раскрывается.
- Проверки (Windows): 864 теста Rust по всему рабочему пространству, 218 JS, весь e2e и загрузка File Manager прошли; `clippy --workspace --all-targets -D warnings`, tsc, oxlint, структура, fmt и `gen:types --check` чистые; ветки Linux и macOS у `alef-modules` проходят `cargo check --all-targets` для `x86_64-unknown-linux-gnu` и `aarch64-apple-darwin` (кросс-компиляция через `zig cc`). Реестр до и после гейта не содержит записей `alef-*` и `alefunit*`. Мутации: 63 в Rust и 18 в JS-обёртке, в итоге пойманы все (после правок повторялись только затронутые записи). Прогоны отпустили настоящее: не было тестов на управляющие символы и пробелы в адресе, на повтор, число и длину схем, цифру и заглавную букву в схеме, относительную папку, удаление файла при выключении, изменённый протокол в реестре, прерванный сигнал, прерывание во время подписки и падающий обработчик в `open-url`; фильтр `mimeapps.list` вынесен в чистую функцию с тестом. Остальное оказалось избыточным кодом и удалено: ветка `Deny` (отказ возвращает `check`), проверка `is_open` в цикле доставки, перепроверка файла перед удалением, проверка «исключительности» дерева в реестре (дублировала проверку пустоты) и защита от повторного `stop` в JS. Тесты реестра пишут в настоящий `HKCU` под уникальными именами и убирают их при любом исходе.
- Не сделано: macOS deep links (нужен подписанный бандл M7) и `SMAppService`; ручные проверки — переживает ли автозапуск перезагрузку ОС, настоящий щелчок по ссылке и холодный запуск через неё; ветки Linux и macOS выполняются только в CI (локально они типизированы, но не запускались), в том числе `xdg-mime` на раннере; Wayland и окружения без `xdg-mime` отвечают `NOT_AVAILABLE`.

## Риски

- Wayland: трей и глобальные хоткеи зависят от окружения рабочего стола.
- macOS: многое требует подписанного бандла (M7).
