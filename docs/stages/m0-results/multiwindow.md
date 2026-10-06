# M0.2 — Мультиоконность: результаты

Spike: `backend/crates/alef-runtime/src/spikes/multiwindow/` (+ минимальные хуки в `window/app.rs`, `window/mod.rs`), страница `experiments/multiwindow-spike/`. Включение: `ALEF_SPIKE_MULTIWINDOW=1` (+ `ALEF_RESIZE_TRACE=1` для трейсов, `ALEF_TRANSPORT_SPIKE=1` для отчётов страницы). Бюджеты: загрузка второго окна — 140 с; подтверждение resize — 2000 мс (обычный режим), 1 мс (`ALEF_SPIKE_MW_STRICT=1`); ожидание resize-кадра — 100 мс. Запуск:

```
ALEF_SPIKE_MULTIWINDOW=1 ALEF_TRANSPORT_SPIKE=1 ALEF_RESIZE_TRACE=1 \
  backend/target/debug/alef-file-manager.exe --frontend-dir experiments/multiwindow-spike
```

## Что сделано

- Второе окно winit (без декораций, resizable) со своим `WindowRenderingContext`, своим `WebView` (`WebViewBuilder::new(&servo, rendering)`, тот же экземпляр `Servo`) и своим делегатом; создаётся после загрузки первой страницы.
- События маршрутизируются по `WindowId`: события второго окна обрабатывает spike (RedrawRequested, Resized с синхронным ожиданием кадра, ScaleFactorChanged, ввод, CloseRequested — потребляется), первое окно идёт прежним путём без изменений.
- Автосценарий без физического ввода: 30 чередующихся программных `request_inner_size` (по 15 на окно, логические размеры 880–1300 × 640–810), закрытие второго окна (drop webview → rendering → window), 5 resize первого после этого, затем выход из цикла. Все фазы/шаги логируются (`MW ...`); ожидание кадра — существующий путь `wait_for_resize_frame` (лимит 100 мс).
- Пейсинг: spike устанавливает `ControlFlow::WaitUntil` только раньше текущего дедлайна (минимум); каждая незавершённая фаза обязана сама разбудить цикл — иначе `ControlFlow::Wait` засыпает навсегда. Два таких зависания найдены измерением и исправлены: пауза между шагами resize (тишина Servo → нет пробуждений) и фазы ClosePrimary/Fail.

## Результаты (debug-сборка; машина загружена посторонними процессами — значимы counts и отношения, не абсолюты)

### Автоматическая оценка (oracle)

Результаты этого запуска worktree приведены ниже; никаких прежних результатов сюда не переносили. Для всех прогонов применялся `backend/target/debug/alef-file-manager.exe`. Скрипт `run.sh` использует `timeout 180`; здесь `timeout` разрешил все обычные сценарии по их штатному завершению. Фактические длительности сценариев — из `MW summary elapsed`; у control — его 25-секундный предел.

| Прогон | Режим | Exit code | failed | requested / confirmed | elapsed |
|---|---|---:|---|---:|---:|
| normal1 | oracle | 0 | false | 35 / 35 | 15.024 с |
| normal2 | oracle | 0 | false | 35 / 35 | 15.355 с |
| normal3 | oracle | 0 | false | 35 / 35 | 15.304 с |
| strict | `ALEF_SPIKE_MW_STRICT=1` | 1 | true | 35 / 0 | 13.774 с |
| control | без `ALEF_SPIKE_MULTIWINDOW`, напрямую | 124 (timeout) | n/a (summary отсутствует) | n/a | 25 с |

У всех трёх обычных прогонов все семь полей `mw-summary` были true. Strict завершился с `resizes_confirmed=false`, `timeouts_within_tolerance=false`, а в summary `clean_exit=true`; весь процесс завершился с кодом 1, как ожидалось. Control был запущен без multiwindow-флага и не выдал ни строк `MW`, ни `mw-summary`; он сообщил `mw-loaded`, без crash-строки, но не завершился в пределах 25 с (runner `timeout` вернул 124). Таким образом, обычное однооконное поведение наблюдалось, но чистый выход за отведённое время не подтверждён. В первоначальной попытке control без `ALEF_TRANSPORT_SPIKE=1` лог содержал HTTP 404 отчёта; повторный запуск с транспортным флагом исключил эту помеху.

| Проверка | Результат |
|---|---|
| Oracle работает (обнаруживает SUCCESS) | Да: все 3 сегодняшних нормальных прогона PASS, 0 таймаутов, confirmed=35 |
| Oracle работает (обнаруживает FAIL) | Да: сегодняшний strict-mode завершился с exit 1, 35 таймаутов, confirmed=0 |
| Оба окна рисуют (в успешном прогоне) | Да: отчёты `mw-loaded` для обоих окон; второе окно имело 37–38 presents |
| 35 resize, все подтверждены | Да: во всех 3 обычных прогонах `requested=35 confirmed=35 timeouts=0` |
| Закрытие второго, первое живо | Да: `w2_closed_w1_alive=true` во всех 3 обычных summary |
| Закрытие первого при открытом втором | Историческое наблюдение, не перепроверялось в этой серии |
| Паники/GL-ошибки | В пяти сегодняшних логах нет строки `content crashed` |
| Контроль: spike выключен | Нет строк `MW`; `mw-loaded` получен; код 124 по пределу 25 с, чистый выход не подтверждён |
| Compile, test, clippy | build exit 0; root package: 3 теста; `node scripts/cargo.mjs test --locked --workspace` → 3 + 22 passed, 0 failed (22 в runtime, включая 8 юнит-тестов спайка), exit 0; clippy с `-D warnings` exit 0; fmt `--check` exit 0 |

## Per-painter resize-wait (шаг 4)

- Код (servo-paint patch): `resize_wait: RefCell<Option<ResizeWait>>` — поле `Painter`; `Paint::resize_rendering_context(webview_id, size)` → `painter_mut(webview_id.into())` — ровно один painter; `WebViewId(PainterId, BrowsingContextId)` (servo-base 0.6.0) вшивает id painter'а в webview. Painter один на rendering context (`register_rendering_context` дедуплицирует по `Rc::ptr_eq`); удаление последнего webview painter'а удаляет painter и глушит его WebRender (`remove_webview` → `remove_painter`).
- Поведение: чередующиеся resize двух окон ждут кадр независимо (6–17 мс, `timed_out=false`); общий на процесс wait давал бы перекрёстные таймауты — не наблюдались.

## Вывод

**Автоматический критерий спайка M0.2 пройден в сегодняшней серии**: три успешных обычных oracle-прогона подряд, strict-mode FAIL детектирован ожидаемо. Контроль без флага не показал MW-сценарий, но завершение за 25 с не подтверждено. Два окна, два webview, один `Servo`; синхронный resize-wait действует на каждое окно отдельно.

**Визуальный/физический критерий спайка M0.2 НЕ ПРОВЕРЕН** (требует manual user interaction): отсутствие белых полос при drag-resize обоих окон пользователем, IME и анимации во втором окне, WebGL в двух окнах. Эти проверки — разовый manual walk-through перед одобрением M0.2 в CHECKPOINT.

## Дизайн для M2.2 `window.create`

1. Реестр окон: `WindowId → WindowSlot { webview, rendering, window, флаги делегата }`; нынешний `State` первого окна — слот 0. Маршрутизация всех `window_event` по `WindowId` должна стать основным путём (сейчас это хук spike в `app.rs`).
2. `window.create(opts)` — UiRequest → создание окна+контекста+webview в главном потоке (как `Secondary::create`), ответ — id окна.
3. Закрытие: drop webview → rendering → window (порядок важен); `CloseRequested` — событие в документ с возможностью отмены; закрытие последнего окна завершает процесс.
4. Пробуждение цикла: минимизировать дедлайны всех окон (анимации, шаги сценариев); правило «каждый активный источник обязан выставить WaitUntil-минимум» — иначе цикл со статичными страницами засыпает навсегда (урок двух зависаний spike).
5. Бюджет памяти ~150–250 МБ на окно; WebRender-кэши растут при resize (пик +60…+120 МБ на 30 шагов) — при многих окнах ограничивать.
6. Хуки spike в `app.rs`/`mod.rs` (5 блоков BEGIN/END, ~35 строк) удалить при переходе на реальный реестр окон.

## Не проверено (manual-only)

Физический drag-resize пользователя обоих окон одновременно (white-strip критерий); анимации (`requestAnimationFrame`) во втором окне (пейсинг реализован, не упражнялся); IME во втором окне (в spike опущено); WebGL в двух окнах; >2 окон; macOS/Linux; release-mode timings.

## Примечания по измерениям

- В этой серии использовался `backend/target/debug/alef-file-manager.exe`: независимый четвёртый прогон прошёл (`mw oracle: PASS`, exit 0, все 7 проверок true). Каталог per-slug `backend/target/spike-bins/<slug>/...` в этом worktree не создавался.
- В сегодняшней серии второй странице потребовалось менее секунды после создания окна; применяемый кодом дедлайн — 140 с (не смешивать с глобальным watchdog 150 с).
- Oracle exit code non-zero только если `failed=true` либо crash/panic обнаружены; spike emit'ит процессу non-zero через хук `window/mod.rs:73–77` (`run()` возвращает Err).
- Strict-mode FAIL тест (1 мс бюджет) доказывает, что oracle обнаруживает вневремя resize-ответы.

## Известная нестабильность оракула

На перенесённых crates (M1.1) один из ~15 прогонов `run.sh` завершился с кодом 1 сразу после пересборки и интеграционного прогона; лог этого прогона утерян (очистка scratch), какая именно проверка не прошла — неизвестно. Повторы не воспроизвели: 1 + 8 прогонов подряд и 4 пары «интеграционный прогон → multiwindow» — все PASS. Гипотеза (не проверена): холодный старт после пересборки (бюджет подтверждения resize 100 мс на первом шаге; ранее наблюдался один промах 112.8 мс). Раннер теперь при провале печатает сводку `mw-summary` и хвост лога — при следующем сбое причина будет в выводе.
