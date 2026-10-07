# M4 — Сеть и командная строка

Обзор: `../FRAMEWORK-PLAN.md`. Зависит от M3 (`fs` для сценариев скачивания).

## Цель

Модули `http` (клиент и сервер), `socket`, `websocket` (клиент и сервер), `cli` и безоконные режимы `app` (консоль, служба). Нативная сеть не подчиняется CORS, но ограничена своими scopes в манифесте; `cli` даёт JS доступ к командной строке ОС (JS → Servo → фреймворк → ОС).

## Модули

### `http` (net)

```ts
http.request(url, { method?, headers?, body?: string | Uint8Array | ReadableStream, timeout?, redirect?: 'follow'|'manual', proxy?, signal? })
  : Promise<{ status, headers, url, body: ReadableStream<Uint8Array>, text(), json(), bytes() }>
http.download(url, path, { onProgress? }): Promise<void>     // pipe в fs, scope fs.write
```

- `hyper` клиент + `rustls` (оба в дереве), корневые сертификаты — как у Servo; HTTP/1.1 и HTTP/2.
- Scope: `permissions.net.http` — шаблоны `https://api.example.com/*`, `https://*.example.com/*`.
- Тело ответа — поток транспорта v2 с credit; тело запроса-поток — через входящий поток.
- Cookies: по умолчанию не хранятся; опция `cookieJar: 'session'` — в памяти сессии.

**Как сделано (M4.1)** — `alef-modules/src/net/http/`, `packages/api/src/net/http.ts`:

- Команды `http.request` (малое тело — вместе с вызовом, ≤ 192 KiB), `http.start` + `http.response` (большое тело и `ReadableStream` идут вверх входящим потоком; ответ запрашивается после), `http.download`. Ответ — голова JSON и поток тела с credit; у ответа на `HEAD`, 204, 205, 304 тела нет.
- Редиректы идут вручную (до 10): на каждом шаге адрес снова проверяется по `permissions.net.http`, поэтому сервер не уводит приложение за пределы манифеста. Правила как в Fetch: 303 делает из любого метода кроме GET/HEAD запрос GET, 301 и 302 — из POST; тело и его заголовки уходят вместе с методом; 307/308 и 301/302 с другим методом повторяют тело, если оно в памяти (поток уже ушёл — ответом остаётся сам редирект); при смене origin уходят `Authorization` и `Cookie`.
- Заголовки хоста и обрамления (`host`, `content-length`, `transfer-encoding`, `connection`, `upgrade`, `keep-alive`, `te`, `trailer`, `proxy-*`) страница не ставит — `INVALID_ARGUMENT`; `User-Agent` по умолчанию `Alef/<версия>`, но страница может задать свой. Адрес с логином и паролем отклоняется (для этого есть `Authorization`).
- Новый код ошибки `NETWORK` (502): не удалось соединиться, обрыв соединения, слишком много редиректов, сервер ответил отказом на `download`. Время вышло — `TIMEOUT`; `timeout` покрывает запрос вместе с редиректами до начала ответа.
- Подмена права (`substitute`) — мёртвая сеть: запрос висит до своего `timeout` (30 с, если не задан) и падает с `TIMEOUT`, на сервер ничего не уходит; редирект на подменённый адрес — отказ.
- `download` пишет через общую цель записи `fs` (`fs.write`, подмена `fs.write` — теневая папка) и сам убирает недописанный файл при ошибке, обрыве и отмене.
- Не сделано: `cookieJar` и `proxy` (в API их нет), клиентские сертификаты, HTTP/2 не проверен тестами (нет TLS-сервера в тестах; `https` проверяется только отказом по scope).
- Проверки: модульные тесты правил (`spec.rs`, редиректы в `client.rs`, тела в `body.rs`), 17 тестов через реестр на сервере loopback (какие запросы и с какими заголовками и телами он видел на каждом шаге редиректа, какие соединения оборваны клиентом), 13 тестов JS-обёртки, e2e `http` (6 проверок, 8 MiB вверх и вниз потоком, abort доходит до соединения, скачивание с прогрессом) и `http-substitute`. Мутации: Rust 96 из 96 и JS 49 из 49 пойманы. Они нашли 11 дыр в тестах: Rust — origin по схеме при том же номере порта, прогресс скачивания на каждом куске; JS — abort в `start`, `response` и `download` (в вызове и в потоке прогресса), границы `ok`, флаг `redirected`, кадры не тела в потоке ответа и в скачивании. Разбор списка до прогона убрал мёртвое: снятие `Proxy-Authorization` при смене origin (страница не может его задать, `proxy-*` запрещены) и вторую проверку статуса редиректа; проверки схемы и логина с паролем в `spec.rs` через реестр недостижимы (scope отказывает раньше), поэтому они проверяются модульно. Мутациями не проверено: время ожидания подменённого права по умолчанию (30 с, тест длился бы столько же).

### `socket` (net)

```ts
socket.connect({ host, port, tls?: boolean | { serverName?, ca? } }): Promise<TcpSocket>   // readable, writable, close, localAddress, remoteAddress
socket.listen({ host?, port }): Promise<TcpServer>            // AsyncIterable<TcpSocket>, close
socket.udp({ host?, port? }): Promise<UdpSocket>              // send(data, host, port), AsyncIterable<{ data, host, port }>
```

- Scope `permissions.net.socket`: `tcp:host:port`, `tcp:*.example.com:443`, `udp:0.0.0.0:5353`, `listen:127.0.0.1:*`.
- Все сокеты — ресурсы сессии.

### `websocket` (net)

```ts
websocket.connect(url, { protocols?, headers? }): Promise<WebSocketConnection>  // send(text|bytes), AsyncIterable<message>, close(code?, reason?)
```

`tokio-tungstenite` + `rustls`; scope — `permissions.net.http` (ws/wss по хосту).

### Серверы: `http.serve` и `websocket.serve` (net)

```ts
const server = await http.serve({ host?: '127.0.0.1', port: 0, tls?, files?: '$APP/public' });  // port 0 — порт выберет ОС
server.address                                              // { host, port }
for await (const req of server) {                           // { method, url, headers, body: ReadableStream, upgrade(), respond({ status, headers, body }) }
  await req.respond({ status: 200, headers: { 'content-type': 'text/plain' }, body: 'hi' });
}
const sockets = websocket.serve({ host?, port, path?, protocols?, origins? });   // AsyncIterable<WebSocketConnection>
const connection = await req.upgrade();                      // WebSocket на том же порту, что и HTTP
```

- Разбор HTTP, TLS и сокеты — в Rust (`hyper`, `rustls`, `tokio-tungstenite`), обработчик — в JS; запросы идут потоком в документ с credit (транспорт v2 дополняется потоком на сервер, а не только одним потоком событий на документ). Сервер — ресурс сессии: закрытие документа закрывает сервер и все его соединения.
- Режим `files`: статика отдаётся из каталога приложения без захода в JS.
- Scope — `listen:host:port` в `permissions.net.socket` (общий для `socket.listen`, `http.serve`, `websocket.serve` и MCP); привязка по умолчанию **только к loopback**, другой адрес — явное право и отдельная строка в окне согласия (M2b). Для HTTP-серверов проверяются `Host` и `Origin` (против DNS-rebinding и CSRF), список допустимых `Origin` задаёт разработчик.
- Подмена права `listen` (§6.4): вызов успешен и выдаёт порт, но сокет не открывается и входящих нет.
- Граница: каждый запрос — событие через мост Servo; для API, WebSocket-каналов, MCP и локальных инструментов этого достаточно, высокой нагрузки (тысячи запросов в секунду) это не потянет — для неё нужен Rust-хост.

### `cli` (system)

```ts
cli.exec(commandLine, { shell?: boolean | string, cwd?, env?, timeout?, input?, signal? })
  : Promise<{ code: number | null, signal?: string, stdout: string, stderr: string }>
cli.spawn(program, args, { cwd?, env?, stdin?: 'pipe'|'ignore', stdout?, stderr? })
  : Promise<ChildProcess>   // pid, stdin: WritableStream, stdout/stderr: ReadableStream, wait(): Promise<{ code, signal }>, kill(signal?)
cli.pty(program, args, { cols, rows, cwd?, env? }): Promise<Pty>   // readable, writable, resize(cols, rows), kill, wait
```

- Право `permissions.cli.exec`: список программ (имя или абсолютный путь); `*` — любые, только явно. Для `exec` через оболочку проверяется сама оболочка и первая программа командной строки; при `*` — без ограничений.
- Объявленные команды (§6.4, M2b): приложение заранее перечисляет фиксированные команды в `permissions.cli.commands` (`name`, `program`, шаблон `args`, `description`); пользователь по каждой разрешает, подменяет (команда зависает до таймаута) или отказывает; вызов вне списка — `PERMISSION_DENIED`. `permissions.cli.exec` (список программ, `*`) остаётся для приложений, которым нужны произвольные команды, и подтверждается отдельно, с предупреждением, что оболочка раскрывает всё.
- Оболочка по умолчанию: Windows — `cmd.exe /C` (опция `powershell`), Unix — `/bin/sh -c`.
- `spawn` без оболочки — аргументы без интерпретации.
- PTY — `portable-pty` (ConPTY на Windows).
- Процессы — ресурсы сессии: при закрытии сессии дерево процессов завершается (Windows — Job Object; Unix — process group).
- Sidecar: программы из каталога приложения (`$APP/bin/<name>`) — scope `sidecar:<name>`.

### Консольный режим `app`

```ts
app.stdin: ReadableStream<Uint8Array>, app.stdout / app.stderr: WritableStream<Uint8Array>
app.exit(code): Promise<never>
```

- Режимы: **консоль** — `windows: []` + `console: true` (stdin/stdout, код выхода); **служба** — `windows: []` без консоли (долгоживущие серверы, фоновая работа; завершение по SIGTERM/Ctrl+Break/`CTRL_CLOSE_EVENT` → событие `before-quit` → выход). Runtime не создаёт окно; документ приложения выполняется в скрытом `WebView` на программном контексте отрисовки (спайк M0.5 подтверждает, что это работает без дисплея; запасной путь — невидимое окно 1×1). Модули `window`, `screen`, `dialog` в этих режимах отвечают `NOT_AVAILABLE`.
- Регистрация службы ОС (Windows Service, systemd unit, launchd) — установка и автозапуск (M5, M7b); runtime в режиме службы не требует консоли.
- Windows: бинарник GUI-subsystem → `AttachConsole(ATTACH_PARENT_PROCESS)`, перенаправление stdio; при запуске двойным кликом консоли нет — stdout в никуда, это документировать.
- Код выхода процесса = `app.exit(code)`.

## Структура кода

```
alef-modules/src/net/      mod.rs, http/ (mod.rs, client.rs, body.rs, spec.rs), socket/ (mod.rs, tcp.rs, udp.rs, tls.rs), websocket.rs
alef-modules/src/system/   cli/ (mod.rs, exec.rs, spawn.rs, pty.rs, tree.rs)
packages/api/src/net/      http.ts, socket.ts, websocket.ts
packages/api/src/system/   cli.ts
```

## Приёмка

| Проверка | Как |
|---|---|
| `http.download` 500 MiB в `fs` с прогрессом, память стабильна, abort останавливает загрузку | e2e (локальный тестовый сервер на loopback) |
| URL вне `net.http` → `PERMISSION_DENIED` | e2e |
| TCP эхо (connect/listen), UDP эхо, TLS к тестовому серверу | e2e |
| WebSocket эхо | e2e |
| `cli.exec('git --version')` с правом → код 0 и stdout; без права → отказ | e2e |
| `cli.spawn` с потоковым stdout, запись в stdin, `kill`; reload → процесс убит | e2e |
| `cli.pty` — интерактивная оболочка, `resize` | e2e (полуручной) |
| Консольная утилита: `echo data | app` → обработанный stdout, код выхода, без окна | интеграционный тест на трёх ОС |

## Риски

- Безоконные режимы без дисплея (Linux-сервер без X/Wayland, служба Windows в session 0) — результат спайка M0.5; этап M4 не начинается до его итога.
- Завершение дерева процессов кроссплатформенно — Job Object / process group, тесты на каждой ОС.
