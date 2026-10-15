# Сегментированные сабпути пакета `@alef-runtime/api`

## Карта модулей

| Сабпуть | Файл | Что экспортирует (кратко) |
| --- | --- | --- |
| `.` | `src/index.ts` | всё сразу: публичная поверхность пакета |
| `./types` | `types/index.ts` | сгенерированные DTO-типы (только типы) |
| `./core` | `src/core/index.ts` | ошибки, handshake, транспорт, события, стримы |
| `./ai` | `src/system/ai.ts` | `ai`: роли Rush, чат-поток и обработчики инструментов |
| `./app` | `src/desktop/app.ts` | `app`: жизненный цикл приложения, события |
| `./window` | `src/desktop/window.ts` | `window`, `nativeWindow`, `screen`, `AppWindow` |
| `./dialog` | `src/desktop/dialog.ts` | `dialog`: диалоги открытия/сохранения |
| `./shell` | `src/desktop/shell.ts` | `shell`: открыть, показать в папке, иконки файлов |
| `./menu` | `src/desktop/menu.ts` | `menu`: меню приложения |
| `./tray` | `src/desktop/tray.ts` | `tray`, `Tray`: иконка в трее |
| `./shortcut` | `src/desktop/shortcut.ts` | `shortcut`, `Shortcut`: глобальные горячие клавиши |
| `./fs` | `src/data/fs.ts` | `fs`, `FileHandle`: файлы и каталоги |
| `./store` | `src/data/store.ts` | `store`, `Store`: key-value хранилище |
| `./sqlite` | `src/data/sqlite.ts` | `sqlite`, `SqliteDatabase` и связанные классы |
| `./crypto` | `src/data/crypto.ts` | `crypto`: хэши, шифры, подписи, KDF |
| `./secrets` | `src/data/secrets.ts` | `secrets`: хранилище секретов |
| `./http` | `src/net/http.ts` | `http`, `HttpResponse`: HTTP-клиент |
| `./http-server` | `src/net/http-server.ts` | `HttpServer`, `ServerRequest`: HTTP-сервер |
| `./socket` | `src/net/socket.ts` | `socket`, TCP/UDP-классы |
| `./websocket` | `src/net/websocket.ts` | `websocket`, `WebSocketServer`, `WebSocketConnection` |
| `./updater` | `src/net/updater.ts` | `updater`: обновление приложения |
| `./cli` | `src/system/cli.ts` | `cli`, `ChildProcess`, `Pty`: процессы и терминал |
| `./clipboard` | `src/system/clipboard.ts` | `clipboard`: буфер обмена |
| `./notification` | `src/system/notification.ts` | `notification`: уведомления |
| `./os` | `src/system/os.ts` | `os`: информация о системе, события темы |
| `./path` | `src/system/path.ts` | `path`: пути (данные, конфиг, кэш) |

## Правило зависимостей

Модуль импортирует только из `src/core/*` и своих файлов (каталог `src/core/` —
один модуль). Каталог `types/` — только типы: сгенерированы, кода не тянут.
Каталоги `data`, `net` и `system` семьями не считаются: между разными модулями
(даже в одном каталоге) разрешены ровно такие рёбра, каждое с обоснованием:

- `http` → `http-server` и `body` — клиент и сервер делят один транспорт и разбор тел;
- `http-server` → `body` и `websocket-connection` — сервер читает тела и отдаёт
  WebSocket-соединения;
- `websocket` → `http`, `http-server` и `websocket-connection` — клиент и сервер
  WebSocket строятся на HTTP-транспорте;
- `menu` → `window` — меню прикрепляется к окну; это ребро разрешено только
  как `import type`.

Типы `Cancelable` и `Unlisten` живут в `core/events.ts`; `app` и `window`
реэкспортируют их для совместимости.

## Без побочных эффектов при импорте

Ни один модуль `src` ничего не делает при импорте: нет подписок, слушателей,
обращений к рантайму. Всё происходит при первом вызове.

Корневой `.` и любой реэкспорт всего тянут весь код пакета — для размера бандла
импортируйте по сабпутям.

## Импорт только типов

Пишите `import type { ... }` (или инлайн-модификатор `type` в обычном импорте):
пакет проверяется с `verbatimModuleSyntax`, и импорт типа не тянет код.

```ts
import { fs } from '@alef-runtime/api/fs';
import type { DirEntry } from '@alef-runtime/api/types';
```

Сабпуть `./types` — только для проверки типов: его резолвят `tsc` и бандлер
(внутри — `./core.js`). Под Node (`--experimental-strip-types`) в рантайме
этот сабпуть не импортировать — тесты прогоняют его только статически.
