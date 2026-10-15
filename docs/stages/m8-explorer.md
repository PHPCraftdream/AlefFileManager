# M8 — Проводник: события ФС, иконки и контекстное меню

## Цель

Дать приложению-проводнику (File Manager и подобным) то, что есть у системного проводника: события об изменениях файловой системы и томов, системные иконки файлов и папок, элементы системного контекстного меню файла и их вызов. Всё асинхронно или потоком событий: ни один вызов не блокирует страницу, тяжёлая работа идёт вне потока интерфейса.

| Часть | Что |
|---|---|
| M8.1 | `fs.watch`: событие `attrib`; `fs.volumes()` и `fs.watchVolumes()` — тома, подключение и отключение |
| M8.2 | `shell.fileIcon()` — системная иконка файла, папки, диска или типа (PNG) |
| M8.3 | `shell.contextMenu()` — элементы системного контекстного меню и `invoke` |

Уже есть (M3.2): `fs.watch(path, { recursive })` — `create`/`modify`/`remove`/`rename`/`overflow` потоком (`AsyncIterable`), `readDirStream` пачками.

## M8.1 — события ФС и тома

## M8.2 — иконки

### API и границы безопасности

`shell.fileIcon(target, options?)` возвращает `{ png: Uint8Array, width, height }`. В `target` допускается ровно один селектор: `{ path }`, `{ extension }`, `{ mime }` или `{ kind: 'folder' | 'file' | 'drive' }`. Поддерживаемые размеры запроса: `16, 24, 32, 48, 64, 128, 256`; размер по умолчанию — `32`. Фактические размеры PNG сообщает ответ: платформенный источник может вернуть ближайший доступный размер или общий резервный PNG `32×32`.

Для `{ path }` backend сначала проверяет разрешение `fs.read`, затем существование пути, и только потом передаёт его рабочему потоку ОС. При политике подмены результатом служит встроенная общая иконка, без системного запроса по этому пути; отказ в разрешении также не доходит до OS lookup. Запросы без пути используют системные ассоциации типов/иконок и не требуют чтения файла.

### Выполнение, очередь и кэш

Подготовка (включая авторизацию и проверку существования), завершение кэша и формирование base64-ответа выполняются через `spawn_blocking`, а не на потоке Tokio. Запросы проходят через ограниченную очередь Tokio ёмкостью 32 к одному выделенному потоку `shell-icons` на процесс. Время ожидания включает помещение в очередь и ответ рабочего потока: лимит — 30 секунд. Уже отменённая задача пропускается до OS lookup; отмена активного системного вызова сама по себе не прерывает его. Кэш ограничен 256 записями для целей типа/расширения/MIME/вида; результаты конкретных путей не кэшируются.

### Источники и ограничения платформ

- **Windows:** `SHGetFileInfoW` предоставляет системные small (16 px) и large (32 px) иконки; выбирается ближайшая из них, при равном расстоянии — меньшая. Для legacy `HICON` восстанавливается альфа-канал рендерингом на чёрном и белом фоне, включая непрозрачные чёрные пиксели. Режим `DI_NORMAL`; миниатюры через `IShellItemImageFactory` и shell overlays не запрашиваются.
- **macOS:** AppKit `NSWorkspace` выбирает иконку пути либо типа (через Uniform Type Identifiers); она растеризуется в `NSBitmapImageRep` запрошенного размера и кодируется в PNG. Вызовы выполняются выделенным потоком. Сгенерированные объявления objc2 для `NSWorkspace` не устанавливают гарантию допустимости фонового потока; это ограничение проверки API, а не подтверждение thread-safety.
- **Linux:** GIO получает стандартную иконку/тип содержимого, а поиск файла темы выполняется вручную, чтобы не инициализировать GTK. Учитываются `GTK_THEME`, цепочка `Inherits` из `index.theme`, затем `Adwaita`/`hicolor`; поиск ограничен заданным набором распространённых размеров и контекстов. Это не полная реализация Freedesktop Icon Theme: произвольные каталоги размеров из `Directories` не сканируются. Отсутствие файла темы или ошибка декодирования дают общий PNG; ошибка GIO-запроса пути возвращается как ошибка.

Для системных целей типов, если источник/файл иконки недоступен, платформенный worker может вернуть встроенную общую иконку `32×32`; ошибки запроса пути не маскируются таким fallback.

### Проверки и доказательства

В исходниках имеются unit-тесты общего модуля: `an_unsupported_size_is_refused`, `nearest_size_breaks_ties_downward`, `rgba_png_round_trips_exact_pixels`, `png_header_requires_signature_and_ihdr`, MIME/расширения и десериализация цели, ключи/ёмкость кэша, `bounded_queue_skips_cancelled_jobs_and_replies_to_live_ones`. Linux: `theme_search_reaches_standard_sized_mimetype_paths_and_inheritance`, `no_theme_asset_is_a_generic_fallback_for_non_paths`; Windows: `mask_recovery_preserves_black_opaque_and_transparent_pixels`, `native_size_chooses_nearest_supported_shell_size`, `file_flags_select_small_or_large_shell_icon`, `fake_name_is_a_synthetic_extension_filename`; macOS: `requested_size_is_rendered_without_resampling`, `kind_targets_use_system_uniform_type_identifiers`. Эти тесты в рамках финализации M8.2 не запускались; сборка и CI также не запускались. Изменения API objc2 сверялись с generated source точных версий: [NSBitmapImageRep 0.3.2](https://docs.rs/objc2-app-kit/0.3.2/src/objc2_app_kit/generated/NSBitmapImageRep.rs.html), [NSGraphics 0.3.2](https://docs.rs/objc2-app-kit/0.3.2/src/objc2_app_kit/generated/NSGraphics.rs.html), [NSImage 0.3.2](https://docs.rs/objc2-app-kit/0.3.2/src/objc2_app_kit/generated/NSImage.rs.html), [NSWorkspace 0.3.2](https://docs.rs/objc2-app-kit/0.3.2/src/objc2_app_kit/generated/NSWorkspace.rs.html), [NSData 0.3.2](https://docs.rs/objc2-foundation/0.3.2/src/objc2_foundation/data.rs.html). Они подтверждают используемые декларации/методы, но не поведение на живых ОС.

## M8.3 — контекстное меню

## Приёмка

| Проверка | Как |
|---|---|
| `attrib` приходит при смене прав/времени; тома перечисляются, подключение виртуального тома видно событием | e2e + unit |
| Иконка файла, папки, типа: PNG нужного размера на трёх ОС | e2e |
| Контекстное меню файла: элементы есть, `invoke` безопасного элемента выполняется; без права — подмена (пустое меню) | e2e + ручная (расширения оболочки Windows) |
