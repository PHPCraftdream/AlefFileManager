# Сегментированные сабпути пакета `@alef-runtime/api`

## Карта модулей

| Сабпуть | Файл | Что экспортирует (кратко) |
| --- | --- | --- |
| `.` | `src/index.ts` | всё сразу: публичная поверхность пакета |
| `./types` | `types/index.ts` | сгенерированные DTO-типы (только типы) |
| `./core` | `src/core.ts` | ошибки, handshake, транспорт, события, стримы |
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

Модуль импортирует только из `src/core/*` и своих файлов. Каталог `types/` — только
типы: сгенерированы, кода не тянут.

Исключения, каждое с обоснованием:

- семейство `net`: один транспорт запросов/ответов и сервер обновления на нём.
  - `http` → `http-server` и `body` — клиент и сервер делят разбор тел;
  - `http-server` → `body` и `websocket-connection` — сервер читает тела и отдаёт
    WebSocket-соединения;
  - `websocket` → `http`, `http-server` (типы) и `websocket-connection` — тот же
    транспорт для клиента и сервера;
- `menu` → тип `AppWindow` из `window` — меню прикрепляется к окну.

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
