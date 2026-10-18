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
| `./media` | `src/system/media.ts` | `camera`, `microphone`, `audio` и классы `Camera`, `Microphone`, `AudioSink`: кадры камеры в `<canvas>`, f32-чанки микрофона, checksum-приёмник |

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

## Сборка и потребление (API-4)

`npm run build` в `packages/api` собирает пакет в `dist/`: ESM `.js` и `.d.ts` для каждого
сабпути. Источник каждого сабпути называет поле манифеста `alef-sources` (сабпуть → файл),
карта `exports` держит только условия `types`, `import` и `default` — и все они указывают
в `dist/`. Скрипт `build.mjs` компилирует источники из `alef-sources` двумя запусками tsc
(флаги те же, что в трансгиляции e2e, `tests/e2e/lib.mjs`): рантайм-сабпути дают `.js`
(относительные `.ts`-спецификаторы переписываются в `.js`), все сабпути дают `.d.ts`.
Структура `dist/` повторяет репозиторий: `dist/src/**`, `dist/types/**`,
`dist/type-entries/**`. Сборка чистит `dist/` и не читает корневой tsconfig. Скрипт
`prepack` запускает ту же сборку перед `npm pack` и `npm publish` — тарбол без `dist/`
собрать нельзя.

`alef-sources` — внутреннее поле монорепы: npm игнорирует незнакомые поля манифеста,
в резолвинг оно не участвует, а в тарбол исходники не входят. Зато всё, что рекламирует
карта `exports` установленного пакета, в тарболе лежит: гард-тест сверяет список
`npm pack --dry-run` с целями `exports` и не пускает в тарбол исходники. Монорепа без
предварительной сборки работает, потому что источники резолвятся напрямую:

| Кто | Механизм | Что видит |
| --- | --- | --- |
| tsc монорепы (`typecheck`) | `paths` корневого tsconfig (перечислены используемые сабпути) | исходники |
| rsbuild фронтенда | `source.alias` (те же сабпути, сначала более длинные) | исходники |
| e2e-тест страницы File Manager | loader-хук `frontend/test/native-api.test.mjs`, собранный из `alef-sources` | исходники |
| потребитель тарбола (devflow, npm) | условия `types`/`import`/`default` | `dist/` |

Новый используемый сабпуть добавляется и в `paths`, и в `alias` — за этим (по значениям,
а не только по ключам) следит тест `frontend/test/native-api.test.mjs`.

Тарбол: `files: ["dist"]` — исходники не публикуются, `prepack` собирает их перед
упаковкой. Гард-тесты сегментации после сборки проверяют, что у каждого сабпути есть
файлы в `dist/`, что `default` ведёт на тот же модуль, что и `import`, что type-only
сабпути не оставляют js-заглушек, что декларации не называют `.ts` и что тарбол несёт
всё рекламируемое и ничего кроме `dist/`. Релизный сторедж
(`scripts/release/publish-package.mjs`) падает на файле из `files`, которого нет, а в
`.github/workflows/release.yml` перед стореджем добавлен шаг сборки `dist/`. Сценарий
e2e `devflow` собирает пакет перед `npm pack` и гоняет приложение, которое импортирует
сабпути из установленного тарбола.

Не потребитель сабпутей — `packages/mcp`: он собирается вместе с исходниками по
относительным путям (`../../api/src/...`) и карту `exports` не резолвит. Страницы e2e
импортируют уже собранное дерево (`./src/index.js`), там сабпутей нет.

## Запуск в CI

Гард тесты сегментации запускаются при изменениях в `packages/api/**`,
`docs/api/**` или `scripts/ci/**`. Перечислять рёбра здесь нельзя —
см. тест `packages/api/test/modules/segments.test.mjs`.
