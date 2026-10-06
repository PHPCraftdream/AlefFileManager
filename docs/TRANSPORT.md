# Alef: транспорт v2 (дизайн)

Задача: один in-process транспорт между JS страницы и runtime для всех API — unary-вызовы, бинарные данные, потоки в обе стороны, события, дескрипторы ресурсов. Без TCP-портов, pipe и других OS endpoint (как и сейчас).

## Что установлено в исходниках Servo 0.6

- `ProtocolHandler::load` получает `&mut DoneChannel` (`servo-net-0.6.0/fetch/methods.rs:177`, `= Option<(TokioSender<Data>, TokioReceiver<Data>)>`). Если handler выставит канал и `ResponseBody::Receiving`, `main_fetch` → `wait_for_response` (`methods.rs:894`) передаёт каждый `Data::Payload` в `target.process_response_chunk`.
- В script это `Response::stream_chunk` (`servo-script-0.6.0/fetch/fetch.rs:749-751`) — чанки попадают в `response.body` (ReadableStream) по мере поступления. Значит, **потоковый ответ native → JS возможен через обычный `fetch`**.
- Ограничение: если отправитель канала уничтожен без `Data::Done`/`Error`/`Cancelled`, fetch-воркер паникует (`methods.rs:931-932`). Runtime обязан всегда завершать поток явно (guard с `Drop`).
- `Data` не реэкспортируется из `servo::protocol_handler` — нужна прямая зависимость `servo-net = "=0.6.0"` (та же версия, что уже в `Cargo.lock`).
- Тело запроса уже читается чанками (`BodyChunkRequest`, `bridge.rs`) — бинарные тела JS → native работают; потоковые тела запроса (`ReadableStream` + `duplex`) в Servo не рассчитываем.

## Каналы

### 1. Unary-вызов

`POST native://invoke/<command>`

- аргументы: `application/json`, либо бинарное тело `application/octet-stream` + JSON-аргументы в заголовке `X-Alef-Args`;
- ответ: JSON или `application/octet-stream` (`ArrayBuffer` в JS);
- лимит тела — на команду (например, `fs.write` — мегабайты, остальные — 256 KiB);
- ошибки: HTTP-статус + `{ code, message }`, `code` из единого списка (`ENOENT`, `EACCES`, `PERMISSION_DENIED`, `TIMEOUT`, `CLOSED`, …).

### 2. Поток native → JS

`GET native://stream/<id>` — долгоживущий ответ через `DoneChannel`. Тело — последовательность кадров:

```
[kind: u8][length: u32 LE][payload: length bytes]
kind: 1 = json, 2 = binary, 3 = end, 4 = error(json)
```

`@alef-tron/api` разбирает кадры и отдаёт `AsyncIterable` / `ReadableStream`.

Используется для: входящих данных сокета, чтения файла потоком, `fs.watch`, тела `http`-ответа, stdout/stderr процесса, кадров камеры, аудио, и **общего канала событий** документа (заменяет нынешний `evaluate_javascript` с JSON).

**Flow control.** Servo кладёт чанки в ReadableStream без учёта скорости чтения, поэтому backpressure делается явно, кредитами: runtime отправляет не больше окна (например, 1 MiB) неподтверждённых байт; `@alef-tron/api` после чтения отправляет `stream.ack(id, bytes)`. Источник (сокет, файл, камера) приостанавливается, когда окно исчерпано.

### 3. Поток JS → native

Последовательность unary-вызовов `stream.write(id, chunk)` с бинарным телом; каждый ждёт подтверждения — backpressure естественная. `stream.close(id)` / `stream.abort(id, reason)`.

## Дескрипторы ресурсов

- Таблица ресурсов в runtime: файлы, сокеты, БД, процессы, потоки, медиа-устройства. В JS — непрозрачный id.
- Владелец — документ (окно + загрузка). При перезагрузке, навигации или закрытии окна все ресурсы документа закрываются. Для этого capability-токен выдаётся на документ и перевыпускается при каждой загрузке (сейчас токен один на процесс и переживает reload).
- Явное закрытие: `close()`; в `@alef-tron/api` — также `Symbol.asyncDispose` (`await using`).

## Права

Каждая команда объявляет требуемое право (`fs.read`, `net.connect`, `process.spawn`, …) и scope (путь, хост:порт, имя программы). Проверка — в runtime до выполнения; манифест задаёт разрешённое. Камера и микрофон — дополнительно системный запрос пользователю.

## Мультиоконность

Токен и таблица ресурсов на окно/документ. События: адресно (в окно) или `broadcast`. Окна общаются через runtime (`app.emit` / `window.emit`), без прямого доступа друг к другу.

## Результаты spike (2026-10-05)

Код: `backend/crates/alef-runtime/src/bridge/spike.rs` (маршруты `native://spike/*`, только при `ALEF_TRANSPORT_SPIKE=1`), страница `experiments/transport-spike/`. Запуск:

```
ALEF_TRANSPORT_SPIKE=1 backend/target/release/alef-file-manager.exe --frontend-dir experiments/transport-spike
```

Release-сборка, машина сильно загружена посторонними процессами — абсолютные MB/s не показательны, значимы соотношения в одном прогоне.

| Проверка | Результат |
|---|---|
| Потоковый ответ native → JS | Работает: 20 чанков с интервалом 100 мс → 20 отдельных `read()`, первый через 116 мс. Задержка отправка→JS: p50 0.9 мс, p90 3.4 мс, max 16 мс |
| Пропускная способность потока (чанки 256 KiB) | 286 MB/s |
| Ответ одним телом (`ResponseBody::Done`) | 3.4 MB/s — **в ~80 раз медленнее** потока в том же прогоне |
| Тело запроса JS → native (`ArrayBuffer`) | 235 MB/s |
| Буферизация без чтения | Runtime отправил 64 MiB за 0.7 мс, пока JS не читал; всё легло в буфер Servo — backpressure отсутствует |
| `AbortController.abort()` в JS | JS получает `AbortError`, но runtime **не узнаёт**: отправка продолжилась до конца, канал не закрылся |
| Перезагрузка страницы во время потока | Runtime **не узнаёт**; канал закрылся только при закрытии окна |
| Хранилища страницы | `origin` = `"null"` (opaque): `localStorage`/`sessionStorage` → `SecurityError`, `indexedDB` отсутствует |

## Выводы для реализации

1. Потоки native → JS делаются через `DoneChannel` — подтверждено.
2. **Любые большие ответы — только чанками** (≈256 KiB), никогда `ResponseBody::Done(vec)`; `Done` — лишь для маленьких JSON-ответов.
3. **Кредитный flow control обязателен** — Servo буферизует без ограничений.
4. **Жизненный цикл потоков ведёт runtime сам**: Servo не сообщает ни об abort, ни о reload. Нужно:
   - `@alef-tron/api` вешает на `AbortSignal` явный `stream.close(id)`;
   - токен и таблица ресурсов — на загрузку документа; при новой загрузке/навигации (`notify_load_status_changed`, смена URL) runtime закрывает все ресурсы прежнего документа;
   - guard: каждый поток завершается `Data::Done`/`Cancelled` (иначе паника Servo).
5. Upload (JS → native) бинарными телами достаточно быстрый — `stream.write(id, chunk)` через unary-вызовы годится.
6. **Web storage недоступен** из-за opaque origin `native://`. Варианты: собственный `store`/`sqlite` API (уже в плане) и/или патч Servo, дающий `native://app` tuple origin, чтобы заработали `localStorage`/IndexedDB. Решить отдельно.

Дальше: реализация транспорта + каркас `@alef-tron/api`, затем перевод `runtime.window` и событий на новый транспорт, затем API по фазам из `API-ROADMAP.md`. Spike-маршруты остаются как стенд для регрессионных замеров до готовности транспорта.
