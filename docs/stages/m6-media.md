# M6 — Медиа

Обзор: `../FRAMEWORK-PLAN.md`. Решение владельца проекта (2026-10-18): отложение снято, выбран **вариант B** — нативный API без GStreamer (критерий 2: размер дистрибутива важнее стандартных web API); решение обратимо. M6.1 — спайк: кадры 1280×720@30 виртуальной камеры через транспорт в `<canvas>` (задержка, потери, CPU), синус микрофона и checksum-приёмник — всё на виртуальных устройствах, чтобы сценарий шёл в CI без железа.

## Сделано в M6.1

- Модуль `media` в alef-modules: трейты `CameraBackend` / `MicrophoneBackend` / `AudioBackend`.
- Бэкенды: `MissingCamera` / `MissingMicrophone` / `MissingAudio` отвечают `NOT_AVAILABLE` в обычной сборке; виртуальные (`VirtualCamera`, `VirtualMicrophone`, `VirtualAudio`) — детерминированные, служат e2e-прогонам и тестам. Камера рисует движущийся тестовый рисунок RGBA 1280×720 с запросным fps (по умолчанию 30, диапазон 1..120); микрофон — фазо-непрерывный синус f32 (по умолчанию 440 Гц, 48000 Гц, чанк 20 мс); приёмник аудио считает сэмплы и считает FNV-1a-64 по байтам.
- Команды: `media.camera.list/open/close`, `media.microphone.list/open/close`, `audio.open/write/close`. Открывание устройства возвращает `{ camera|microphone|sink, stream }` (страница читает поток `openReadable`).
- Права: раздел `permissions.media` манифеста (`camera`, `microphone`, bool; раздел необязателен, по умолчанию закрыт) и `permissions.audio` (bool, по умолчанию false). Подмена — `NOT_AVAILABLE` без открытия устройства; отказ и необъявленное право — `PERMISSION_DENIED`. `media.camera` и `media.microphone` — risky-права («sees through the camera…» / «hears through the microphone…»), показываются в окне согласия и `alef permissions` наравне с остальными.
- JS: субпуть `@alef-runtime/api/media` (`./media` + `./media/types`): объекты `camera` (`list`, `open`), `microphone` (`list`, `open`), `audio` (`open`); классы `Camera`/`Microphone` — async-iterable пар «заголовок+нагрузка» с `close()`, возвращающим статистику; `AudioSink` — `write(Float32Array|Uint8Array)` и `close()` со статистикой и checksum-hex; повторный `close()` отвечает `NOT_FOUND`.
- e2e: сценарий `media` (приложение `tests/e2e/apps/modules/system/media`, режимы allowed / denied / substituted), SOFT-пороги (цель — измерение на медленных раннерах): ≥ 5 fps, p95 задержки < 2000 мс на 1280×720@30; страница печатает строку `media-camera-stats frames= fps= p50_ms= p95_ms= main_ms= total_ms= drops= elapsed_ms=`; вытеснение кадров политикой drop-oldest легально: seq строго растёт, разрывы seq сходятся с дельтой `dropped`.

## Контракт кадров и чанков

Пара кадров потока: сначала JSON-заголовок, затем binary-нагрузка — один или несколько чанков, каждый не больше транспортного чанка (256 КиБ), сумма длин которых равна `byteLength`. Камера: `{"seq","timestamp","width","height","byteLength","dropped","format":"rgba"}` + RGBA `width*height*4` байт построчно. Микрофон: `{"seq","timestamp","rate","samples","byteLength","dropped","format":"f32le"}` + little-endian f32. Короткий чанк — норма, не ошибка; нехватка или лишний байт, или новый заголовок до окончания нагрузки, — внутренняя ошибка; JS-декодер собирает чанки обратно в кадр. Поле `dropped` — сколько элементов очередь вытеснила к моменту создания элемента; продюсер никогда не блокируется (drop-oldest вытесняет только целые кадры). Закрытие: camera → `{"camera","frames","dropped"}`, microphone → `{"microphone","chunks","samples","dropped"}`, audio.close → `{"sink","samples","checksum"}` — checksum строкой-16hex: u64 не переживает JSON-число; page пишет raw LE f32 байты, приёмник fold-ит FNV-1a-64.

## Исполнение (BUS)

Продюсер — допущенное blocking-задание медиа-модуля (бюджет 8; насыщение даёт `BUSY` до старта продюсера): первый кадр сразу, дальше по тикам; остановка прерывает ожидание мгновенно, закрытие ждёт завершения задания. Между продюсером и relay — ограниченная очередь с политикой drop-oldest (камера 3 кадра, микрофон 8 чанков; счётчики pushed/dropped, вытесняются только целые кадры). Relay ждёт кредит вне BUS и пишет JSON-заголовок и binary-чанки попарно. Пути остановки — close ресурса, teardown сессии, разрыв потока; остановка идемпотентна. Закрытие устройства отменяет relay по watch-сигналу и дожидается его: незаконченный кадр отбрасывается, читатель видит `End`.

## Права

```
permissions: {
    audio: true
    media: {
        camera: true
        microphone: true
    }
}
```

Раздел и флаги необязательны, по умолчанию закрыто. Подмена — `NOT_AVAILABLE` без открытия устройства; отказ и необъявленное право — `PERMISSION_DENIED`. `media.camera` и `media.microphone` — risky-права, их описания («sees through the camera…» / «hears through the microphone…») показываются в окне согласия и в `alef permissions` наравне с остальными.

## Измерения спайка (заполняется по CI)

<!-- M6.1-SPIKE-MEASUREMENTS: after the first green `media` scenario in CI, paste the `media-camera-stats` lines here (fps, p50/p95, main_ms) with the runner OS. Until then only the SOFT thresholds hold: >= 5 fps, p95 < 2000 ms. -->

## Дальше (после M6.1)

Реальные бэкенды за cargo feature (`nokhwa` для камеры, `cpal` для аудио/микрофона) — без новых зависимостей в M6.1; системные запросы ОС (macOS TCC: подпись и Info.plist-описания; Windows/Linux — без TCC); повторное измерение спайка на настоящих устройствах.
