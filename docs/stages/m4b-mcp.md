# M4b — Серверы и MCP

Обзор: `../FRAMEWORK-PLAN.md`. Зависит от M4 (`http.serve`, `websocket.serve`, консольный режим `app`, `cli`) и M2b (права и подмена).

## Цель

Приложение Alef — в том числе без окна — поднимает серверы (HTTP, WebSocket) и говорит по MCP (Model Context Protocol): публикует свои инструменты агентам и сам подключается к чужим MCP-серверам. Серверы — штатный способ делать «службы» на Alef.

## `@alef-tron/mcp`

JS-пакет поверх модулей, а не модуль runtime: протокол MCP меняется быстро, JSON-RPC тонок, обновлять его проще отдельно от runtime. Runtime даёт транспорт: `http.serve`, `websocket.serve`, `socket`, `cli.spawn`, консольный режим (stdio).

```ts
import { mcp } from '@alef-tron/mcp';

const server = mcp.server({ name: 'notes', version: '1.0.0' })
  .tool('search', { description, inputSchema }, async args => ({ content: [...] }))
  .resource('notes://all', { ... }, async () => ({ contents: [...] }))
  .prompt('summarize', { ... }, async args => ({ messages: [...] }));
await server.listen({ stdio: true });                              // консольный режим: JSON-RPC по stdin/stdout
await server.listen({ http: { port: 0, path: '/mcp' } });          // Streamable HTTP

const client = await mcp.connect({ command: 'npx', args: ['some-server'] });   // stdio через cli.spawn
const web = await mcp.connect({ url: 'https://mcp.example.com/mcp' });         // Streamable HTTP через http.request
await client.listTools(); await client.callTool('search', { q: 'x' });
```

- Версия протокола MCP — по рукопожатию `initialize`; поддерживается актуальная и предыдущая ревизии спецификации, список — в CHANGELOG пакета.
- Валидация входов инструментов по JSON Schema — в пакете, без зависимостей от Node.
- Сервер — ресурс сессии: закрытие документа закрывает сервер; для долгоживущих серверов — безоконный режим или служба.

## Безопасность

Сервер на `127.0.0.1` достижим для любого локального процесса и любой страницы в браузере пользователя. Поэтому по умолчанию:

- привязка **только к loopback**; другой адрес — явное право `listen:<host>:<port>` и отдельная строка в окне согласия (M2b);
- проверка `Host` и `Origin` против DNS-rebinding и CSRF (требование спецификации MCP для HTTP); список допустимых `Origin` задаёт разработчик;
- для HTTP — токен (`Authorization: Bearer`), выдаваемый клиенту при подключении; удалённый доступ и OAuth — отдельным решением, не в этом этапе;
- окно согласия предупреждает: «приложение откроет MCP-сервер, любой локальный процесс сможет вызывать его инструменты»;
- подмена права `listen` (§6.4): `listen()` успешен и выдаёт порт, сокет не открывается; подмена `cli`/`net` для клиента — таймаут.

## Приёмка

| Проверка | Как |
|---|---|
| MCP-сервер на stdio в консольном режиме: `initialize`, `tools/list`, `tools/call` | e2e без окна |
| MCP-сервер на Streamable HTTP: то же; чужой `Origin` и неверный `Host` отклонены; без токена — 401 | e2e |
| Клиент подключается к собственному серверу (stdio через `cli.spawn`, HTTP) | e2e |
| Подмена `listen`: порт выдан, подключение к нему не удаётся | e2e |
| Закрытие документа закрывает сервер и его соединения | e2e |
| Совместимость: референсный клиент MCP (`@modelcontextprotocol/inspector` или SDK) работает с сервером | ручная |

**Как сделано (M4b)**

- `packages/mcp` — отдельный приватный пакет `@alef-tron/mcp` версии 0.1.0, без новых зависимостей и без Node в реализации. Слои: JSON-RPC core, подмножество JSON Schema, fluent-сервер, клиент и адаптеры stdio/HTTP поверх существующих JS-обёрток API; backend и API не менялись. Корневая проверка TypeScript включает пакет; README и CHANGELOG описывают поверхность и границы.
- Рукопожатие `initialize` → `notifications/initialized`; поддерживаются **2025-11-25** и **2025-06-18**, неизвестное предложение получает новейшую ревизию. Обе отвергают batches. Мартовский режим batches оставлен только в общем JSON-RPC core для явно выбранной совместимости и отдельного unit-теста, MCP его не согласует. Core принимает также `id: null` и позиционные параметры-массивы JSON-RPC; MCP требует объект и отвечает `Invalid params`. Уведомления, включая уведомления с неверными параметрами, ответа не получают; методы уведомлений с `id` отвергаются как неверные запросы, без побочного действия. `initialize` не отменяется уведомлением MCP cancellation; локальный таймаут/закрытие транспорта остаются возможны.
- Сервер цепочкой объявляет `tool`, `resource`, `prompt`, затем `listen`/`close`; клиент даёт все шесть операций list/call/read/get и `close`. Ошибки JSON-RPC стандартные; неожиданная ошибка обработчика скрыта за `Internal error`. Контекст обработчика содержит `signal` и `progress`; duplex-транспорт передаёт cancellation/progress. Ожидания ограничены временем; таймаут HTTP dispatch действительно abort-ит контекст и снимает активный запрос, а не только прерывает ожидание ответа. Обработчик должен соблюдать `signal`: JS не может принудительно остановить произвольный пользовательский код. Прогресс после отмены не отправляется; исключение пользовательского callback не ломает соединение. Cleanup чтения тела освобождает lock и не ждёт бесконечно зависшей отмены потока.
- Валидация инструментов: boolean schemas, `type`/union, `properties`, `required`, структурные `enum`/`const`, `items`, `additionalProperties`, числовые границы, длина строк в Unicode code points, `pattern`, `allOf`/`anyOf`/`oneOf`/`not`, локальные `#/$defs` refs с JSON Pointer escapes; глубина ограничена. Это не полный JSON Schema: неизвестные keywords игнорируются, удалённые refs, formats, tuples и прочие расширения не реализованы; схемы и регулярные выражения — доверенная конфигурация приложения.
- Stdio — newline JSON в обычных потоках `{ input, output }` (старые `readable`/`writable` оставлены aliases), с фрагментацией UTF-8, несколькими сообщениями, CRLF и ограничением размера. EOF с незавершённой строкой — ошибка. `listen({ stdio: true })` берёт `app.stdin`/`app.stdout` консольной утилиты (M4.6); `server.closed` исполняется, когда сервер закрыт или кончился вход единственного соединения stdio — утилита выходит вместе с клиентом (`await server.closed; await app.exit(0)`). Клиент `{ command, args }` использует `cli.spawn` без shell, закрывает потоки, убивает и ожидает ребёнка.
- HTTP — JSON-ветка Streamable HTTP: loopback-only listen, POST JSON, 202 для уведомлений, GET 405, DELETE сессии, `Mcp-Session-Id` и `MCP-Protocol-Version`. Обе согласуемые ревизии требуют свой заголовок после initialize: отсутствующий трактуется как March по compatibility rule и потому отвергается. По умолчанию Host/Origin/token включены; `allowedOrigins` передаётся также native guard (`origins` — alias), разрешены same-loopback origins и отсутствие Origin у native-клиента. Токен доступен как `server.token`, клиент передаёт его опцией `token`. Токен и session IDs — 256 бит из Alef `crypto.random`: native Servo показал отсутствие WebCrypto global, поэтому он не используется. Размер тела по умолчанию 1 MiB, сессий 128, время 30 с; неверный JSON, превышение размера, чужие Host/Origin, отсутствие/ошибка токена, неизвестная сессия и неверная ревизия отвергаются. Явные `checkHost: false`, `checkOrigin: false`, `token: false` выключают только JS-проверки, не native guard/consent. Собственные серверные OAuth/TLS/remote bind не добавлены. HTTP listener — ресурс `http.serve`: document teardown закрывает native listener и соединения без новых lifecycle hooks.
- Native e2e `mcp`: **7 проверок раннера + 1 проверка native-клиента**. Plain fetch выполняет initialize/list/call, проверяет ревизии и отказ batches, разрешённый/чужой Origin, 401 без/с неверным токеном, неверный JSON, большое тело, неизвестную сессию и неверный protocol header; неверный Host проверяется raw HTTP, поскольку Node fetch подменяет этот заголовок. Инструмент страницы запускает navigation: новый документ докладывает запуск, старый порт больше не принимает соединения, runtime всё ещё жив — это не проверка одного лишь выхода процесса. E2e transpile перенаправляет imports MCP к общей копии API, чтобы не создавать второй transport singleton. Добавлены отдельные приложение и сценарий, `scenarios/modules.mjs` не менялся. Чтобы сохранить максимум 7 entries в `scenarios`, startup probe перенесён рядом с startup app; read-only сравнение подтвердило byte-identical содержимое исходному файлу.
- Native e2e `mcp-stdio` (консольный режим, без окна): раннер пишет JSON-строки в stdin настоящего процесса и читает stdout — `initialize`, `tools/list`, `tools/call`, отказ по схеме (`-32602`), ответ `-32700` на битую строку; в stdout только протокол; закрытие stdin завершает утилиту с кодом 0 и вердиктом PASS. Без драйвера OpenGL на Windows сценарий пропускается, как `console`.
- Проверки: **51 unit-тест MCP** (48 пакета и 3 на консоль и `closed`), JS-тесты API и frontend, интегрированные `tsc`, `oxlint` и structure (≤7 entries/dir, ≤700 lines/file); мутации пакета: 91 мутация по Host/Origin/токену/сессиям/схеме/фреймингу/ядру/клиенту, одна эквивалентна (id запроса в JSON не бывает NaN). Review-регрессии сначала подтвердили пять дефектов (null/array, ответ уведомлению, request с именем cancellation/progress, отмена initialize, HTTP timeout без abort), затем прошли после исправления.
- **Не сделано / не проверено:** resource subscriptions/templates, sampling, elicitation, tasks, SSE/resumption и промежуточный progress по HTTP; OAuth и remote listen; полноценная JSON Schema; MCP-specific e2e подмены listen/net/cli и отдельное предупреждение consent об MCP. URL-клиент допускает удалённый HTTPS через `http.request`, но лишь в рамках manifest `permissions.net.http`; удалённый HTTPS MCP-сервер в этом прогоне не проверялся, redirects не следуются. Ручная совместимость с Inspector/референсным SDK **не проверена**; успешные unit/native self-tests не объявляются такой проверкой. Поэтому всю приёмку этапа завершённой не считать.

## Риски

- Спецификация MCP меняется: пакет версионируется отдельно, тесты на зафиксированные ревизии.
- Серверная нагрузка идёт через мост Servo (каждый запрос — событие в документ): для API и MCP хватает, высокую нагрузку это не потянет (граница записана в `m4-net-cli.md`).
- Безопасность локальных серверов — главный риск этапа; проверки `Host`/`Origin` и токен включены по умолчанию, отключение — явное и видно в окне согласия.
