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

## Риски

- Спецификация MCP меняется: пакет версионируется отдельно, тесты на зафиксированные ревизии.
- Серверная нагрузка идёт через мост Servo (каждый запрос — событие в документ): для API и MCP хватает, высокую нагрузку это не потянет (граница записана в `m4-net-cli.md`).
- Безопасность локальных серверов — главный риск этапа; проверки `Host`/`Origin` и токен включены по умолчанию, отключение — явное и видно в окне согласия.
