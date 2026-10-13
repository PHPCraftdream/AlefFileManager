# M0.4 — CI: кроссплатформенная сборка (M0.4a: workflow написан, не запускался)

## Сделано

- `.github/workflows/ci.yml`: матрица `windows-latest` (x64), `macos-14` (arm64), `macos-15-intel` (x64), `ubuntu-24.04` (x64), `fail-fast: false`; артефакты — бинарник (все ОС) и `frontend/dist` (отдельно, из Linux-джоба); Linux e2e под `xvfb-run`.
- Typecheck выполняется ровно один раз: полный `npm run build` (tsc + rsbuild + cargo) — только на Linux; на остальных ОС — `npm run build:rust`.

## Дизайн workflow

- Шаги: checkout → Node 24 с `cache: npm` (lockfile в репозитории есть) → Python 3 (`setup-python`) → Rust stable (`dtolnay/rust-toolchain@stable`; MSRV 1.88 = `backend/Cargo.toml` `rust-version`, cargo проверяет сам) → кэши (`actions/cache`: cargo registry и `backend/target`, ключи `hashFiles('backend/Cargo.lock')`) → sccache → зависимости сборки по ОС → `npm ci` → `npm run lint` + `npm run lint:structure` (все ОС) → `npm run test:rust` (все ОС) → сборка (Linux: `npm run build`; прочие: `npm run build:rust`) → Linux e2e → артефакты (`upload-artifact@v4`, `if-no-files-found: error`).
- sccache: `mozilla-actions/sccache-action@v0.0.7`, `SCCACHE_GHA_ENABLED`, `RUSTC_WRAPPER: sccache`; `scripts/cargo.mjs` и `scripts/dev.mjs` наследуют окружение (spawnSync), т.е. sccache действует и через npm-скрипты.
- e2e smoke (Linux): `ALEF_TRANSPORT_SPIKE=1 LIBGL_ALWAYS_SOFTWARE=1 timeout 180 xvfb-run -a … --frontend-dir experiments/transport-spike` — фронтенд-дист не нужен. Проверки: код выхода 0; в логе нет `panicked at`; последняя строка `spike report: {"phase":"main"` парсится как JSON, в нём нет верхнеуровневого `error` и есть непустой `results.echo`. Успешный и ошибочный отчёты страницы различаются именно этими полями (`experiments/transport-spike/spike.js:144-146`; успех содержит `results.echo`, заполненный на шаге 5); строку печатает `backend/crates/alef-runtime/src/bridge/mod.rs:266`.
- `timeout-minutes: 300`.

## Зависимости сборки (обоснование по крейтам из `backend/Cargo.lock`)

| Пакет/шаг | Кто требует | Статус |
|---|---|---|
| Win: `ilammy/msvc-dev-cmd@v1` | MSVC/SDK: link.exe для всех нативных сборок (mozjs_sys, aws-lc-sys, freetype-sys …) | требуется |
| Win: choco `llvm` | bindgen 0.72.1 (крейт mozjs) ищет libclang | требуется |
| Win: choco `cmake` / `ninja` | aws-lc-sys 0.45.0 (build-dep cmake); ninja — из README репозитория | cmake — требуется; ninja — precaution |
| macOS: `xcode-select -p` + brew `cmake ninja` | Xcode CLT (clang/libclang, тулчейн); surfman macOS — системные фреймворки (objc2, CGL) | CLT — требуется; cmake/ninja — precaution (обычно в образе) |
| Linux: `clang libclang-dev` | bindgen 0.72.1 (clang-sys) | требуется |
| Linux: `cmake` | aws-lc-sys 0.45.0 (build-dep cmake) | требуется |
| Linux: `ninja-build` | ни один build-скрипт в lock не требует; README | precaution |
| Linux: `pkg-config` | `-sys`-крейты без bundled-режима зондируют системные библиотеки через pkg-config (`yeslogic-fontconfig-sys` — в lock; в дереве сборки Servo freetype и harfbuzz уже собираются из bundled-исходников, см. строку ниже) | precaution |
| Linux: `build-essential` | bundled-сборка freetype-sys (C) и harfbuzz-sys (C++) через cc: Servo включает `bundled_freetype` (`servo-0.6.0/Cargo.toml`, фича `bundled`) и `harfbuzz-sys` с `features = ["bundled"]` (`servo-fonts-0.6.0/Cargo.toml`); `build.rs` freetype-sys (`if !cfg!(feature = "bundled")` → `pkg_config.probe("freetype2").unwrap()`) и harfbuzz-sys (`bundled` → `build_harfbuzz()`, иначе `probe_library("harfbuzz").unwrap()`) подтверждают, что при включённой фиче системные библиотеки НЕ используются; make — фолбэк mozjs | требуется |
| Linux: `python3`, `curl`, `git` | фолбэк-сборка mozjs из исходников (python/make; curl качает архив) | precaution: основной путь — готовый архив |
| Linux: `libfontconfig1-dev libfreetype-dev` | freetype и harfbuzz — bundled (строка выше), системные dev-пакеты для них не нужны; fontconfig: крейт `yeslogic-fontconfig-sys` 6.0.1 в lock (зависит от `dlib`, `pkg-config`), исходники на этой машине не скачаны (платформенная зависимость) — поведение сборки НЕ проверено | precaution (не проверено) |
| Linux: `libx11-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libegl1-mesa-dev libgl1-mesa-dev` | x11-dl / xkbcommon-dl / wayland-sys линкуются через dlopen — заголовки им не нужны | precaution |
| Linux (runtime): `libx11-6 libxcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0` | dlopen в winit (x11-dl, xkbcommon-dl, wayland-sys) | требуется в рантайме |
| Linux (runtime): `libegl1 libgl1 libglx-mesa0 libgl1-mesa-dri` | софтверный GL/EGL (llvmpipe) для smoke | требуется в рантайме |
| Linux (runtime): `libfontconfig1 libfreetype6` | fontconfig — dlib dlopen (yeslogic-fontconfig-sys) | требуется в рантайме |
| Linux: `xvfb` | headless X11 для smoke | требуется |

## Факты из исходников (OBSERVED)

| Факт | Где |
|---|---|
| surfman 0.13 на Linux с feature `sm-x11` (её включает патч `backend/patches/servo-paint-api/Cargo.toml:140-145`) использует дефолтный модуль `unix` — multi-бэкенд «dynamically switches between Wayland, X11 and surfaceless»: `Connection = MultiConnection<MultiDevice<WaylandDevice, X11Device>, SWDevice>` | surfman-0.13.0 `src/unix.rs:3,13-16`; выбор дефолта — `src/lib.rs:53-56` + `build.rs:23,33` (`wayland_default` ложен при `sm-x11` без `sm-wayland-default`) |
| `from_display_handle` у multi пробует основной бэкенд, при ошибке — альтернативный (итог: Wayland → X11 → surfaceless) | surfman-0.13.0 `src/multi/connection.rs:193-201` |
| X11-бэкенд surfman — EGL (`EGL_PLATFORM_X11_KHR`), не GLX; принимает Xlib-хэндлы, Xcb → `Unimplemented`, прочие → `IncompatibleRawDisplayHandle` | surfman-0.13.0 `src/x11/connection.rs:5-8,224-235` |
| Wayland-бэкенд принимает только Wayland-хэндлы (`_ => Err(IncompatibleRawDisplayHandle)`) | surfman-0.13.0 `src/wayland/connection.rs:180-193` |
| Следствие для xvfb (X11): wayland-ветка отклонит Xlib-хэндл, x11-ветка примет. Поведение на реальной Wayland-сессии НЕ проверено | инференс из строк выше |
| `LIBGL_ALWAYS_SOFTWARE=1` (софтверный Mesa EGL/llvmpipe) — допущение, на раннере не проверено | — |
| `mozjs_sys` 153.3.0-0 сначала качает готовый `libmozjs-<target>.tar.gz` с релизов `servo/mozjs` (curl); фолбэк — сборка из исходников (Windows: MozTools/mozmake; Linux/macOS: make+python) | `mozjs_sys-153.3.0-0/build.rs:78-117` (`download_archive`, `build_spidermonkey`, `find_moztools`) |
| Релиз `mozjs-sys-v153.3.0-0` содержит архивы для всех 4 целей CI (windows-msvc, apple-darwin ×2, linux-gnu) — список ассетов прочитан через GitHub API | github.com/servo/mozjs/releases/tag/mozjs-sys-v153.3.0-0 |
| bindgen 0.72.1 требует libclang на всех ОС | `backend/Cargo.lock` (bindgen, clang-sys) |
| `surfman::declare_surfman!()` (`backend/src/main.rs:9`) кроссплатформен: тело макроса целиком под `#[cfg(target_os = "windows")]` | surfman-0.13.0 `src/macros.rs:19-29` |
| Патч winit: Win32-правки только в `src/platform_impl/windows/*` за `#[cfg(windows_platform)]` | `backend/patches/winit/src/platform_impl/mod.rs:16-17,31-32` |
| Windows-крейты за cfg | `backend/crates/alef-runtime/Cargo.toml:31-32` (windows-sys 0.52); surfman Cargo.toml (winapi, wio — только Windows) |
| GStreamer не нужен: media — dummy (`media-gstreamer` не в default; в lock нет gstreamer-*) | servo-0.6.0 `Cargo.toml:54-58` |
| aws-lc-sys 0.45.0: NASM на Windows не обязателен (prebuilt-объекты, предупреждение) | aws-lc-sys-0.45.0 `builder/cc_builder.rs:651-654` |

## Ожидаемые проблемы (запуск не выполнялся)

| Файл | Почему может сломать | Фикс |
|---|---|---|
| `scripts/cargo.mjs`, `scripts/dev.mjs` | жёсткий `--jobs 1` — холодная сборка длительная; влияние на время НЕ измерено, риск упереться в 300 мин | env-переменная (раздел ниже) |
| `backend/target` в actions/cache | debug-дерево Servo — десятки ГБ; лимит кэша GitHub 10 ГБ на репозиторий, кэш будет вытесняться | основной механизм — sccache; target-кэш сузить/убрать при необходимости |
| сбой скачивания mozjs-архива | фолбэк-сборка SpiderMonkey: Windows — MozTools (на раннере нет); Linux/macOS — make+python (ставим в шаге зависимостей) | для Windows — MozillaBuild + `MOZTOOLS_PATH` как запасной план |
| libclang для bindgen | Windows: LLVM (choco/образ); macOS: Xcode CLT; Linux: `libclang-dev` | при проблемах задать `LIBCLANG_PATH` |
| `backend/crates/alef-runtime/src/window/platform/{linux,macos}.rs` | компилируются только на своей ОС (cfg в `platform/mod.rs`), до сих пор не собирались нигде кроме Windows | покажет первый прогон; правки локальны |
| `backend/crates/alef-runtime/src/window/platform/portable.rs` | матрицей НЕ компилируется вообще: cfg `not(any(windows, macos, linux))` (`platform/mod.rs:6-7`) | отдельная проверка, если появится нестандартная ОС |
| `backend/crates/alef-runtime/src/window/platform/macos.rs` (`SUPPORTS_NATIVE_RESIZE = false`) | рантайм, не сборка: drag/resize на macOS не поддержан winit | проверка на реальной macOS в M1/M2 |
| Wayland-сессия Linux | multi-бэкенд пробует Wayland-хэндл первым, но реальное поведение не проверено; Xcb-хэндлы — `Unimplemented` | проверка в M1; при проблемах `sm-wayland-default` или X11 |
| Windows-раннер без GPU | WGL-контекст требуемого уровня не гарантирован (не проверено) | e2e оставлен на Linux; при необходимости ANGLE (feature `no-wgl` патча servo-paint-api → `sm-angle-default`) |

## Предложение: override `--jobs 1` (scripts вне скоупа)

`scripts/cargo.mjs:8` передаёт `--jobs 1` явно; CLI-флаг приоритетнее `CARGO_BUILD_JOBS`. Предложение (по строке на скрипт):

```js
const jobs = process.env.ALEF_CARGO_JOBS ?? '1';
// ...(command === 'fmt' ? [] : ['--jobs', jobs])
```

`ALEF_CARGO_JOBS: "4"` уже стоит в env ci.yml, но начнёт действовать ТОЛЬКО после этой правки scripts; сейчас переменная ни на что не влияет, локальное поведение (1) не меняется.

## Что не проверено

- Workflow не запускался: нужен пуш в `PHPCraftdream/AlefFileManager` — только с явного разрешения владельца.
- Время холодной/тёплой сборки по ОС; состав предустановленного ПО в образах раннеров; лимит кэша против `backend/target`; e2e на ubuntu (софтверный EGL); поведение на реальной Wayland-сессии.
- Компиляция `window/platform/{linux,macos}.rs` и патчей на не-Windows ОС — инференс по cfg-структуре, не сборка.
- Версии actions — мажорные теги, не SHA-пины (закреплены позже, см. дополнение ниже).

## Ручные проверки человеку

1. Первый запуск через `workflow_dispatch`; скорректировать шаги зависимостей по факту.
2. Решить по `ALEF_CARGO_JOBS` (правка scripts — вне скоупа M0.4a).
3. Проверить размер кэша `backend/target`, оба артефакта (бинарник + `alef-frontend-app`).

## Команды и результаты

- Валидация YAML: парсер подмножества YAML на Node без зависимостей (скрипт передан в `node` через stdin, в репозиторий не попадал). Результат: разбор до EOF без синтаксических ошибок; семантика PASS — 4 цели матрицы, `fail-fast: false`, у каждого из 20 шагов `uses`/`run`, typecheck только на Linux, оба артефакта, `${{ }}` сбалансированы.
- Логика assert'а smoke проверена локально на образцах: успех (включая вложенный `results.abort.error`) — PASS; верхнеуровневый `error` — корректный FAIL; отсутствие `results.echo` — корректный FAIL; `panicked at` в логе — корректный FAIL.
- `node scripts/check-structure.mjs` — PASS.
- Размеры файлов (`wc -l`): ci.yml = 190, ci.md = 106.

## Зависимости

Новых Cargo/npm зависимостей нет; `Cargo.toml`/`Cargo.lock` не менялись. Actions: `actions/checkout@v4`, `actions/setup-node@v4`, `actions/setup-python@v5`, `actions/cache@v4`, `actions/upload-artifact@v4`, `mozilla-actions/sccache-action@v0.0.7`, `dtolnay/rust-toolchain@stable`, `ilammy/msvc-dev-cmd@v1`.

## Вывод

Каркас CI готов к первому пушу. Главные неизвестные — фактическое время сборки с `--jobs 1` и состав образов раннеров; по коду препятствий компиляции для macOS/Linux не видно, платформенные расхождения — в рантайме (Wayland-сессии, native resize на macOS, GPU на Windows-раннере).

## Дополнение оркестратора

- Образ `macos-13` (x64) выведен из эксплуатации GitHub: задание стояло в очереди без раннера (в двух запусках подряд более двух часов, пока его не отменял следующий пуш). Заменён на `macos-15-intel`; результат на Intel-macOS появится с первым запуском после замены.
- Строки таблицы про freetype/harfbuzz/fontconfig исправлены по `build.rs` крейтов: bundled-режим включён Servo, системные dev-пакеты для них не нужны (оставлены как precaution); поведение `yeslogic-fontconfig-sys` не проверено (исходники не скачаны на Windows-машине).

## Дополнение: поставка и аудит (R-025, R-027)

Что сделано после ревью (`docs/reviews/2026-10-09-full-review.md`):

- **Действия закреплены по SHA**, тег — в комментарии той же строки (`actions/checkout`, `setup-node`, `setup-python`, `cache`, `upload-artifact`, `sccache-action`, `msvc-dev-cmd`, `rust-toolchain`, `taiki-e/install-action`). SHA — те коммиты, на которые указывали теги на 2026-10-11, то есть версии не менялись. Обновлять — осознанно: взять новый SHA тега и поменять строку.
- **Компилятор закреплён** (`toolchain: 1.99.0` в ci.yml) вместо плавающего `stable`; то же значение использовали зелёные запуски на 2026-10-11.
- **Нижняя граница компилятора проверяется** (`supply.yml`, задание `msrv`): `alef-core` собирается на 1.88.0, `alef-modules` — на 1.90.0. `rust-version = "1.88"` у `alef-modules`, `alef-runtime` и `alef` было неверным: `fjall` и `lsm-tree` требуют 1.90 (по `cargo metadata`), поэтому значение поправлено на 1.90. Для `alef-runtime` и `alef` нижняя граница определяется Servo и отдельно не проверяется; они собираются закреплённым компилятором в ci.yml.
- **Аудит зависимостей** (`supply.yml`, `cargo-deny@0.19.9`, `backend/deny.toml`): уязвимости, лицензии, источники. Известных уязвимостей нет; восемь уведомлений «не поддерживается» (`bincode`, `proc-macro-error`, `ttf-parser`, шесть `unic-*`) — транзитивные зависимости Servo, внесены в `ignore` с причиной. Лицензии — разрешающие и MPL-2.0 (Servo); пары «A OR B» проходят по разрешённой стороне. Повторяющиеся версии крейтов — предупреждение, не отказ (под Servo это норма).
- **Копии крейтов в `backend/patches`** сверяются с опубликованными (`docs/PATCHES.md`, `scripts/supply/check-patches.mjs`, задание `patches`).
- **Мёртвая переменная `ALEF_CARGO_JOBS` убрана** из ci.yml; предложение о `--jobs` выше остаётся предложением.
- **e2e на Windows**: первый запуск всего набора (52 сценария) показал 8 PASS и 44 отказа при создании оконного контекста на раннере без GPU (`Cannot create Servo graphics context: RequiredExtensionUnavailable`, WGL). Обязательным остался шаг с восемью сценариями без окна (`--only`); полный набор на программном контексте на этом раннере не работает (см. «Как сделано (#107)»).

Не сделано: e2e на macOS и Linux-расширение (отдельная задача, в конце); версии пакетов `choco`, `brew` и `apt-get` не закреплены (образы раннеров меняются, закреплять нечем без собственного образа).

## Дополнение: изоляция тестовых хуков (R-018)

Дизайн: обычные сборки оставляют `e2e` и `spike-origin` выключенными. В CI Rust-сборки для e2e явно используют `node scripts/cargo.mjs build --locked --workspace --features alef/e2e`; на Linux перед ними остаётся `npm run build:frontend`. Отдельный Linux-шаг собирает `node scripts/cargo.mjs build --locked -p alef --release` без features и передаёт `backend/target/release/alef` сканеру `scripts/supply/check-no-hooks.mjs` (маркеры в UTF8 и UTF16LE). Тесты сканера добавлены в блок Generated TypeScript types; поставка, загрузка артефактов и остальные настройки не менялись.

Наблюдаемые результаты локальной проверки на Windows:

- Rust-тесты в обоих вариантах (plain и `e2e`), включая doctests, — PASS; workspace clippy plain и `e2e` + `spike-origin` — PASS.
- Сканер plain **DEBUG**-бинарника: 0 совпадений для пяти строк (`e2e.dropFiles`, `e2e.once`, `e2e.flood`, `ALEF_E2E`, `ALEF_SPIKE_ORIGIN`) в UTF8/UTF16LE. Feature **DEBUG**-бинарник отклонён: найдены четыре маркера. Контракт сканера проверен; сканирование настоящего release-бинарника локально **не запускалось** — Linux-шаг добавлен в CI, его результат здесь не заявляется.
- Полный Windows e2e: **55 PASS**, с существующими пропусками visible startup и service-signal; Ctrl — ручная проверка. Plain-запуск с враждебным окружением: exit 3, файлы прав/согласия не созданы. Все десять мутаций обнаружены проверками и восстановлены.

**Артефакты CI не для распространения:** upload по-прежнему берёт DEBUG-бинарники с feature `e2e` и тестовыми хуками, а не проверяемый release без features. Изменение upload вне скоупа; он сохранён.

Структура: сканер и его тесты размещены в `scripts/supply`; существующие скрипты не переносились.

**Как сделано (#107)**

- Тихое окно e2e (feature `e2e`, `ALEF_E2E=1`, `ALEF_E2E_QUIET=1`, без `ALEF_E2E_VISIBLE`) рисует на `SoftwareRenderingContext` размера окна: на Windows всегда, на других ОС при `ALEF_E2E_SOFTWARE=1`. Скрытое нативное окно остаётся (геометрия, мониторы, хэндлы меню); видимость и фокус по-прежнему логические (`Pretended`). Обычная сборка этого кода не содержит: маркер `ALEF_E2E_SOFTWARE` в ней отсутствует (проверено).
- На машине с GPU полный набор: 55 PASS (прежние пропуски: видимый старт, service-signal, SIGTERM на переднем плане); 17 мутаций ограничителей и выбора контекста пойманы.
- **Итог эксперимента на раннере без GPU (#107, #116).** Шаг-эксперимент в CI на `9da96e0` дал 16 PASS (сценарии без окна) и 47 FAIL, все с `Cannot create Servo software graphics context: RequiredExtensionUnavailable`. Механизм: бэкенд WGL нашей копии surfman в `Device::new` требует расширение `WGL_NV_DX_interop` и устройство `D3D_DRIVER_TYPE_HARDWARE` (`backend/patches/surfman/src/wgl/device.rs`), а программный адаптер делегирует аппаратному low-power (`connection.rs`); у OpenGL «GDI Generic» раннера этого нет. Проверка гипотезы «поставить программный OpenGL»: Mesa 26.2.4 (llvmpipe), `opengl32.dll` и `libgallium_wgl.dll` рядом с копией exe, запуск на машине с GPU — сценарии `window` и `restore` падают с тем же сообщением, то есть Mesa нужного расширения тоже не даёт. Шаг-эксперимент удалён из CI (около 9 минут на прогон); на Windows-раннере остался обязательный шаг с восемью сценариями без окна. Это же ограничение продукта: на Windows без аппаратного OpenGL (виртуальные машины, серверы без GPU) приложение не создаёт ни окно, ни скрытый WebView (`no software rendering context`). Выход — другой бэкенд поверхности (ANGLE на D3D11/WARP: features surfman `sm-angle-builtin`/`sm-angle-default`, сборка mozangle) или раннер с GPU; вынесено в отдельную задачу.

## Дополнение: мутационное тестирование в CI

После готовой фичи оркестратор ломает её важные строки по одной («мутации») и требует, чтобы хотя бы один тест упал: мутация, которую не заметил ни один тест, — это отсутствующий тест. Раньше это делал локальный скрипт (часы компиляций на машине пользователя), теперь это workflow `mutation.yml`: `gh workflow run mutation.yml -f list=archive`, затем `gh run watch <id> --exit-status`. Список — JSON-массив в `scripts/quality/mutations/<list>.json`; элемент — `{name, file, from, to, package, features?, test?}`: харнес (`scripts/quality/mutate.mjs`) заменяет единственное вхождение точного текста `from` на `to` в файле `file`, запускает `node scripts/cargo.mjs test --locked -p <package>` (для ускорения список делится на 4 шарда: `--shard i/4`) и относит исход к одной из четырёх категорий: **CAUGHT** — тесты упали (exit 101 и `FAILED` в выводе), **ESCAPED** — тесты прошли (мутант выжил: отсутствующий тест или избыточный код; чинится в продуктовом репозитории, не здесь), **INVALID** — мутант не компилируется или команда упала иначе (`error[`/`could not compile`); это дефект списка или окружения, запуск считается проваленным, **TIMEOUT** — зависание; считается поимкой, только если в выводе видно идущий тест (`test … …`), иначе это дефект. `ANCHOR_MISSING`/`ANCHOR_AMBIGUOUS` — якорь `from` не найден или найден не единожды: список надо чинить. Выживший мутант — вывод о продукте: добавить тест или удалить мёртвый код в продуктовом коде, а не в этом workflow.

## Замер CI (2026-10)

### Linux-x64 (CI, `main`)

| Шаг | #142 | #136 | #127 |
|---|---|---|---|
| Ожидание очереди | 40.9 | 33.9 | 1.6 |
| Весь джоб (всего) | 63.1 | 54.8 | 23.1 |
| Шаг джоба (работа) | 22.2 | 20.9 | 21.5 |
| Cache backend/target | 0.7 | 1.2 | 0.6 |
| Servo build prerequisites | 0.4 | 0.8 | 0.4 |
| Rust tests | 3.7 | 3.7 | 3.5 |
| Desktop backends | 0.4 | 0.4 | 0.4 |
| Generated TypeScript types | 0.5 | 0.4 | 0.4 |
| Build (typecheck + frontend + Rust) | 2.4 | 2.2 | 2.3 |
| Release build without test hooks | 11.5 | 9.7 | 11.2 |
| e2e (alef on Servo, headless X11) | 1.4 | 1.3 | 1.5 |

### Windows-x64 (CI, `main`)

| Шаг | #142 | #136 | #127 |
|---|---|---|---|
| Ожидание очереди | 40.0 | 31.6 | 23.9 |
| Шаг джоба (работа) | 39.3 | 19.2 | 20.0 |
| Cache restore | — | 0.6 | 0.8 |
| Servo prerequisites (Windows) | 2.2 | 2.0 | 2.1 |
| Rust tests | 23.0 | 8.1 | 8.4 |
| Desktop backends | 2.5 | 0.8 | 0.8 |
| Generated TypeScript types | 0.9 | 1.0 | 1.0 |
| Build (Rust, без frontend) | 5.1 | 4.5 | 4.5 |
| Post Cache backend/target (сохранение) | 2.7 | — | — |

### macOS-x64 (CI, `main`)

| Шаг | #142 | #136 | #127 |
|---|---|---|---|
| Ожидание очереди | 39.8 | 34.5 | 24.5 |
| Шаг джоба (работа) | 40.9 | 31.6 | 25.6 |
| Rust tests | 24.8 | 13.8 | 12.0 |
| Desktop backends | 2.9 | 1.4 | 1.3 |
| Build (Rust, без frontend) | 8.0 | 10.7 | 6.5 |
| Post Cache (сохранение) | 2.1 | — | — |

### macOS-arm64 (CI, `main`)

| Шаг | #142 | #136 | #127 |
|---|---|---|---|
| Ожидание очереди | 39.6 | 0.1 | 9.7 |
| Шаг джоба (работа) | 11.4 | 11.5 | 11.3 |
| Rust tests | 4.9 | 3.4 | 3.3 |
| Build (Rust, без frontend) | 3.6 | 2.1 | 2.1 |

### angle-win, последний завершённый (#140, failure; Windows-джоб)

| Показатель | Мин |
|---|---|
| Ожидание очереди Windows-джоба | 45.0 |
| Rust tests (Windows, ANGLE) | 20.9 |
| Build (Rust, без frontend; ANGLE) | 13.5 |
| Desktop backends | 2.2 |
| e2e Windows lifecycle/integration: app | 0.5 |
| e2e Windows desktop/system/processes: modules | 0.9 |
| Весь джоб (работа) | 43.4 |

Остальные джобы #140 прошли как обычно (Linux-джоб не отличается от `main`).

### Ответы на вопросы замера

1. Что доминирует по ОС: Linux — release-сборка (9.7–11.5 мин, ~50% работы джоба) и очередь (1.6–40.9 мин); Windows — Rust tests (8.1–23.0 мин) и очередь; macOS-x64 — Rust tests (12.0–24.8 мин) и build (6.5–10.7). Доля e2e на Linux — 1.3–1.5 мин из ~21–22 мин работы джоба ≈ 6–7%. Доля Windows ANGLE build — 13.5 из 43.4 мин ≈ 31%.
2. Разбивка e2e по сценарным группам из списка шагов НЕ видна: API отдаёт только суммарное время шага; логи не скачивались (лимит запросов), поэтому per-group времена не получены.
3. Дубли работы: (а) на Linux debug-Rust компилируется трижды (Rust tests → Build typecheck+frontend+Rust → release), суммарно ~17.6 мин Rust-компиляций; на Win/mac — дважды (tests + build), 5–28 мин суммарно; (б) lint/fmt/structure/types/JS-тесты чистых пакетов выполняются на всех четырёх ОС, хотя не зависят от системы (каждый шаг <20 c, поэтому в списках шагов не виден — суммарно до ~1 мин × 3 лишних ОС); (в) workflow `Supply` шагов CI не дублирует (cargo-deny, msrv alef-core 1.88 / alef-modules 1.90, patches — уникальные), вычисления ≤1 мин на джоб, но стеночное время запуска #73 — 41.2 мин из-за очереди (джоб cargo-deny ждал 38.5 мин); два предыдущих запуска Supply отменены concurrency. Пакетный сценарий дважды на Linux не подтверждается списками шагов (шаг e2e один).
4. Кэш: restore `backend/target` 0.4–2.4 мин (максимум mac-x64 #127, 142 c), сохранение 2.1–2.7 мин (когда ключ новый). Холодный vs тёплый прогон одного джоба напрямую не сравнить без логов (sccache-счётчики в логах — пропущено по условию). Косвенный признак разброса кэша: Rust tests Windows 8.1–23.0 мин и macOS-x64 12.0–24.8 мин при прочих равных — вероятно разница warm/cold.
5. Ранжированные экономии (оценка на прогон, риск):
   1. Очередь: 30–45 мин стеночного времени на джоб матрицы — крупнейшая статья; убирается только бо́льшим числом/перезалуком раннеров (деньги), технического риска нет. Планировщик `scripts/ci/plan.mjs` сам очередь не убирает.
   2. Схлопывание debug-компиляций в одну (tests+build): ~3–8 мин на Windows/mac-x64, ~2 мин на Linux; риск низкий (порядок шагов).
   3. Стабилизация тёплого кэша `backend/target` (сохранять чаще/точнее ключи): до ~15 мин на Windows и mac-x64 в худших прогонах; риск средний (лимит кэша 10 ГБ, вытеснение).
   4. Release-сборка на Linux: только `-p alef` уже сделано; экономить кэшированием release-цели/узким набором — ~2–4 мин; риск низкий.
   5. Системо-независимые проверки (lint, fmt, structure, types, JS чистых пакетов) один раз на Linux: ~0.5–1 мин × 3 ОС; риск низкий.
   6. Кэш choco-пакетов Windows (llvm, cmake) против шага prerequisites 2.0–2.2 мин: ~1.5 мин; риск низкий.
   7. e2e Linux 1.4 мин — оптимизировать не стоит.
   Оговорка: ветка `ci-plan` с `scripts/ci/plan.mjs` на удалённом репозитории отсутствует (404 на 2026-10-13), поэтому какие из экономий планировщик убирает сам — по данным НЕ проверено; оценка дана по описанию задачи.

### Использованные запросы gh api (без токенов)

```
gh api "repos/PHPCraftdream/AlefFileManager/actions/runs?branch=main&status=success&per_page=15" --jq '.workflow_runs[] | select(.name=="CI")'
gh api "repos/PHPCraftdream/AlefFileManager/actions/runs?branch=angle-win&per_page=20" --jq '.workflow_runs[] | select(.name=="CI")'
gh api "repos/PHPCraftdream/AlefFileManager/actions/runs?branch=main&per_page=15" --jq '.workflow_runs[] | select(.name=="Supply")'
gh api "repos/PHPCraftdream/AlefFileManager/actions/runs/<id>/jobs?per_page=100" --jq '.jobs[] | {name, conclusion, created_at, started_at, completed_at, steps: [.steps[] | {name, sec: ((.completed_at|sub("\\.[0-9]+Z$";"Z")|fromdate) - ((.started_at|sub("\\.[0-9]+Z$";"Z")|fromdate)))}]}'
```

Всего израсходовано 9 запросов `gh api`; логи не скачивались, запуски не запускались и не отменялись. Пар длин «>20 c» в таблицах нет: показаны все шаги с шаговым временем.
