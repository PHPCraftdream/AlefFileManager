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

У каждого модульного сабпута есть парный сабпут `<модуль>/types`
(например `./fs/types`). Его файл лежит в `packages/api/type-entries/**`
и содержит единственный `export type * from ...`.

## Правило зависимостей

Модуль импортирует только из `src/core/*` и своих файлов (каталог `src/core/` —
один модуль). Каталог `types/` — только типы: сгенерированы, кода не тянут.
Каталоги `data`, `net` и `system` семьями не считаются: между разными модулями
(даже в одном каталоге) разрешены только рёбра из списка, который живёт в тесте.

Единственный источник правды о разрешённых рёбрах — тест
`packages/api/test/modules/segments.test.mjs`: константа `EDGES`, каждое ребро
с комментарием-обоснованием. Этот документ список рёбер не дублирует. Новое
межмодульное ребро роняет гард тестом, который называет файл и само ребро.

Типы `Cancelable` и `Unlisten` живут в `core/events.ts`; `app` и `window`
реэкспортируют их для совместимости.

## Без побочных эффектов при импорте

Ни один модуль `src` ничего не делает при импорте: нет подписок, слушателей,
обращений к рантайму. Всё происходит при первом вызове. Модульный вызов
на верхнем уровне файла (не объявление) роняет гард тестом, который называет
файл и строку.

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

Для типов конкретного модуля используйте сабпут `<модуль>/types`:

```ts
import type { FileOpenOptions } from '@alef-runtime/api/fs/types';
```

Такой сабпут содержит только типы и при `--experimental-strip-types`
стирается целиком: его импорт не загружает больше ничего из пакета.
Резолвинг в `tsc` и бандлерах при этом продолжает работать.

## Запуск в CI

Гард тесты сегментации запускаются при изменениях в `packages/api/**`,
`docs/api/**` или `scripts/ci/**`. Перечислять рёбра здесь нельзя —
см. тест `packages/api/test/modules/segments.test.mjs`.
