# Live resize: проблема и попытки исправления

> Пути ниже указаны до реструктуризации и могут быть устаревшими. Текущее расположение (`backend/crates/alef-runtime/src/`): `host.rs` → `window/{mod,app,state,delegate}.rs`; `resize_wait.rs` → `window/resize_wait.rs`; `window_frame.rs` → `window/input/frame.rs`; `wheel.rs` → `window/input/wheel.rs`; `platform/` → `window/platform/`; `bridge.rs`/`commands.rs` → `bridge/mod.rs`/`bridge/commands.rs`; `frontend/src/runtime.ts` → `frontend/src/native/runtime.ts`. Упоминания `backend/runtime` и `backend/crates/alef-runtime/src/host.rs` в тексте читать с учётом этой карты.

## Статус

**Проблема не исправлена.** Пользователь проверил последнюю запущенную версию после коммита `e95520b` и сообщил, что при увеличении окна белый контейнер расширяется рывками, а содержимое отдельно догоняет его, тоже рывками.

Конечное совпадение размеров native окна и Servo viewport не доказывает плавность отображения во время перетаскивания. Предыдущая проверка была недостаточной для закрытия этой задачи.

## Ожидаемое поведение

- Resize краями и углами включён по умолчанию, может быть отключён через framework API.
- Во время удержания мыши размер окна, rendering surface и отображаемое содержимое обновляются согласованно.
- Нет белых промежуточных областей и отдельного запаздывающего скачка содержимого.
- После maximize → restore перенос и resize продолжают работать.

## Наблюдаемые дефекты

1. При увеличении окна возникает белая расширяющаяся область.
2. Внешняя область и содержимое меняются не как единый визуальный кадр: содержимое догоняет размер окна отдельно.
3. Обновления видны как рывки, а не плавный native resize.

Это пользовательское наблюдение — основание считать текущую реализацию неисправной. Источник белого цвета пока не установлен: нет доказательства, что его рисует Windows, graphics surface, compositor либо документ Servo.

## Текущая архитектура

- Native HWND и event loop предоставляет локально patched winit 0.30.13.
- В том же процессе работает Servo 0.6.0.
- `backend/crates/alef-runtime/src/host.rs` связывает native input, resize, Servo event loop и presentation.
- `backend/crates/alef-runtime/src/window_frame.rs` выполняет native edge/corner hit-testing.
- `backend/crates/alef-runtime/src/platform/` содержит платформенные реализации.
- `backend/patches/servo-paint-api/rendering_context.rs` предоставляет `WindowRenderingContext`, resize graphics surface и presentation.
- React frontend показывает custom titlebar и состояние окна; DOM resize overlays удалены. Изменение размеров выполняет OS/winit, а не React.

## Что уже было изменено

### 1. Native drag/resize coordinates

Файл: `backend/patches/winit/src/platform_impl/windows/window.rs`.

В Windows drag helper значение `WM_NCLBUTTONDOWN.lParam` формировалось из адреса stack `POINTS`, хотя Windows ожидает packed signed screen coordinates. Заменено на упаковку двух signed 16-bit координат в значение `LPARAM`.

Добавлена проверка signed coordinates, включая отрицательные координаты монитора и границы i16. Она прошла.

Это исправляет запуск OS drag/resize, но само по себе не синхронизирует rendering.

### 2. Зависание drag после maximize → restore

Файлы:

- `backend/patches/winit/src/platform_impl/windows/window.rs`;
- `backend/patches/winit/src/platform_impl/windows/window_state.rs`;
- `backend/patches/winit/src/platform_impl/windows/event_loop.rs`;
- `backend/crates/alef-runtime/src/host.rs`.

Изменения:

- maximized state читается из native HWND, а не только из cached flags;
- перед изменением window flags maximized flag синхронизируется с native состоянием;
- drag guard больше не устанавливается заранее только из намерения начать drag: lifecycle привязан к `WM_ENTERSIZEMOVE`/`WM_EXITSIZEMOVE`;
- restore не вызывает снятие minimized/maximized без необходимости.

В одном из native smoke после maximize → restore наблюдались перенос на +24/+16 px и resize на +30 px при удержании мыши. Это подтверждает восстановление возможности move/resize, но не отсутствие белых кадров и рывков на всём жесте.

### 3. Native edges вместо DOM overlays

Файлы: `host.rs`, `window_frame.rs`, `ui.rs`, `frontend/src/runtime.ts`, `frontend/src/App.tsx`.

- Hit-testing выполняется в native host: 8 logical pixels с DPI scaling; corners имеют приоритет.
- При нажатии на edge вызывается OS `drag_resize_window`, соответствующий mouse down не передаётся в DOM.
- `WindowOptions::new` задаёт `resizable = true`.
- Добавлены `WindowAction::SetResizable` и `nativeWindow.setResizable(enabled)`.
- При отключении resizing edge hit-testing не запускает resize.

Browser-кнопка в свежем native процессе переключала Windows `WS_THICKFRAME`: true → false → true. Это проверка опциональности, не проверка качества live presentation.

### 4. Синхронизация viewport с текущим native размером

Файл: `backend/crates/alef-runtime/src/host.rs`, методы `State::synchronize_viewport` и `State::redraw`.

Попытка устранить работу с устаревшими размерами queued `WindowEvent::Resized`:

- текущий размер читается через `window.inner_size()`;
- zero-size не передаётся в Servo;
- при несовпадении вызывается `webview.resize(size)`;
- после вызова проверяется совпадение `rendering.size()` и native client size;
- синхронизация выполняется также перед redraw.

На размерах 1231×800, 1273×800 и 1327×800 native geometry до/после capture совпадала, а frontend показывал соответствующий размер. Это доказывает согласование установившегося состояния, но не атомарное изменение кадра во время drag.

### 5. Ожидание уведомления о новом кадре

Файл: `backend/crates/alef-runtime/src/host.rs`.

- Добавлен общий `frame_ready: Rc<Cell<bool>>`.
- `WebViewDelegate::notify_new_frame_ready` выставляет его и запрашивает redraw.
- При изменении surface флаг сбрасывается, выставляется `resize_pending`.
- Пока resize pending и уведомления нет, `redraw` не вызывает `paint`/`present`.
- После уведомления вызываются `webview.paint()` и `rendering.present()`.
- Удалён преждевременный explicit redraw непосредственно из обработки Resized.

**Эта попытка не решила пользовательский дефект.** Bool-флаг не содержит viewport size, generation или layout epoch. В host нет проверки, что уведомление относится именно к последнему resize и что документ уже построил layout/display list для этого размера. Также ожидание кадра не доказывает сохранение корректного отображения после изменения native graphics surface.

### 6. Cursor и публикация состояния во время modal resize

Файл: `backend/crates/alef-runtime/src/host.rs`.

- Пока активен native resize, host не переключает resize cursor обратно в default по промежуточному hover.
- Native window state публикуется не только в `about_to_wait`, но и из wake/window event обработки, чтобы не зависеть только от обычного цикла вне Windows modal resize.

Эти изменения относятся к cursor/state lifecycle. Они не являются доказанным исправлением рывков rendering.

## Что проверено и чего проверки не доказывают

Проверено:

- Последний выполненный Rust test run: 3 теста приложения и 11 runtime tests прошли; Clippy и frontend checks прошли.
- Отдельный winit regression test signed packed coordinates прошёл.
- Production native приложение запускается, команды custom titlebar работают.
- В свежем процессе maximize давал native/client UI состояние 1604×998; restore возвращал предыдущие 1327×800.
- Restore glyph в maximized состоянии содержит два перекрывающихся квадрата.
- Maximized client origin `(158, 41)` совпадал с work area `(158, 41, 1762, 1039)`: дополнительный верхний gap относительно доступной области — 0 px. Work area не менялась глобальными Windows settings.
- Конечные client sizes и UI state совпадали для нескольких нечётных ширин.

Не доказано:

- Отсутствие белых промежуточных кадров при непрерывном увеличении окна.
- Готовность содержимого именно для размера каждого представляемого кадра.
- Плавность всего жеста, а не нескольких отдельных состояний.

`PrintWindow` captures и конечные geometry measurements недостаточны для этих утверждений. Они не заменяют наблюдение пользовательской поверхности во время непрерывного resize.

Часть попыток physical-input smoke была остановлена safety guard: `WindowFromPoint` возвращал HWND чужого `Qt51519QWindowIcon`, а `SetForegroundWindow` возвращал false. Пробовались свежий native процесс и временный `HWND_TOPMOST`; устойчивый owned hit не был получен. Физический mouse down в чужое окно не отправлялся. Эта помеха тестированию не объясняет дефект приложения и не отменяет пользовательский результат.

## Что установлено чтением Servo source

Resolved Servo 0.6.0, `servo-paint/painter.rs::resize_rendering_context`:

1. Делает graphics context current.
2. Сразу вызывает `rendering_context.resize(new_size)`.
3. Меняет viewport rect каждого webview renderer и вызывает `notify_viewport_updated`.
4. Отправляет WebRender transaction с новым document view.
5. Отправляет root pipeline display list и выставляет `RepaintReason::Resize`.

Таким образом, graphics surface меняется до завершения последующей работы document/layout/render pipeline. Наш host проверяет размер surface, но не доказал соответствие готового содержимого этой surface.

В winit Windows window class `hbrBackground = 0`; поэтому объяснение «winit явно очищает окно белой class brush» не подтверждено. Нужно исследовать дальнейший Windows/graphics/compositor путь, а не назначать причину по цвету.

## Следующее направление расследования — не готовое решение

- Проследить `WM_SIZE`/modal event delivery → surface resize → viewport notification → document reflow/display list → WebRender frame → paint → present.
- Определить, какой этап создаёт белую область и какие поколения viewport/layout реально представлены.
- Проверить, связаны ли кадры, допускаемые текущим bool `frame_ready`, с последним размером.
- Исправлять lifecycle resize/presentation, а не скрывать белый цвет фоновой заливкой, debounce до mouse release либо fake browser resize.
- После исправления наблюдать native окно при удержании мыши на edge/corner, увеличении и уменьшении, включая maximize → restore. Конечный screenshot не считать достаточным acceptance.

Дополнительных изменений rendering после нового пользовательского сообщения пока не внесено. Этот файл фиксирует неисправное состояние и уже испробованные подходы, а не объявляет задачу решённой.

## Исправление синхронизации resize (реализовано, ожидает визуальной проверки)

### Найденная первопричина (подтверждено чтением исходников)

- `servo-paint-0.6.0` `Painter::render()` сначала очищает всю surface цветом `shell_background_color_rgba` (по умолчанию белый), затем рисует содержимое, выложенное для СТАРОГО viewport. При этом `resize_rendering_context` меняет размер surface и сразу сообщает кадр как готовый → белая полоса.
- Display list содержимого для нового размера приходит позже (script reflow → `handle_new_display_list` → сборка сцены WebRender → кадр готов) → содержимое «догоняет» рывками.
- `frame_ready: bool` в host принимал любой кадр независимо от размера; present выполнялся асинхронно относительно `WM_SIZE` → рывки.

### Что изменено

**`backend/patches/servo-paint`** — новый path patch через `[patch.crates-io]` в `backend/Cargo.toml`. Добавлен `resize_wait.rs`:

- пока активен resize-wait, `RepaintReason::Resize` и `NewWebRenderFrame` не триггерят `notify_new_frame_ready`;
- готовность сообщается только для WebRender-кадра, созданного не раньше корневого display list, чей размер viewport (× hidpi, ±1 px) совпадает с размером surface;
- порядок устаревших кадров обрабатывается через снапшот существующего счётчика `pending_frames`;
- safety-таймаут 250 мс; 6 unit-тестов.

**`backend/crates/alef-runtime/src/host.rs`**:

- `WindowEvent::Resized` синхронно ждёт до 100 мс кадр нового размера (счётчик поколений на Condvar, bump из `Waker::wake` рядом с `EventLoopProxy`; без busy-spin);
- затем paint и present выполняются до выхода из `WM_SIZE`;
- по таймауту paint выполняется всё равно, в stderr пишется лог.

**`backend/crates/alef-runtime/src/resize_wait.rs`**: чистые хелперы ожидания/поколений + trace-флаг; 3 unit-теста (всего 14 runtime-тестов проходят).

Диагностика: переменная окружения `ALEF_RESIZE_TRACE=1` печатает одну строку в stderr на каждый present: путь (`resize` — синхронный present из `WM_SIZE`, `redraw` — обычный), размер surface и native client size, время ожидания, `timed_out`. Размер viewport display list host не видит: его совпадение с surface проверяет патч `servo-paint`, поэтому `timed_out=false` у строки `resize` означает, что представлен кадр с содержимым для нового размера.

### Как проверить

1. Запустить приложение с `ALEF_RESIZE_TRACE=1`.
2. Тянуть края и углы окна. Ожидается: во время перетаскивания не представляется ни один кадр с несовпадающими размерами (каждая trace-строка показывает разрешённое ожидание либо явный timeout), белая область не растёт, содержимое и рамка движутся вместе.
3. Проверить maximize → restore.

**Статус: исправление реализовано, ожидает визуальной проверки пользователем.**
