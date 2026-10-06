# M0 — Spike-проверки архитектурных рисков

Обзор: `../FRAMEWORK-PLAN.md`. Этап снимает пять рисков, от которых зависит устройство M1. Каждый spike — минимальный код, включаемый только переменной окружения или в `experiments/`, с замером и записанным решением. Продуктовый код не меняется, кроме явно указанного.

Общие правила:

- код spike — в `backend/crates/alef-runtime` (до разделения — `backend/runtime`) за `ALEF_SPIKE_<NAME>=1` или в `experiments/<name>/`; страницы сценариев сами отправляют отчёт в stderr и закрывают окно (как `experiments/transport-spike`);
- замеры — в release-сборке; при загруженной машине фиксируются соотношения, а не абсолютные числа;
- итог каждого spike записывается в конец этого файла (раздел «Результаты») и, если меняет архитектуру, — в `FRAMEWORK-PLAN.md`/`TRANSPORT.md`.

---

## M0.1 — Origin приложения

**Вопрос.** Можно ли отдавать документы приложения с настоящего https-origin, чтобы работали web storage и secure context, а API оставался на `native://`?

**Факты из исходников Servo 0.6.** `WebViewDelegate::load_web_resource(webview, WebResourceLoad)` вызывается для HTTP(S)-запросов, включая main frame (`WebResourceRequest.is_for_main_frame`). `WebResourceLoad::intercept(response)` → `InterceptedWebResourceLoad::send_body_data(chunk)` × N → `finish()`; потоковая отдача поддерживается. В `WebResourceRequest` нет тела запроса — POST-вызовы API так обслуживать нельзя. Сейчас у `native://` opaque origin `"null"`: `localStorage`/`sessionStorage` → `SecurityError`, `indexedDB` отсутствует.

**Шаги.**

1. Перехват `https://<app-id>.alef/*` в `load_web_resource`: GET → файл из каталога ассетов, потоково чанками 256 KiB, правильный `Content-Type`, CSP-заголовок; прочие хосты пока не трогать (`DoNotIntercept`).
2. Проверить, в каком потоке вызывается делегат, и можно ли передать `InterceptedWebResourceLoad` в задачу Tokio (`Send`). Если нет — чтение файла в Tokio, отправка чанков через канал в главный поток.
3. Тестовая страница с `https://<app-id>.alef/` сообщает: `location.origin`, `isSecureContext`, `localStorage`/`sessionStorage` (запись-чтение), `indexedDB` (открыть БД; если нет — проверить pref Servo `dom_indexeddb_enabled`), `crypto.subtle` (digest SHA-256).
4. С этой страницы вызвать `native://call` (существующий spike-маршрут или `native://invoke`): проходит ли CORS-preflight, нет ли блокировки как mixed content, задержка вызова в сравнении со страницей на `native://app`.
5. Загрузка ассетов: время загрузки JS-бандла File Manager через перехват и через `native://app`.
6. Блокировка внешних запросов на уровне перехвата: запрос к неразрешённому `https://example.com` → `cancel()`; убедиться, что это не ломает страницу (база для `external` из манифеста в дополнение к CSP).

**Критерий успеха (вариант A принят).** Origin — `https://<app-id>.alef`, `localStorage` работает, `isSecureContext === true`, вызовы `native://` из такой страницы работают, загрузка ассетов не медленнее `native://app` более чем в 1.5 раза.

**Запасной вариант B.** Если A не проходит — оценить патч Servo, дающий схеме `native` tuple origin (место: определение origin для URL с нестандартной схемой в `servo-url`/`script`), объём патча и риск при обновлениях Servo.

**Решение записывается:** выбранный вариант, origin-строка, список работающих/неработающих web API, влияние на транспорт (нужен ли preflight, заголовки CORS).

---

## M0.2 — Мультиоконность

**Вопрос.** Работает ли несколько окон с отдельными webview в одном экземпляре Servo 0.6 вместе с нашим патчем синхронного resize?

**Шаги.**

1. За `ALEF_SPIKE_MULTIWINDOW=1` по кнопке/команде создать второе окно winit: свой `WindowRenderingContext`, свой `WebView` (`WebViewBuilder::new(&servo, rendering)`), свой делегат.
2. Обработка событий окна по `WindowId`: `RedrawRequested`, `Resized` (синхронное ожидание кадра — для своего webview), ввод, закрытие.
3. Проверить: оба окна рисуются; resize каждого окна плавный, `ALEF_RESIZE_TRACE` показывает отсутствие таймаутов; закрытие второго окна не ломает первое; закрытие первого при открытом втором — корректное завершение или переход «главного» окна.
4. Проверить, что патч `servo-paint` (resize-wait) работает для каждого painter отдельно (по `PainterId`/rendering context), а не глобально.
5. Замерить прирост памяти на окно.

**Критерий успеха.** Два окна живут независимо, resize обоих без белых полос и таймаутов, закрытие любого окна корректно.

**Если не проходит.** Определить причину (общий GL-контекст surfman, ограничения painter) и оценить: отдельный rendering context на окно с общей share group, либо ограничение MVP одним окном + `<dialog>`-окнами внутри документа.

---

## M0.3 — Трей, меню, глобальные хоткеи и диалоги вместе с циклом winit

**Вопрос.** Уживаются ли `tray-icon`, `muda`, `global-hotkey`, `rfd` с нашим циклом winit (modal resize loop на Windows, синхронный resize)?

**Шаги.**

1. За `ALEF_SPIKE_INTEGRATION=1` в главном потоке после создания окна: иконка в трее с меню (2 пункта), меню окна (`muda`, для Windows — `init_for_hwnd`), глобальный хоткей (`Ctrl+Alt+Shift+A`), кнопка «Открыть файл» через `rfd::AsyncFileDialog` с родителем — HWND окна.
2. События трея/меню/хоткея (их каналы `receiver()`) пробрасывать в цикл winit через `EventLoopProxy` и логировать.
3. Проверить: клики по трею и пунктам меню приходят; хоткей срабатывает, когда окно не в фокусе; диалог модален относительно окна и не останавливает отрисовку; во время открытого диалога resize/drag других окон работает; resize/drag главного окна, maximize/restore — без регрессий (`ALEF_RESIZE_TRACE`).
4. Выход из приложения: иконка трея удаляется, хоткей снимается.

**Критерий успеха.** Все события доставляются, регрессий resize/drag нет. На macOS/Linux — повтор в CI/на машинах из M0.4 (macOS: меню приложения в строке меню; Linux: трей через AppIndicator, зависимость от `libayatana-appindicator`/GTK).

---

## M0.4 — Кросс-платформенная сборка в CI

**Вопрос.** Собирается и проходит ли тесты runtime на Windows, macOS и Linux; что ломается вне Windows?

**Требование.** Нужен пуш в GitHub (`PHPCraftdream/AlefFileManager`) — только с явного разрешения владельца.

**Шаги.**

1. Workflow GitHub Actions, матрица: `windows-latest` (x64), `macos-14` (arm64), `macos-13` (x64), `ubuntu-24.04` (x64).
2. Зависимости сборки Servo по платформам (Linux: пакеты из руководства Servo — clang, cmake, ninja, gstreamer dev не нужен при dummy media, X11/Wayland dev-библиотеки; macOS: Xcode CLT, cmake, ninja; Windows: MSVC, LLVM, cmake, ninja).
3. Кэш: sccache + `actions/cache` для `target` и cargo registry; цель — повторная сборка без изменений Servo < 15 минут.
4. Шаги: `npm ci` → `npm run check` → `npm run test:rust` → `npm run build`; артефакт — бинарник.
5. Linux без дисплея: e2e-сценарии через `xvfb-run` (X11) — проверить, что окно создаётся и spike-страница отрабатывает.

**Ожидаемые проблемы.** Патч winit — Windows-специфичные изменения должны компилироваться под `cfg`; surfman на Linux (EGL/GLX) и macOS (CGL); `window/platform` для macOS/Linux есть, но не проверены запуском; синхронный resize на macOS (live resize через `NSWindow`) и на Wayland ведёт себя иначе.

**Критерий успеха.** Зелёная сборка и тесты на всех четырёх конфигурациях; список платформенных расхождений с планом исправлений перенесён в M1/M2.

---

## M0.5 — Безоконный режим Servo

**Вопрос.** Может ли Servo исполнять JS приложения без окна и без дисплея: скрытый `WebView` на `SoftwareRenderingContext` (программный GL, есть в Servo 0.6), без winit? От ответа зависят режимы «консоль» и «служба», серверы (M4, M4b) и формулировка M4/M7.

**Минимум кода** (за `ALEF_SPIKE_HEADLESS=1` или в `experiments/headless/`): процесс без winit создаёт `SoftwareRenderingContext` 1×1, `Servo` и `WebView` с нашим `native://` транспортом, сам крутит `spin_event_loop` по `EventLoopWaker`, `webview.hide()`, кадры не рисуются; страница делает `app.info()` и `fetch` и печатает результат в stdout; завершение по SIGTERM/Ctrl+C.

**Среды:** Windows (обычный сеанс), Windows-служба (session 0, нет рабочего стола — главный риск: у нас GL через WGL с патченным `surfman`), Linux под `xvfb`, Linux **без `DISPLAY`** (EGL surfaceless / Mesa), macOS (без сеанса `WindowServer`, например по SSH).

**Замеры:** время от старта до первого `app.info()`; память процесса в простое; CPU в простое (refresh driver не должен крутиться у скрытого `WebView`); корректное завершение.

**Приёмка:** таблица «работает / не работает / что нужно доустановить» по средам и цифры; вывод — хватает ли одного режима, нужен ли запасной путь (скрытое окно 1×1, `Xvfb` в поставке), какие требования к системе писать в документацию, обещаем ли «службу» или только «консольное приложение».

## Результаты

M0.1: **вариант A принят** — `https://<app-id>.alef` через `load_web_resource`: secure context, `localStorage`/`sessionStorage`/IndexedDB (нужна настройка Servo `dom_indexeddb_enabled`), `native://` без preflight, внешние запросы отменяются до сети. Не работает `crypto.subtle` (фича `servo/webcrypto` выключена). Перехват буферизует тело целиком, capability в URL виден делегату (нужна редакция/иная доставка). Подробности: `m0-results/origin.md`.
M0.2: **мультиоконность работает** на одном Servo (два окна, свой webview и rendering context, resize-wait по painter независим, закрытие любого окна, код выхода 0); оракул `experiments/multiwindow-spike/run.sh` — PASS на объединённой сборке, strict-режим падает. Стоимость ≈ +150–250 МБ на окно (debug). Не проверено: физическое перетаскивание, IME, анимации и WebGL во втором окне, >2 окон, macOS/Linux. Подробности и дизайн для `window.create`: `m0-results/multiwindow.md`. Spike удалён в M2.2 (его место заняли реестр окон и e2e-сценарий `window`).
M0.3: **крейты уживаются с циклом winit**: tray-icon, muda, global-hotkey, rfd; достаточно существующего proxy-пробуждения и собственного канала (по одному потребителю на общий приёмник крейта — иначе события теряются); меню/трей снимать до закрытия окна; диалог не останавливает отрисовку, resize без таймаутов; оракул `experiments/integration-spike/run.sh` проверен (PASS, намеренный провал падает). Вручную: клик/меню/хоткей, модальность, Linux/macOS; пробел для M5: меню-ускорители. Подробности: `m0-results/integration.md`.
M0.4: **M0.4a выполнено** (workflow `.github/workflows/ci.yml` написан и проверен парсером YAML, не запускался); **M0.4b (push и запуск на трёх ОС) ждёт разрешения владельца на push**. Подробности: `m0-results/ci.md`.
