# M1 — Ядро фреймворка

Обзор: `../FRAMEWORK-PLAN.md`. Транспорт: `../TRANSPORT.md`. Зависит от M0 (решения origin, мультиоконности, интеграции, CI).

## Цель

Фреймворк как платформа: crates, транспорт v2, сессии и ресурсы, реестр команд с правами, манифест `alef.ktav`, генерация TS-типов, каркас `@alef-tron/api`, генерический бинарник `alef`. Существующее окно и события переведены на новый транспорт; File Manager продолжает работать (его Rust-команды временно живут на новом реестре до M3).

**Не входит:** новые модули API (M2+), упаковка (M7).

## 1. Структура crates

```
backend/
  Cargo.toml                       workspace; [patch.crates-io] без изменений
  crates/
    alef-core/src/
      lib.rs
      error.rs                     ErrorCode, AlefError, From<io::Error>
      protocol/                    frame.rs (кодек кадров), credit.rs, call.rs (Payload)
      session/                     session.rs (SessionId, жизненный цикл), resources.rs, streams.rs (hub)
      registry/                    command.rs (builder, Handler), context.rs (CallContext), dispatch.rs
      security/                    manifest.rs (структуры alef.ktav), permissions.rs, scope.rs, csp.rs
    alef-modules/src/lib.rs        пусто до M2 (регистрация модулей)
    alef-runtime/src/              перенос прежнего backend/runtime: bridge/, window/, ui.rs, store.rs
    alef/src/main.rs               генерический бинарник
  src/                             File Manager до M3 (регистрирует свои команды через новый реестр)
```

Зависимости: `alef-core` — без Servo и winit (tokio, serde, ktav `=0.8.0`, ts-rs, bytes). `alef-runtime` → `alef-core`. Правило структуры (≤ 7 элементов, ≤ 700 строк) — на каждом уровне.

## 2. Транспорт v2

**Unary-вызов** `POST native://call/<command>` (`../TRANSPORT.md`):

- тело: `application/json` (аргументы) или `application/octet-stream` (данные) + аргументы в заголовке `x-alef-args` (JSON, percent-encoded);
- ответ всегда потоковый (`DoneChannel` + чанки ≤ 256 KiB) — даже маленький JSON; `Content-Type` `application/json` или `application/octet-stream`;
- ошибки: статус + `{ code, message, details? }`;
- токен в `Authorization: Bearer`; если M0.1 выбрал https-origin, CORS-заголовки разрешают только origin приложения.

**Потоки** `GET native://stream/<id>`: кадры `[kind u8][len u32 LE][payload]` (1 json, 2 binary, 3 end, 4 error). Служебные команды:

| Команда | Назначение |
|---|---|
| `runtime.stream.ack { id, bytes }` | вернуть кредит (окно 1 MiB) |
| `runtime.stream.close { id }` | отмена со стороны JS (вешается на `AbortSignal`) |
| `runtime.stream.write { id }` + бинарное тело | чанк во входящий поток; ответ после приёма (backpressure) |
| `runtime.stream.end { id }` | конец входящего потока |

Guard в runtime всегда завершает Servo-канал `Data::Done`/`Cancelled` (иначе паника Servo, `methods.rs:931`).

**События документа**: при старте `@alef-tron/api` открывает поток `runtime.events.subscribe` — все события runtime (окно, приложение, модули) идут кадрами json. `evaluate_javascript`-доставка удаляется.

**Рукопожатие** `runtime.hello` → `{ protocol: 1, runtime: "x.y.z", platform, arch, modules: [...], permissions: {...} }`. Несовпадение `protocol` — ошибка инициализации с понятным текстом.

## 3. Сессии и ресурсы

- Сессия = документ (окно + документ-pipeline Servo). Создаётся первым `runtime.hello` документа; более новый документ того же окна заменяет сессию (токен прежнего умирает сразу), более старый вернуть её не может; закрывается при закрытии окна. `LoadStatus::Started` сессиями не управляет: Servo не присылает его для первой загрузки, а для остальных статус доходит до embedder'а независимо от скриптов страницы.
- Токен выдаётся на сессию; запросы со старым токеном → `PERMISSION_DENIED`.
- Таблица ресурсов: `ResourceId → Box<dyn Resource>` (`async fn close()`), владелец — сессия. Закрытие сессии закрывает ресурсы и потоки (входящие получают `CLOSED`, исходящие — завершение источника).
- Лимиты на сессию: число ресурсов, число одновременных вызовов (сейчас 32), суммарный кредит потоков.

## 4. Реестр команд и права

```rust
registry.command("window.setTitle")
    .permission(Permission::None)
    .handler(|ctx: CallContext, args: SetTitle| async move { ... });
```

- `Handler` получает `CallContext` и десериализованные аргументы; бинарное тело — через `ctx.body()`.
- Ответ: `Json(T)`, `Bytes(Vec<u8>)`, `Stream(StreamId)`.
- Права проверяются диспетчером до вызова: `Permission` команды + `scope` из аргументов (`#[scope]`-поле DTO) против манифеста и грантов сессии.
- Пространство `runtime.*` зарезервировано.

## 5. Манифест `alef.ktav`

Формат и правила — `../FRAMEWORK-PLAN.md` §6.1, геометрия окон — §6.2.

- Структуры в `alef-core/security/manifest.rs`, `#[serde(deny_unknown_fields)]`, без `default` у `external` и `permissions`.
- Загрузка: `ktav::from_str` → `Manifest`; ошибка → `MANIFEST_INVALID` с именем поля (строка и колонка — только у синтаксических ошибок Ktav); runtime не открывает окно.
- CSP строится из `external` (`connect-src`, `script-src`, `style-src`, `img-src`, `font-src`, `media-src`, `frame-src`) + обязательные источники транспорта Alef; применяется ко всем документам приложения.
- Scope-шаблоны путей: `$APPDATA`, `$APPCONFIG`, `$APPCACHE`, `$HOME`, `$DOCUMENTS`, `$DOWNLOADS`, `$DESKTOP`, `$TEMP`, `$APP` (каталог приложения); сопоставление после канонизации (symlink, `..`).

## 6. Генерация TS-типов

- DTO команд, событий и манифеста: `#[derive(TS)] #[ts(export)]` (`ts-rs`).
- `npm run gen:types` → `packages/api/types/*.ts`; CI: генерация + `git diff --exit-code`.

## 7. `@alef-tron/api` — каркас

```
packages/api/
  package.json        name @alef-tron/api, type module, exports ./src/index.ts (до M7 — без сборки)
  types/              сгенерированные типы
  src/
    index.ts
    core/   transport.ts, stream.ts, events.ts, resource.ts, errors.ts, handshake.ts
    desktop/window.ts (перенос текущего nativeWindow)
```

- `call(command, args, { body?, signal? })`, `openReadable(id)`, `openWritable(id)`, `on(event, cb, { signal })`, `AlefError`.
- Тест «всё асинхронное»: обход экспортов модулей, каждая функция → `Promise`/`AsyncIterable` (`node --test`).
- Подключение во frontend File Manager: alias `@alef-tron/api` → `packages/api/src` (rsbuild + tsconfig paths); `frontend/src/native/runtime.ts` заменяется импортами из пакета.

## 8. Генерический бинарник `alef`

`alef --app <dir>` (читает `<dir>/alef.ktav`, ассеты `<dir>/<assets из манифеста>`), `alef --app <dir> --dev-url http://127.0.0.1:3000`. Окна создаются по `windows` манифеста. File Manager до M3 остаётся отдельным бинарником на `alef-runtime` со своими командами.

## Порядок работ

1. Разделение crates без изменения поведения (проверка: build/test/clippy/e2e как сейчас).
2. `alef-core`: error, protocol (кодек + credit + тесты), session/resources/streams (тесты на закрытие, кредит, лимиты).
3. Registry + dispatch + permissions/scope + manifest (тесты: отсутствующие разделы, неизвестные поля, scope-сопоставление, CSP-строка).
4. Bridge: маршруты `call`/`stream`, сессии по документу (`runtime.hello`), события; удаление `native://invoke` и spike-маршрутов (стенд переезжает в `tests/e2e`).
5. ts-rs + `gen:types` + CI-проверка.
6. `@alef-tron/api` core + перевод окна и событий; File Manager на новом API.
7. Бинарник `alef`; `tests/e2e` — раннер и сценарии.

## Приёмка

| Проверка | Как |
|---|---|
| Unary JSON и бинарный (16 MiB туда и обратно, целостность) | e2e |
| Credit: при медленном читателе runtime не превышает окно 1 MiB | e2e + лог runtime |
| `abort()` → источник закрыт ≤ 100 мс | e2e + лог |
| Reload → все ресурсы и потоки сессии закрыты; старый токен отклонён | e2e |
| Нет права → `PERMISSION_DENIED`; scope вне манифеста → отказ | Rust unit + e2e |
| Пустой `external.connect` → внешний `fetch` заблокирован; разрешённый адрес → проходит | e2e |
| Манифест без `external`/`permissions` или с неизвестным полем → запуск отклонён с путём ошибки | Rust unit + запуск |
| События окна приходят через поток событий | e2e |
| Все экспорты `@alef-tron/api` асинхронные | `node --test` |
| Сгенерированные типы совпадают с закоммиченными | CI |
| File Manager работает как раньше | ручная проверка + e2e смоук |
| `npm run check`, `test:rust`, `lint:structure` зелёные на Windows/macOS/Linux | CI |

## Риски

- Объём рефакторинга bridge — разбивать на шаги с зелёной проверкой после каждого.
- Навигация внутри SPA (history API) не создаёт нового pipeline — сессия и ресурсы сохраняются, так и задумано.
- CORS/preflight при https-origin (M0.1) удваивает число запросов — замерить, при необходимости отказаться от `Authorization` в пользу токена в URL пути.

## Статус реализации

Сделано и влито (проверено `test`/`clippy -D warnings`/`fmt --all --check`/`lint:structure`):

- **M1.1** разделение на crates (`alef-core`, `alef-modules`, `alef-runtime`, `alef`).
- **M1.2** `alef-core`: кодек кадров (`FrameDecoder` терпит любые границы чанков, лимит 16 MiB проверяется до буферизации), `CreditGate` (окно 1 MiB; `acquire(n > window)` → `TooLarge`; закрытие будит ожидающих), `parse_args_header`, `Limits`, `ResourceTable` (закрытие в обратном порядке, один раз), `StreamHub` (исходящие с кредитом, входящие с backpressure; guard: потерянный writer всегда даёт терминальный кадр; закрытие будит заблокированную запись), `SessionManager` (токен из внешнего источника, перезагрузка закрывает прежнюю сессию окна, лимит одновременных вызовов).
- **M1.3** `alef-core`: манифест `alef.ktav` (`Manifest::from_ktav_str`), `Length` (`px`/`%screen`/`%work`), `build_csp`, `PermissionSet`/`Grants`, scopes путей/URL/сокетов/программ, `Registry` с проверкой прав до обработчика.

- **M1.4a** `alef_core::protocol::transport` — слой запрос/ответ без Servo: `Transport::new(TransportConfig, Registry, Arc<SessionManager>, Arc<PermissionSet>)` и `async handle(TransportRequest) -> TransportResponse`. Маршруты `POST call/<команда>`, `GET stream/<id>`, `OPTIONS`; токен сессии привязан к окну (`TransportRequest.window` — непрозрачный id, его выводит обвязка из webview); `runtime.hello` отдаёт токен текущей сессии окна тому, кто предъявил bootstrap-токен (и допустимый `Origin`); `runtime.stream.{ack,close,write,end}` — обычные runtime-команды реестра. Ответ `hello`: `{protocol:1, runtime, platform, arch, modules:[...], token, limits:{maxUnaryBody,maxBulkBody,streamWindow,chunkSize,maxResources,maxConcurrentCalls}}`. Любой отказ в аутентификации — один и тот же 403 `{"code":"PERMISSION_DENIED","message":"permission denied"}`. Паника обработчика → 500 `INTERNAL` без текста паники; обрыв клиентом (drop будущего) отменяет вызов и прерывает задачу — обработчики должны быть безопасны к отмене. `CallContext` теперь держит `Arc<Session>` (`ctx.streams()`, `ctx.resources()`, `ctx.grants()`); страничные входящие потоки открывают обработчики через `StreamHub::open_incoming_reader`.

- **M1.4b** `alef-runtime::bridge` — обвязка Servo поверх транспорта: `ProtocolHandler` отдаёт `native://call/<команда>` и `native://stream/<id>` в `Transport`; тело ответа идёт ТОЛЬКО через канал тела fetch чанками ≤ 256 KiB с гарантированным терминалом (`Done`/`Cancelled`; иначе Servo паникует), обрыв fetch отменяет команду и закрывает источник потока. Окно запроса — `WindowRegistry` по `Request.target_webview_id` (webview привязывается сразу после создания); документ — `Request.pipeline_id` (Servo создаёт новый pipeline на каждую загрузку, включая reload, и нумерует их по порядку), поэтому `runtime.hello` сам создаёт или заменяет сессию (`SessionManager::session_for_document`: проверка и замена атомарны, окно 0 — «нет окна» — сессии не получает, устаревший документ получает обычный 403). `EventBus`: события окна и runtime идут потоком `runtime.events.subscribe` (очередь 256, переполнение сбрасывает подписчика с `BUSY`); CSP документов `native://app` — `build_csp` или встроенная по умолчанию. Команды File Manager живут в реестре как `app.<имя>` (окно — `window.apply`). При закрытии окна: `windows.unbind` и `sessions.close_window`.
- **M1.5** TS-типы из Rust (`ts-rs =12.0.1`, MIT; фичи `serde-json-impl`, `no-serde-warnings`; в `Cargo.lock` добавлены только `ts-rs`, `ts-rs-macros`, `termcolor`). DTO `alef-core` (`ErrorCode`, `AlefError`, `SessionId`/`ResourceId`/`StreamId`, манифест и его подструктуры, `Monitor`, `WindowDef`; `Length` и `WindowPosition` описаны вручную: принимается `number | string` / `"center" | {x, y}`, выдаётся строка) помечены `#[derive(ts_rs::TS)]` и `#[ts(export, export_to = "core.ts" | "manifest.ts")]`; `u64` → `number` (JSON-число, точность до 2^53), необязательные поля — `?:`. Вывод тестов `export_bindings_*` идёт в `backend/target/ts-rs` (`.cargo/config.toml`, `[env]`), `scripts/gen-types.mjs` сливает его в `packages/api/types/{core,manifest,index}.ts` (сортировка по имени типа, LF, баннер, проверка `tsc --ignoreConfig`). `npm run gen:types` пишет файлы, `npm run check:types` генерирует во временный каталог и падает при любом расхождении с закоммитченным (CI), `npm run test:types` — тесты генератора (идентичность двух прогонов и коммита, один изменённый байт/пропавший/лишний файл, нормализация). Rust-тесты сверяют union `ErrorCode` с `as_str()` в обе стороны и форму манифестных типов. Новый DTO: `#[derive(ts_rs::TS)]` + `#[ts(export, export_to = "<группа>.ts")]`, затем `npm run gen:types` (инструкция — в шапке скрипта). Мутации: смена doc-комментария в Rust → `check:types` падает (`manifest.ts: differs at line 12`); смена wire-строки `ErrorCode` → Rust-тест на union падает.
- **M1.6** `packages/api` (`@alef-tron/api`, пока без сборки: `exports` указывает на `src/index.ts`, во frontend — alias в rsbuild и `paths` в tsconfig). Поверхность: `connect()`, `call(команда, args, { body?, signal? })` (JSON-ответ или `Uint8Array`; ошибки — `AlefError { code, message, details, status }`, код `TRANSPORT` — сбой самого транспорта), `openReadable(id)` / `openWritable(id)`, `on(событие, cb, { signal })`, `nativeWindow` (прежний API окна на `window.apply`). Рукопожатие — один раз на документ, токен сессии прикладному коду не отдаётся. Читатель потока сам подтверждает кредит пачками ≥ 256 KiB (в кредит входят все байты полезной нагрузки, и json-кадры тоже), закрывает источник при выходе из цикла/`close()`/сигнале; `on()` держит один поток `runtime.events.subscribe` на документ и после `BUSY` подписывается заново. Все экспорты асинхронные (`node --test` обходит экспорты). Тесты — `npm run test:api` (32 теста на фиктивном runtime; запуск `node --experimental-strip-types`, поэтому в исходниках нет `enum`/параметров-свойств и импорты с расширением `.ts`); мутации: json-кадры без ack, кэш неудачного рукопожатия, bootstrap вместо токена сессии, нет закрытия при раннем выходе, снимок окна до подписки, устаревшие ревизии, неразрезанные куски записи, события не по имени — каждая роняет свой тест. Отклонение от наброска: `resource.ts` не создан — закрывать ресурсы по id нечем, пока нет модулей с ресурсами (M3), `Resource` появится вместе с `fs`. File Manager работает на пакете: `frontend/src/native/runtime.ts` удалён; маршрут `native://invoke` (теперь 404), доставка событий через `evaluate_javascript` и очередь `UiRequest::Emit` удалены — события окна и приложения идут только потоком. `RuntimeHandle::emit` возвращается после постановки в очередь (раньше — после выполнения JS). Под `ALEF_LOG_CALLS=1` runtime пишет в stderr по строке на запрос (`ALEF_CALL <маршрут> <статус>`; без заголовков, тел и токенов).
- **Смоук на реальном Servo** (до M1.7; в M1.7 заменён `tests/e2e`, см. ниже — `experiments/m1-smoke` удалён, раннер и страница перенесены; `ALEF_M1_SMOKE=1` теперь `ALEF_E2E=1`; `node experiments/m1-smoke/run.mjs --exe <бинарь> [--expect-fail] [--verbose]`): hello, единый отказ без токена/с чужим/с bootstrap, JSON-эхо, бинарное эхо 16 MiB без потерь, команда File Manager через реестр и через старый `invoke`, окно credit (4 MiB, не более 1 MiB в полёте, 3 остановки), отмена fetch → источник остановлен за 3–7 мс (лимит раннера 250 мс, цель e2e 100 мс), события потоком, затем навигация и настоящий reload (новый токен, токен прежнего документа → 403). `--expect-fail` включает индуцированную порчу эха: раннер обязан увидеть FAIL ровно в `binary-echo-16MiB` и `lib-binary-roundtrip-4MiB`. С M1.6 страница смоука идёт на настоящей библиотеке (раннер транспилирует `packages/api` через `tsc --rewriteRelativeImportExtensions` рядом со страницей): помимо сырых проверок — `lib-*` (connect, JSON, бинарное эхо 4 MiB, отображение ошибок, самостоятельные ack при чтении 8 MiB, закрытие источника, события, `nativeWindow.watch`), а после навигации и reload — `*-library-reconnects`; раннер сверяет полный список проверок, задержку закрытия источника для обоих потоков (≤ 250 мс) и пик окна кредита каждого потока (≤ 1 MiB). `experiments/m1-smoke/boot.mjs` запускает настоящий File Manager со собранным `frontend/dist` и требует успешный стартовый трафик (`runtime.hello`, `app.hello`, `preferences.get`, подписка и поток событий, `window.apply`) без ошибок страницы. Состояние между документами смоук переносит через URL: Servo не сохраняет `window.name`, а у кастомной схемы можно менять только фрагмент (смена query — навигация). В debug-сборке эхо 16 MiB занимает 5–8 с — скорость мерить в release на гейте M1.
- **M1.7** генерический бинарник `alef` (крейт `backend/crates/alef`: библиотека `alef_launch` + `main`): `alef --app <каталог> [--dev-url http://127.0.0.1:PORT]` читает `<каталог>/alef.ktav`, строит CSP (`build_csp(.., "native://app")`: у непрозрачного origin `'self'` в Servo не совпадает, поэтому источник указан явно), разрешённые origin и права из манифеста, раздаёт файлы приложения по `native://app/…` и открывает окно манифеста. Что рантайм пока не умеет, он отклоняет до открытия окна (код выхода 2, сообщение с кодом ошибки и именем поля): процентные размеры окна, несколько окон, `monitor`/`position`/`restore` не по умолчанию — до модуля окон M2.2; молча урезать манифест не пытается. `--dev-url` — только `http://127.0.0.1`, без учётных данных; документ грузится с dev-сервера, а `runtime.hello` из этого origin принимается (bootstrap-токен идёт во фрагменте). Servo не добавляет заголовок `Origin` для собственных схем, поэтому мост подставляет origin документа из `Request.origin` (для `native://` — `null`). Маршруты spike (`bridge/spike.rs`) и `experiments/{m1-smoke,transport-spike}` удалены; команды `e2e.*` и `e2e.report` существуют только при `ALEF_E2E=1`, порча эха — `ALEF_E2E_BREAK`.
  `tests/e2e` (`npm run test:e2e`, `node tests/e2e/run.mjs [--exe <alef>] [--only core,induced,permissions,csp,dev,manifest]`): каждое приложение — каталог с `alef.ktav` и страницей, итог страница сообщает через `e2e.report`, раннер сверяет полный список проверок. Сценарии → пункты таблицы приёмки: `core` — unary JSON и бинарное эхо 16 MiB, окно кредита (пик ≤ 1 MiB), `abort` → источник закрыт ≤ 250 мс, события окна потоком, навигация и reload (новый токен, старый → 403, брошенный поток закрыт рантаймом), те же проверки через `@alef-tron/api`; `induced` — порча эха, раннер обязан увидеть FAIL ровно в двух бинарных проверках; `permissions` — чтение внутри scope разрешено, вне — отказ, запись без права — отказ, из переменных окружения доступна только перечисленная; `csp` — пустой `external.connect`: внешний `fetch` заблокирован (0 запросов на сервер), указанный адрес проходит (1 запрос), встроенный скрипт заблокирован, транспорт под политикой работает; `dev` — страница с HTTP-origin, вызовы, бинарный обмен, события и отказ чужому HTTP-origin, получившему bootstrap-токен; `manifest` — 14 случаев отказа/справки запуска (нет `external`/`permissions`, неизвестное поле, относительный scope, не-origin в `connect`, id-путь, нет окна, проценты, два окна, нет `alef.ktav`, неизвестный флаг, не-loopback dev-url, `--help`). Для работы нужен дисплей, поэтому в CI e2e пока не запускается (Linux — `xvfb`, отдельным шагом). Проверка «File Manager работает как раньше» — `tests/e2e/boot-file-manager.mjs` (собранный `frontend/dist`, обязательный стартовый трафик без ошибок страницы).

Решения реализации, важные для следующих шагов:

- `Registry::dispatch` — `async` (обработчики асинхронные). Паника обработчика **не** перехватывается (задача tokio завершится — мост обязан превратить это в `INTERNAL` и завершить поток).
- Файловые права: `PermissionSet::authorize_path` возвращает **канонический путь** — модуль `fs` обязан работать с ним, а не с исходной строкой (проверенный и используемый путь совпадают). Канонизация идёт по компонентам: `..` применяется, существующие компоненты резолвятся (symlink/junction), висячая ссылка, Windows-алиасы (`file.`, `file:stream`, `NUL`) и относительные пути — отказ. TOCTOU между проверкой и открытием остаётся на модуле `fs` (открывать без следования ссылкам, где возможно).
- `Grants::grant_read/write` возвращают `Result`; каталог даёт поддерево, файл (в том числе ещё не существующий) — только себя; чтение и запись раздельны.
- Права на сеть: `net.http`/`shell.openExternal` — схемы `http|https|ws|wss`, шаблон без пути = весь origin, `/*` = префикс по сегментам, иначе точный путь; кандидат разбирается строго (ASCII, без userinfo, без `.`/`..`/`%2e`/`%2f`/`%5c`, порт 1..65535). `net.socket` — `tcp|udp|listen:host:port`, `*` для хоста и порта только в шаблоне.
- `cli.exec`: `*`, голое имя или абсолютный путь; `.exe` не домысливается; регистр — по ОС.
- `PermissionSet::with_window_create()` — право на создание окон для встраивающего хоста; в манифесте его даёт необязательный раздел `permissions.window.create` (M2.2: универсальный `alef` не имеет Rust-хоста, который мог бы включить право).
- `CallContext::new` создаёт канал отмены, который сам не срабатывает; `with_cancel(handle)` привязывает канал вызывающего (его сброс = отмена).
- Тесты, работающие с диском, создают каталоги в `CARGO_TARGET_TMPDIR`; случаи с симлинками при невозможности их создать пишут `NOT EXERCISED` и падают при `CI=1`.
- Гейт форматирования: `node scripts/cargo.mjs fmt -- --check` теперь проверяет весь workspace (`--all`); раньше проверялся только корневой пакет.

Известные ограничения M1.4b:

- `runtime.hello` из дочернего iframe приложения с более новым pipeline заменит сессию окна: по запросу нельзя отличить iframe от верхнего документа. Чужие `Origin` отсекаются по-прежнему; поддержка iframe — отдельным шагом.
- Сессия старого документа живёт до `hello` нового или до закрытия окна (при уходе на страницу без `hello` ресурсы остаются до закрытия окна).
- Привязка webview к окну идёт сразу после `build()`; запрос страницы, опередивший её, получил бы окно 0 и отказ — на практике скрипт страницы стартует много позже.

Не проверено на этой машине (проверит CI): ветки `cfg(unix)`/macOS (регистр, симлинки Unix, тест литерального `*` в `$HOME`); смоук на Linux/macOS.

### Приёмка M1 (гейт)

По таблице «Приёмка» выше, на коммите `dfa5311` (CI зелёный на windows-x64, macos-arm64, macos-x64 и linux-x64):

- **e2e** (`tests/e2e`, сценарии `core`, `induced`, `permissions`, `csp`, `dev`, `manifest`) — все проверки таблицы (unary и бинарный обмен, окно кредита, отмена, перезагрузка, права, CSP, манифест, события окна) проходят на Windows локально и на Linux (под `xvfb`) в CI; на macOS e2e в CI не запускается (нет дисплея), там — сборка и Rust-тесты.
- **Замер в release** (`alef` из `cargo build --release`, тихий режим, Windows, машина не под нагрузкой; числа ориентировочные): отмена `fetch` → источник закрыт за **1 мс** (цель ≤ 100 мс); пик данных в полёте при медленном читателе ровно **1 MiB** (= окно кредита, 3 остановки на 8 MiB); бинарное эхо 16 MiB туда и обратно **3,6 с** (около 9 MiB/с суммарно), через библиотеку 4 MiB — 0,72 с; чтение потока 8 MiB библиотекой — 25 мс (около 320 MiB/с). Отправка тела запроса заметно медленнее выдачи потока: узкое место, судя по цифрам, в отправке тела `fetch` из Servo, а не в протоколе. Порога скорости в таблице приёмки нет; цифры зафиксированы как точка отсчёта для M3 (потоки `fs`).
- **`npm run check`, `test:rust`, `lint:structure`** — зелёные в CI на всех четырёх раннерах; сгенерированные типы совпадают с закоммиченными (шаг `check:types`).
- **File Manager работает как раньше**: смоук `tests/e2e/boot-file-manager.mjs` (собранный `frontend/dist`, стартовый трафик без ошибок страницы) проходит; ручная проверка глазами — за владельцем (окна тесты не показывают).
