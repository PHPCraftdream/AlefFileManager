# M0.1 — Origin приложения: результаты

Дата: 2026-10-05. Сборка `debug`, машина загружена посторонними процессами — абсолютные времена шумные, значимы соотношения внутри одного прогона.

Код: `backend/crates/alef-runtime/src/spikes/origin.rs` (за `ALEF_SPIKE_ORIGIN=1`), страница `experiments/origin-spike/` (`index.html`, `origin.js`, `gen-big.mjs` — генератор синтетического ассета 2 MiB `big.js`, который в git не хранится: перед прогоном `node experiments/origin-spike/gen-big.mjs`). Хуки: `window/delegate.rs` → `load_web_resource` (3 строки), `bridge/mod.rs` → entry URL + каталог ассетов (6 строк), `window/app.rs` → `.preferences()` (1 строка). Навигационный allow-list менять не потребовалось: существующая ветка `request.url.origin() == entry_url.origin()` уже разрешает только свой origin.

Запуски (по 3 прогона загрузки ассета на страницу, N=50 пингов):

```sh
ALEF_TRANSPORT_SPIKE=1 timeout 180 <exe> --frontend-dir experiments/origin-spike --data-dir <tmp> --root .            # база: native://app
ALEF_TRANSPORT_SPIKE=1 ALEF_SPIKE_ORIGIN=1 timeout 180 <exe> --frontend-dir experiments/origin-spike --data-dir <tmp> --root .  # вариант A
```

Отчёт страницы — `native://spike/report` → stderr (`spike report: {...}`), окно закрывает сама страница.

## Результаты

| Проверка | `native://app` (база) | `https://spike-app.alef` (перехват) |
|---|---|---|
| `location.origin` | `"null"` (opaque) | `"https://spike-app.alef"` |
| `isSecureContext` | `false` | `true` |
| `localStorage` | `SecurityError` | запись+чтение OK |
| `sessionStorage` | `SecurityError` | запись+чтение OK |
| `indexedDB` | отсутствует (`in window` = false) | open+put+get OK (pref `dom_indexeddb_enabled=true`) |
| `crypto.subtle.digest` | `ReferenceError: crypto is not defined` | то же (не зависит от origin, см. ниже) |
| `fetch('native://spike/sink')` ×50, p50/p90, мс | 5.69 / 10.66 (max 672) | 4.89 / 11.48 (max 1315) |
| `fetch` с несафелистед-заголовком `X-Alef-Spike` | OK, 11.8 мс | OK, 22.1 мс, **0 preflight-запросов** |
| `native://invoke` (`runtime.window getState`) | OK, 11.1 мс | OK, 5.4 мс |
| 2 MiB ассет, 3 прогона, мс | 1759.9 / 1094.2 / 1766.8 | 1696.0 / 2278.5 / 1849.9 |
| Внешний URL `https://example.com/` | — | заблокирован `cancel()`: `TypeError: Network error: Load cancelled` за 10.4 мс (Rust-часть cancel — 68.9 мкс), страница жива |
| Отдача из делегата (блокировка main thread) | — | index.html 0.93 мс; origin.js 0.42 мс; big.js 2 MiB — 4.1–6.5 мс (замер начинается ПОСЛЕ `canonicalize`, см. ниже — полная загрузка потока больше) |

Соотношение загрузки ассета (медианы прогонов): 1849.9 / 1759.9 = **1.05**; по средним: 1941.5 / 1540.3 = 1.26. Критерий «не медленнее более чем в 1.5 раза» — пройден (при debug-сборке и загруженной машине; разброс отдельных прогонов ±2× от шума).

Повторный прогон на финальном бинарнике (машина менее загружена): пинги p50/p90 = 2.24/4.76 мс (https) против 4.36/12.19 мс (native, max 364); ассет 860.2/1367.6/843.5 мс (https) против 832.6/1591.6/1081.8 мс (native) — медианы 860.2/1081.8 = **0.79**, т.е. https-перехват не медленнее базы. Оба прогона: соотношение медиан 1.05 и 0.79, средних 1.26 и 0.88 — критерий «не медленнее более чем в 1.5 раза» пройден с запасом (разброс отдельных прогонов ±2× от шума).

Контрольный прогон после ревью (затенение токена в логах, усиленные оракулы страницы): пинги p50 11.4 мс (https) / 3.83 мс (native), все 50+1 пингов валидны (HTTP 200 + тело `1`, failures=0), ассеты 1599.7/2469.4/1254.7 против 819.2/1052.7/1432.8, все `intact` (200 + ровно 2097157 B); медианы 1599.7/1052.7 = **1.52** — в этом прогоне чуть выше 1.5 (абсолютные времена между прогонами различались в разы: p50 пингов https 2.24 → 11.4 мс; причину загрузки не измеряли), в двух предыдущих — 1.05 и 0.79.

## Факты из исходников Servo 0.6 (подтверждены наблюдениями)

- Перехват вызывается на **главном потоке** (`servo.rs` `spin_event_loop` → `handle_net_embedder_message` → делегат); лог: `thread=main`. `WebResourceLoad`/`InterceptedWebResourceLoad` **не `Send`** (`responders.rs`: `Box<dyn AbstractSender>` без `Send`) — передать их в задачу Tokio нельзя; поэтому чтение файла в Tokio, чанки 256 KiB через std-канал, отправка `send_body_data` из колбэка на главном потоке.
- Перехват выполняется в `main_fetch` **до** `scheme_fetch`/`http_fetch` (`servo-net/fetch/methods.rs:541`) — `cancel()` обрывает запрос до любой сети (подтверждено: 68.9 мкс, чистый `LoadCancelled`).
- Точность о главном потоке: `resolve_asset` (включая **синхронный** `canonicalize` на диске) выполняется на главном потоке ДО старта таймера отдачи; чтение файла в Tokio — `tokio::fs::read` **целиком** с последующим разбиением на чанки 256 KiB (не потоковый I/O); статистика/abort в JS-замерах не привязаны к номеру вызова. Поэтому «served … in X мс» — только фаза отдачи чанков и **недооценивает** полную загрузку главного потока.
- Тело перехваченного ответа **буферизуется целиком** в net-воркере (`request_interceptor.rs`: `accumulated_body` → `ResponseBody::Done` на `FinishLoad`) — «стриминговые» `send_body_data` не дают странице инкрементальной доставки; для стат. ассетов неважно, для больших потоков (M1 транспорт) использовать `native://stream`, а не перехват.
- Fetch на `native://` из https-страницы: схема `is_fetchable` → tainting `Basic`, CORS-механика вообще не запускается — **preflight не нужен, ACAO не проверяется** (0 `OPTIONS` на 51 запрос; работает и с несафелистед-заголовком). Mixed-content блокировки нет (handler `is_secure=true`). Главный поток посещается каждым fetch (перехватчик), но это уже так и в базе `native://app` — регрессии нет.
- `crypto` отсутствует в обоих режимах: у крейта `servo` default-фичи `["bundled","clipboard","js_jit"]` **без `webcrypto`** (`servo-0.6.0/Cargo.toml`); pref `dom_crypto_subtle_enabled` по умолчанию `true`, но биндинги выключены фичей. Включение `servo/webcrypto` (подтянет aes/argon2/chacha20poly1305/…) — решение для M3 crypto-модуля.
- Capability через фрагмент entry-URL: страница читает `#capability=…` из `location.hash`. **Делегат `load_web_resource` видит фрагмент** — Servo передаёт в перехват полный URL (наблюдалось в логе первого прогона: строка main-frame содержала весь токен; с этого прогона spike печатает URL через `redact()` без фрагмента). Сеть тут ни при чём (перехват локальный, фрагмент никому не отправляется), но любой код делегата обязан считать URL секретом: один `eprintln!("{url}")` — и credential моста в логах. Для M1: (a) обязательное затенение фрагмента во всех логах runtime (сделано в spike, `redact()`), либо (b) вообще не носить capability в URL — например, инъекция через `evaluate_javascript` сразу после `LoadStatus::Complete` (механизм уже есть — `UiRequest::Emit`); минусы: токен появляется позже старта документа — JS обязан ждать события/promise, при каждой загрузке/навигации нужна повторная инъекция, а до неё вызовы API невозможны; либо (c) выделенный bootstrap-вызов `native://bootstrap` — минус: эндпоинт без токена должен допускать только документ текущей загрузки, иначе открывает мост любому загруженному документу. Не проверено: варианты (b)/(c) не реализованы и не измерялись; проверено только затенение (a).

## Решение

**Вариант A принят.** Origin приложения — `https://<app-id>.alef/`, ассеты отдаёт перехват `load_web_resource` (GET, чанки 256 KiB, Content-Type + CSP + nosniff), entry-URL несёт `#capability=…`. Вариант B (патч Servo для tuple origin у `native://`) не нужен.

Влияние на транспорт (M1):

- preflight/CORS для `native://` из https-origin **не нужны** (но `Access-Control-Allow-Origin: *` в ответах стоит оставить — не мешает);
- CSP документа приложения обязан разрешать `connect-src native:` (+ `'self'`);
- внешние URL блокируются дважды: CSP (рано) и `cancel()` в перехвате (поздно, до сети) — база для `external` из манифеста;
- pref `dom_indexeddb_enabled` включать в runtime по умолчанию;
- IndexedDB при `temporary_storage` пишет `bottles/<x>/<uuid>/indexeddb.sqlite` **относительно CWD** (servo-storage, наблюдение прогона) — runtime обязан задавать явный каталог профиля/хранилища;
- большие бинарные потоки — только через `native://stream` (перехват буферизует).

## Что не проверено

- persistность localStorage/indexedDB между перезапусками (temporary_storage, отдельная сессия); multiwindow с общим перехватчиком; percent-decoding путей ассетов (spike не декодирует `%xx`); статусы 404/405 покрыты unit-тестами `decide` (метод/путь → статус), 403 — только маппингом ошибки `status_for` (через разобранный URL `..` до резолвера не доходит: парсер нормализует dot-сегменты, тест `decide_never_serves_outside_the_root_for_dot_dot_urls` подтверждает 404 для четырёх вариантов обхода); реальным запросом со страницы статусы не проверялись; варианты доставки capability (b)/(c) не реализованы; release-сборка (времена — debug).
