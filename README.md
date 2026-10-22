# Alef runtime

Alef — десктопный runtime для приложений на HTML, CSS и JS/TS. Приложение запускается во встроенном Servo 0.6, а нативные возможности (окна, файлы, сеть, меню, трей, процессы, базы данных и другие) получает через типизированный JS API `@alef-runtime/api`. Rust автор приложения не пишет: runtime — готовый бинарник `alef`, он читает манифест `alef.ktav` и ассеты приложения.

- **Приложение — пакет `.alef`:** zip с манифестом, ассетами и подписью. Runtime ставится отдельно, как JRE; приложение находит нужную версию runtime. Установщик со встроенным runtime — необязательная опция.
- **Права подтверждает пользователь:** разрешить, подменить (приложение не отличает среду-двойник от настоящей) или отказать. Всё, что не объявлено в манифесте, закрыто.
- **Три режима одного бинарника:** окно, консоль (stdin/stdout, код выхода) и служба (без окна и консоли).
- **Платформы:** Windows x64, macOS arm64, macOS x64, Linux x64 (матрица CI).

Приложение-пример на этом runtime — [alef-file-manager](https://github.com/PHPCraftdream/alef-file-manager) (отдельный репозиторий; раньше жило здесь, репозиторий назывался AlefFileManager).

## Что в репозитории

| Каталог | Что |
|---|---|
| `backend/crates/alef-core` | Ядро без Servo: протокол кадров, сессии документов, таблица ресурсов, реестр команд, права, манифест, коды ошибок |
| `backend/crates/alef-modules` | Реализации API без Servo: desktop, system, data, net, media, ai и другие модули |
| `backend/crates/alef-package` | Формат пакета `.alef` на стороне runtime: чтение, подпись, проверка |
| `backend/crates/alef-runtime` | Servo и winit: протокол `native://`, окна, ввод, UI-диспетчер, мост до модулей |
| `backend/crates/alef` | Бинарник `alef`: лаунчер, установка и удаление приложений, права, версии runtime |
| `backend/patches` | Локальные патчи зависимостей: `servo-paint`, `servo-paint-api`, `surfman`, `winit` ([docs/PATCHES.md](docs/PATCHES.md)) |
| `packages/api` | `@alef-runtime/api` — JS API: async-вызовы, потоки, события, сегментированные подпути ([docs/api/SEGMENTS.md](docs/api/SEGMENTS.md)) |
| `packages/mcp` | `@alef-runtime/mcp` — MCP-сервер и клиент поверх модулей `http`, `websocket`, `cli` |
| `packages/tools` | `@alef-runtime/tools` — CLI `alef-runtime`: `keys`, `pack`, `verify`, `check`, `build`, `dev`, `bundle`, `fetch-runtime` |
| `packages/runtime` | `@alef-runtime/runtime` — мета-пакет: выбирает бинарник платформы из `@alef-runtime/runtime-<платформа>` |
| `packages/create-alef-runtime` | `npm create alef-runtime` — генератор проекта приложения (шаблоны `rsbuild` и `vite`) |
| `tests/e2e` | Сценарии на реальном окне и приложения-образцы `tests/e2e/apps/*` |
| `scripts` | Обёртка `cargo`, планировщик CI, генераторы релиза, проверки качества и цепочки поставки |
| `experiments` | Спайки, оставшиеся от этапа M0 |
| `docs` | План, архитектура, этапы, ревью и чекпойнты |

Модули `alef-core` и `alef-modules` не зависят от Servo: их проверка и тесты идут без сборки Servo. Всё, что требует главного потока (окна, меню, трей, глобальные хоткеи), модули выполняют через `UiDispatcher`, остальное — на Tokio.

## Для автора приложения

Пакеты `@alef-runtime/*` и `create-alef-runtime` пока не опубликованы в npm: релиз ещё не выпускался, сухой прогон публикации в CI проходит. До первого релиза команды запускаются из исходников (`node packages/tools/bin/alef-runtime.mjs …`). Рабочий цикл:

```sh
npm create alef-runtime        # проект приложения из шаблона
alef-runtime dev               # сервер сборщика и окно runtime
alef-runtime build             # каталог приложения: манифест, иконка, ресурсы
alef-runtime pack --key <имя>  # подписанный пакет .alef
alef run app.alef              # запуск пакета установленным runtime
alef-runtime bundle app.alef --platform linux-x64   # установщик с runtime внутри
alef-runtime fetch-runtime --platform win32-x64     # только бинарник runtime нужной платформы
```

`bundle` без `--runtime` и `fetch-runtime` берут runtime из npm-пакета платформы, проверяют его integrity по записи реестра и кладут в кэш (`$ALEF_RUNTIME_HOME/cache/runtime`, по умолчанию `~/.alef-runtime/cache/runtime`). Подробности — в [packages/tools/README.md](packages/tools/README.md) и [docs/stages/m7-distribution.md](docs/stages/m7-distribution.md).

## Командная строка runtime

```text
alef run <ПУТЬ|ID> [параметры] [-- <аргументы приложения>]
alef --app <КАТАЛОГ> [параметры] [-- <аргументы приложения>]
alef install <ПАКЕТ> [--trust-key <КЛЮЧ>] [--allow-unsigned]
alef uninstall <ID> [--purge-data]
alef apps [--json|--ui]
alef permissions list|set|reset <PATH|ID> … [--confirm-red]
alef ai limits|log …
alef trust <ЗАПРОС> <ОТВЕТ>
alef runtime list|install|prune …
```

`run` принимает каталог разработки, файл `.alef` или id установленного приложения. Параметры запуска: `--dev-url` (документ с сервера разработки на 127.0.0.1), `--devtools[=PORT]` (локальная отладка Servo, только для каталога разработки), `--grant allow|substitute|deny`, `--trust-key`, `--allow-unsigned`, `--no-prompt`. Полный текст — `alef --help` и `backend/crates/alef/src/cli/args.rs`. `ALEF_TIMING=1` печатает в stderr метки фаз холодного запуска (`ALEF_TIMING <имя> <мс от старта>`); список вех — [docs/stages/m11-warm-start.md](docs/stages/m11-warm-start.md).

## Сборка и проверки

Нужны Rust (минимальные версии: `alef-package` 1.88, `alef-core` 1.89, остальные крейты 1.90), Node.js >= 22.12 (CI — 24), зависимости сборки Servo и SpiderMonkey: clang с libclang, CMake, Ninja, Python 3; на Windows — MSVC Build Tools и Windows SDK. Локально проверено на Windows x64 (Rust 1.97.0 MSVC, Node.js 24.12.0, npm 11.13.0).

```sh
npm ci
npm run check        # Oxlint, лимиты структуры, tsc (packages/api, packages/mcp), Clippy всего workspace
npm run build        # cargo build всего workspace
npm run test:rust    # Rust-тесты
npm run test:e2e     # сценарии на реальном окне (нужен собранный alef)
```

Cargo запускается через `node scripts/cargo.mjs <команда>` — обёртка задаёт `--manifest-path backend/Cargo.toml` и `--jobs 1`. Тесты JS-пакетов: `test:api`, `test:mcp`, `test:tools`, `test:create`, `test:runtime`; скриптов: `test:ci`, `test:release`, `test:types`, `test:structure`, `test:mutate`.

`npm run lint:structure` держит в нашем коде (`backend/crates`, `scripts`, `packages`, `tests`, `experiments`) не более 7 элементов в каталоге и не более 1000 строк в файле (`scripts/quality/check-structure.mjs`).

## CI

- **CI:** планировщик `scripts/ci/plan.mjs` по изменённым файлам выбирает затронутые крейты, JS-пакеты и e2e-сценарии; полная матрица (Windows, macOS arm64, macOS x64, Linux) идёт на main и по расписанию. Подробности — [docs/stages/m0-results/ci.md](docs/stages/m0-results/ci.md).
- **Supply:** `cargo deny` (advisories, bans, sources), проверка лицензий крейтов и npm-пакетов по одному списку `backend/deny.toml` — только когда набор зависимостей изменился с последней зелёной проверки, MSRV и учёт патчей.
- **Release:** сухой прогон по `release-check/**` или вручную — сборка профиля `dist` на четырёх платформах, архивы и dry-run публикации npm; выпуск — по тегу.
- **Mutation:** мутационные проверки по запросу.

## Документация

| Документ | О чём |
|---|---|
| [docs/FRAMEWORK-PLAN.md](docs/FRAMEWORK-PLAN.md) | План: цель, архитектура, раскладка, контракты, каталог модулей, этапы |
| [docs/API-ROADMAP.md](docs/API-ROADMAP.md) | Исходный каталог JS API и решения |
| [docs/APP-RUNTIME.md](docs/APP-RUNTIME.md) | Alef как среда исполнения приложений |
| [docs/TRANSPORT.md](docs/TRANSPORT.md) | Транспорт v2 между страницей и runtime |
| [docs/BUS-ARCHITECTURE.md](docs/BUS-ARCHITECTURE.md) | Обязательная архитектура главного backend-потока |
| [docs/PATCHES.md](docs/PATCHES.md) | Локальные патчи зависимостей |
| [docs/api/SEGMENTS.md](docs/api/SEGMENTS.md) | Сегментированные подпути `@alef-runtime/api` |
| `docs/stages/` | Этапы M0–M11: [M11 — прогрев запуска](docs/stages/m11-warm-start.md); цель, объём, «Как сделано», приёмка; `m0-results/` — результаты спайков и CI |
| [RESIZE-INVESTIGATION.md](RESIZE-INVESTIGATION.md) | Расследование live resize |
| `docs/reviews/`, `docs/checkpoints/` | Ревью и снимки состояния работы по датам |

## Лицензия

`MIT OR Apache-2.0` — на выбор пользователя, см. `LICENSE-MIT` и `LICENSE-APACHE`. Исключения: `backend/patches/servo-paint`, `backend/patches/servo-paint-api` (текст — `LICENSE-MPL` в каталогах) и файлы `backend/crates/alef-runtime/src/window/` с заголовком MPL-2.0 (на основе примера Servo) — MPL-2.0; `backend/patches/winit` — Apache-2.0; `backend/patches/surfman` — `MIT OR Apache-2.0 OR MPL-2.0` (лицензии оригинала, `LICENSE-*` в каталоге).
