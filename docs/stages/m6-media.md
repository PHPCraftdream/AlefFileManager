# M6 — Медиа (отложено, вне MVP)

Обзор: `../FRAMEWORK-PLAN.md`. Решение владельца проекта (2026-10-05): этап отложен.

## Текущее состояние

Сборка Servo использует `servo-media-dummy`: в документах не работают `getUserMedia` (камера, микрофон), Web Audio (`AudioContext`), `<audio>`/`<video>`.

## Варианты на момент возврата к этапу

| | A. GStreamer-backend Servo | B. Нативный API |
|---|---|---|
| Что работает | стандартные `getUserMedia`, Web Audio, `<audio>`/`<video>` | `@alef-tron/api` `camera`, `microphone`, `audio` |
| Реализация | включить media-backend Servo с GStreamer | `nokhwa` (камера), `cpal` (аудио), кадры/сэмплы через транспорт v2, вывод в `<canvas>`/AudioWorklet |
| Размер | +100 MB и более (GStreamer runtime на Windows/macOS) | небольшой |
| Совместимость | стандартный веб-код и библиотеки | нестандартный API |
| Риски | сборка/поставка GStreamer на трёх ОС | производительность передачи кадров, синхронизация аудио |

## Критерии выбора

1. Нужны ли приложениям стандартные web API и готовые библиотеки (WebRTC, видеоплееры) → A.
2. Важнее размер дистрибутива и достаточно захвата/воспроизведения → B.
3. Проверка: spike B — кадры 1280×720@30 через транспорт в `<canvas>` (JPEG/raw), задержка и загрузка CPU.

## Права

`permissions.media.camera`, `permissions.media.microphone` (bool) + системный запрос пользователю (macOS TCC — обязателен, требует подписи и `Info.plist`-описаний).
