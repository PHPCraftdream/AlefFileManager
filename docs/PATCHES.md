# Локальные патчи crates.io

## Область и воспроизводимая база

Опись проверена 2026-10-11 против **архивов опубликованных крейтов**, а не Git-тегов. Подключение четырёх копий: `[patch.crates-io]` в `backend/Cargo.toml`. Этот документ не меняет Rust, зависимости или CI. Проверка реализована в `scripts/supply/check-patches.mjs`, offline fixtures — в `scripts/supply/check-patches.test.mjs`.

Все файлы архивов и локальных каталогов просмотрены рекурсивно, включая скрытые, бинарные, добавленные и удалённые файлы. Нормализация сравнения: только замена байтов CRLF на LF; остальные байты, BOM, пробелы и финальный перевод строки сохраняются. Нельзя декодировать произвольные бинарные файлы как UTF-8. Режимы доступа, времена и каталоги не входят в файловую опись; символические ссылки и специальные файлы проверка отклоняет, а не молча пропускать.

Источник реестра взят из registry-записей `backend/Cargo.lock`: `registry+https://github.com/rust-lang/crates.io-index`. `https://index.crates.io/` — официальный sparse endpoint того же crates.io, не отдельный источник зависимостей. Для каждого имени прочитана запись **точной версии**; SHA-256 скачанного `.crate` совпал с её `cksum`. Адрес загрузки получен из `https://index.crates.io/config.json` (`dl`), результат — `https://static.crates.io/crates/<name>/<version>/download`. SHA-256 считается по сжатому архиву **до распаковки**, без нормализации.

**В основном lock-файле `backend/Cargo.lock` все четыре patched package имеют имя и версию, но НЕ имеют `source` и `checksum`.** Это path-пакеты. Нельзя приписывать этому lock-файлу контрольные суммы архивов. Основной lock фиксирует версии: winit **0.30.13** (не `.30.13`), surfman **0.13.0**, servo-paint и servo-paint-api **0.6.0**.

Для servo-paint-api и surfman дополнительный источник checksum — registry-записи `backend/patches/servo-paint/Cargo.lock`; они совпадают с индексом и архивами. Для winit и servo-paint соответствующих registry-checksum в этом вложенном lock нет: пины получены из exact-version записей индекса. Сам servo-paint во вложенном lock — корневой пакет без checksum. `Cargo.lock` из скачанного архива не является независимым доказательством собственной checksum крейта.

| Крейт | Версия | SHA-256 архива | Источник checksum кроме индекса |
|---|---|---|---|
| winit | 0.30.13 | `a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d` | нет в основном и указанном вложенном lock |
| surfman | 0.13.0 | `6ef7ecad967262d0b8bf1cbe753fe9b13ddf068606d30ceb370b87c73c5964b5` | `backend/patches/servo-paint/Cargo.lock`, registry package surfman 0.13.0 |
| servo-paint | 0.6.0 | `5a657f7253becd768e52c6724860a2f131c1e69e9d4e8076d8fb0d4927b0e054` | нет в основном и указанном вложенном lock |
| servo-paint-api | 0.6.0 | `58345d95996801b228e30fb76228ba9f6b4378961b802519f3e3ef0533ab1d10` | `backend/patches/servo-paint/Cargo.lock`, registry package servo-paint-api 0.6.0 |

## История, причины и подозрительные отличия

- `ef343c2`: введён servo-paint-api. В `rendering_context.rs` контекст становится current **до** загрузки GL-функций; это подтверждают diff, комментарий и README. В `display_list.rs`, `lib.rs`, `tests/compositor.rs` также заменены `ref`/`ref mut` на сопоставление с явной ссылкой. Преобразования похожи на адаптацию match ergonomics, но отдельное обоснование не найдено: **причина не восстановлена**. Удаление опубликованного servo-paint-api `Cargo.lock` тоже не объяснено историей.
- `e95520b`: Windows-патч winit — packed signed coordinates для `WM_NCLBUTTONDOWN`, тест отрицательных координат; maximized state из HWND; синхронизация cached flags; `dragging` устанавливается при `WM_ENTERSIZEMOVE`, а не по намерению начать drag. Сброс в `WM_EXITSIZEMOVE` уже был в оригинале: это не дополнительный изменённый участок. Источники: diff, README и `RESIZE-INVESTIGATION.md`.
- `0d308f4`: servo-paint ждёт display list корневого pipeline для нового viewport и не принимает старые WebRender-кадры за готовый resize. Состояние принадлежит конкретному Painter; допуск размера ±1 px, таймаут 250 мс, добавлен модуль с тестами. `[workspace]` в его `Cargo.toml` изолирует path-крейт от workspace хоста. Источники: diff, комментарий манифеста, `RESIZE-INVESTIGATION.md`, `docs/stages/m0-results/multiwindow.md`. Это происхождение патча, **не новое подтверждение плавности resize**; расследование отмечает необходимость визуальной проверки.
- `c103929`: surfman WGL helper `SurfmanFalseWindow` больше не имеет `WS_VISIBLE`, чтобы не мигать при запуске. Но копия одновременно не содержит 44 опубликованных файла (включая metadata): workflow, gitignore и Android-пример с Gradle wrapper, ресурсами и тестами. История введения копии фиксирует их отсутствие, но не объясняет удаление: **причина не восстановлена**. Это не «только двухстрочный патч» на уровне всего архива.
- `652f95c`: surfman X11 считает пользователей общего EGLDisplay и вызывает Terminate лишь для последнего. По сообщению коммита закрытие одного окна ломало контексты остальных (`BadDisplay` в Linux CI).
- `17d14c5`: в оба Servo-крейта добавлен текст `LICENSE-MPL`; оригинальные архивы этих файлов не содержат. Лицензия крейтов уже была MPL-2.0; это добавление текста, а не смена лицензии.
- servo-paint `.cargo-ok` — tracked добавление, отсутствующее в оригинальном `.crate`; содержимое ровно `{"v":1}`. Похоже на маркер распаковки Cargo, но происхождение именно этой копии не доказано: **причина не восстановлена**. Не исключать его общим правилом «служебные файлы».

### Диск и Git: особый случай winit Cargo.lock

В текущем worktree файл **существует на диске**, отличается от опубликованного: lock format 4 → 3, множество версий и checksum транзитивных зависимостей изменено (например ab_glyph 0.2.28 → 0.2.32). Он **не tracked**; точное правило `.gitignore` `/backend/patches/winit/Cargo.lock` добавлено в `e95520b`. История `RESIZE-INVESTIGATION.md` упоминает отдельный запуск regression test winit, но не доказывает, каким запуском создан этот lock: **причина не восстановлена**.

Решение: каноническая опись — содержимое **tracked-копии**, сравниваемое с архивом. Поэтому её `Cargo.lock` имеет статус `removed`; в `diskObservations` отдельно записан фактический статус `changed`. Чистый checkout без этого локального файла должен проходить. Checker терпит только этот точный ignored/untracked путь, отмечая его в кратком успешном выводе, но не считать его частью патча, не использовать как pin оригинального winit и не требовать текущего локального содержимого. Если файл станет tracked, это переход `removed` → `changed`/`unchanged` и проверка обязана упасть. Другие ignored/untracked файлы внутри четырёх копий не разрешены автоматически.

| Крейт | Файлов в архиве | На диске | Tracked | Отличий диска / tracked от архива |
|---|---:|---:|---:|---:|
| winit | 217 | 216 | 215 | 5 / 5 (Cargo.lock имеет разные статусы) |
| surfman | 135 | 91 | 91 | 46 / 46 |
| servo-paint | 17 | 20 | 20 | 6 / 6 |
| servo-paint-api | 10 | 9 | 9 | 7 / 7 |

Итого 64 отличия до разрешённого исключения metadata; после исключения — 61. Неизменённых файлов в JSON нет, но проверка сравнивает **все** файлы, чтобы новые отличия не исчезали за allowlist.

## Точные исключения и JSON-опись

Единственное игнорируемое publication metadata: файл `.cargo_vcs_info.json` **в корне каждого из четырёх крейтов**, без wildcard и рекурсивного исключения. Он описывает upstream Git SHA и путь публикации, не участвует в сборке; удалён у winit, surfman, servo-paint-api, сохранён без изменений у servo-paint. Ниже эти три удаления всё равно записаны как наблюдения с `ignored: true`. Upstream SHA из архивов: winit `e9809ef54b18499bb4f2cac945719ecc2a61061b`, surfman `205778f497327c573929c7b471194390e15f331d`, оба Servo `c78d2c206f80a1c8b67eefa97f773bba513205d3` (соответственно `components/paint`, `components/shared/paint`). Это provenance публикации, не коммиты локальных патчей.

`Cargo.toml.orig` **не исключается**: у всех четырёх копий он совпал с архивом после CRLF-нормализации; оснований скрывать будущие изменения нет. Нормализованный `Cargo.toml` тоже проверяется. Не исключаются `.cargo-ok`, `Cargo.lock` (кроме отдельно описанной disk-only обработки winit), `.github`, `.gitignore`, примеры, лицензии, бинарные ресурсы.

Пути JSON относительны `root` соответствующего крейта; это переносимые идентификаторы, не локальные абсолютные пути. `status` — отличие tracked-копии от архива; `tracked` — наличие в Git index на момент аудита. `source` фиксирует источник вывода; `reason` не выдумывает мотивацию. `normalizedSha256` у changed/added — SHA-256 локальных байтов после CRLF → LF, дополнительный pin содержимого разрешённого патча. У removed его нет.

```json
{
  "schemaVersion": 1,
  "comparison": "tracked-working-tree-vs-verified-crate",
  "normalization": "CRLF-to-LF-bytes-only",
  "ignoredPublicationMetadata": [
    ".cargo_vcs_info.json"
  ],
  "crates": [
    {
      "name": "winit",
      "version": "0.30.13",
      "root": "backend/patches/winit",
      "sha256": "a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d",
      "checksumSources": [
        {
          "kind": "registry-exact-version",
          "url": "https://index.crates.io/wi/ni/winit",
          "version": "0.30.13",
          "field": "cksum"
        }
      ],
      "primaryLock": {
        "path": "backend/Cargo.lock",
        "version": "0.30.13",
        "checksum": null,
        "source": null
      },
      "archiveUrl": "https://static.crates.io/crates/winit/0.30.13/download",
      "files": [
        {
          "path": ".cargo_vcs_info.json",
          "status": "removed",
          "tracked": false,
          "source": "verified archive metadata; сравнение файлов",
          "reason": "Разрешённое исключение publication metadata; мотивация отсутствия в копии не восстановлена.",
          "ignored": true
        },
        {
          "path": "Cargo.lock",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git e95520b (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "src/platform_impl/windows/event_loop.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git e95520b; RESIZE-INVESTIGATION.md",
          "reason": "Устанавливать dragging при WM_ENTERSIZEMOVE; сброс WM_EXITSIZEMOVE уже в оригинале.",
          "normalizedSha256": "11038b0a1855db7d123dbaf935376802003b3e807018684dc093f38be6bc1338"
        },
        {
          "path": "src/platform_impl/windows/window.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git e95520b; RESIZE-INVESTIGATION.md",
          "reason": "Packed signed coordinates WM_NCLBUTTONDOWN, regression test; maximized из HWND; drag guard по native lifecycle.",
          "normalizedSha256": "263a3969b84239cea665b581488e43f6de645220eabe7ef5b22ff78ff00926ba"
        },
        {
          "path": "src/platform_impl/windows/window_state.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git e95520b; RESIZE-INVESTIGATION.md",
          "reason": "Синхронизировать cached MAXIMIZED с HWND перед изменением flags.",
          "normalizedSha256": "4885b59c2a857425b5f63302f9baff6706c62d87060e194f5fd55645fb09b282"
        }
      ]
    },
    {
      "name": "surfman",
      "version": "0.13.0",
      "root": "backend/patches/surfman",
      "sha256": "6ef7ecad967262d0b8bf1cbe753fe9b13ddf068606d30ceb370b87c73c5964b5",
      "checksumSources": [
        {
          "kind": "registry-exact-version",
          "url": "https://index.crates.io/su/rf/surfman",
          "version": "0.13.0",
          "field": "cksum"
        },
        {
          "kind": "lock",
          "path": "backend/patches/servo-paint/Cargo.lock",
          "name": "surfman",
          "version": "0.13.0",
          "source": "registry+https://github.com/rust-lang/crates.io-index",
          "field": "checksum"
        }
      ],
      "primaryLock": {
        "path": "backend/Cargo.lock",
        "version": "0.13.0",
        "checksum": null,
        "source": null
      },
      "archiveUrl": "https://static.crates.io/crates/surfman/0.13.0/download",
      "files": [
        {
          "path": ".cargo_vcs_info.json",
          "status": "removed",
          "tracked": false,
          "source": "verified archive metadata; сравнение файлов",
          "reason": "Разрешённое исключение publication metadata; мотивация отсутствия в копии не восстановлена.",
          "ignored": true
        },
        {
          "path": ".github/workflows/main.yml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": ".gitignore",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/.gitignore",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/.gitignore",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/build.gradle",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/proguard-rules.pro",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/androidTest/java/org/mozilla/surfmanthreadsexample/SurfmanInstrumentedTest.java",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/AndroidManifest.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/assets/blit.fs.glsl",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/assets/check.fs.glsl",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/assets/grid.fs.glsl",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/assets/quad.vs.glsl",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/assets/tri.fs.glsl",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/assets/tri.vs.glsl",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/java/org/mozilla/surfmanthreadsexample/MainActivity.java",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/java/org/mozilla/surfmanthreadsexample/SurfmanThreadsExampleRenderer.java",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/java/org/mozilla/surfmanthreadsexample/SurfmanThreadsExampleResourceLoader.java",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/drawable-v24/ic_launcher_foreground.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/drawable/ic_launcher_background.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/layout/activity_main.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-anydpi-v26/ic_launcher.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-anydpi-v26/ic_launcher_round.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-hdpi/ic_launcher.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-hdpi/ic_launcher_round.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-mdpi/ic_launcher.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-mdpi/ic_launcher_round.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-xhdpi/ic_launcher.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-xhdpi/ic_launcher_round.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-xxhdpi/ic_launcher.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-xxhdpi/ic_launcher_round.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-xxxhdpi/ic_launcher.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/mipmap-xxxhdpi/ic_launcher_round.png",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/values/colors.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/values/strings.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/main/res/values/styles.xml",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/app/src/test/java/org/mozilla/surfmanthreadsexample/ExampleUnitTest.java",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/build.gradle",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/gradle.properties",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/gradle/wrapper/gradle-wrapper.jar",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/gradle/wrapper/gradle-wrapper.properties",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/gradlew",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/gradlew.bat",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "android-example/settings.gradle",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git c103929 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "src/wgl/context.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git c103929",
          "reason": "Убрать WS_VISIBLE у служебного WGL окна, предотвращая мигание.",
          "normalizedSha256": "4b3ae077692cace3fef3d9761108d60083b3ec1b8405c0fc1dd39a9a6828fb0b"
        },
        {
          "path": "src/x11/connection.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git 652f95c",
          "reason": "Terminate общего EGLDisplay только после последнего пользователя, предотвращая BadDisplay остальных окон.",
          "normalizedSha256": "171f58f5ddba085d17b634e99b334a1cfb1e3691fc75aca09554aa12fa0cad69"
        }
      ]
    },
    {
      "name": "servo-paint",
      "version": "0.6.0",
      "root": "backend/patches/servo-paint",
      "sha256": "5a657f7253becd768e52c6724860a2f131c1e69e9d4e8076d8fb0d4927b0e054",
      "checksumSources": [
        {
          "kind": "registry-exact-version",
          "url": "https://index.crates.io/se/rv/servo-paint",
          "version": "0.6.0",
          "field": "cksum"
        }
      ],
      "primaryLock": {
        "path": "backend/Cargo.lock",
        "version": "0.6.0",
        "checksum": null,
        "source": null
      },
      "archiveUrl": "https://static.crates.io/crates/servo-paint/0.6.0/download",
      "files": [
        {
          "path": ".cargo-ok",
          "status": "added",
          "tracked": true,
          "source": "сравнение verified crates.io archive с tracked-копией; git 0d308f4 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена",
          "normalizedSha256": "afbf9d0f3560b0fd7795e81c42a0a79ee6b6fc67e064f77826aee642cad28d91"
        },
        {
          "path": "Cargo.toml",
          "status": "changed",
          "tracked": true,
          "source": "diff; комментарий Cargo.toml; git 0d308f4",
          "reason": "Изолировать path-крейт от workspace хоста через [workspace].",
          "normalizedSha256": "e3924065192f90d13fa18001a3f4515767664c89043c58c542e336290ab011b2"
        },
        {
          "path": "LICENSE-MPL",
          "status": "added",
          "tracked": true,
          "source": "отсутствует в архиве; git 17d14c5",
          "reason": "Поставлять текст исходной лицензии MPL-2.0 рядом с patched Servo sources.",
          "normalizedSha256": "1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5"
        },
        {
          "path": "lib.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git 0d308f4; RESIZE-INVESTIGATION.md",
          "reason": "Подключить модуль resize_wait.",
          "normalizedSha256": "2fa852d1d233a03d7281735103b4da54158f059b398b9eac1330c9a7d76f8e02"
        },
        {
          "path": "painter.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; git 0d308f4; RESIZE-INVESTIGATION.md",
          "reason": "Ожидать root display list нового размера и не уведомлять готовность старых resize-кадров; состояние per Painter.",
          "normalizedSha256": "ac9635121ac9201da8e2cca4e0f5ab82904e3dbf4b95f201919e05dfe266a74f"
        },
        {
          "path": "resize_wait.rs",
          "status": "added",
          "tracked": true,
          "source": "добавленный файл; git 0d308f4; RESIZE-INVESTIGATION.md",
          "reason": "Хелперы согласования viewport, pending frames, timeout 250 мс и шесть unit-тестов.",
          "normalizedSha256": "bbb30bc0704d710887016070f72f3198e7e49dc3f5819f4eb48e1c16569adce9"
        }
      ]
    },
    {
      "name": "servo-paint-api",
      "version": "0.6.0",
      "root": "backend/patches/servo-paint-api",
      "sha256": "58345d95996801b228e30fb76228ba9f6b4378961b802519f3e3ef0533ab1d10",
      "checksumSources": [
        {
          "kind": "registry-exact-version",
          "url": "https://index.crates.io/se/rv/servo-paint-api",
          "version": "0.6.0",
          "field": "cksum"
        },
        {
          "kind": "lock",
          "path": "backend/patches/servo-paint/Cargo.lock",
          "name": "servo-paint-api",
          "version": "0.6.0",
          "source": "registry+https://github.com/rust-lang/crates.io-index",
          "field": "checksum"
        }
      ],
      "primaryLock": {
        "path": "backend/Cargo.lock",
        "version": "0.6.0",
        "checksum": null,
        "source": null
      },
      "archiveUrl": "https://static.crates.io/crates/servo-paint-api/0.6.0/download",
      "files": [
        {
          "path": ".cargo_vcs_info.json",
          "status": "removed",
          "tracked": false,
          "source": "verified archive metadata; сравнение файлов",
          "reason": "Разрешённое исключение publication metadata; мотивация отсутствия в копии не восстановлена.",
          "ignored": true
        },
        {
          "path": "Cargo.lock",
          "status": "removed",
          "tracked": false,
          "source": "сравнение verified crates.io archive с tracked-копией; git ef343c2 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена"
        },
        {
          "path": "LICENSE-MPL",
          "status": "added",
          "tracked": true,
          "source": "отсутствует в архиве; git 17d14c5",
          "reason": "Поставлять текст исходной лицензии MPL-2.0 рядом с patched Servo sources.",
          "normalizedSha256": "1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5"
        },
        {
          "path": "display_list.rs",
          "status": "changed",
          "tracked": true,
          "source": "сравнение verified crates.io archive с tracked-копией; git ef343c2 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена",
          "normalizedSha256": "5313207a75c27e26f6a63aa5384a7730a866f46ae8720984c631b8d0988fcaef"
        },
        {
          "path": "lib.rs",
          "status": "changed",
          "tracked": true,
          "source": "сравнение verified crates.io archive с tracked-копией; git ef343c2 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена",
          "normalizedSha256": "7d24ae4d6376836a78efb55c0aef6d45c8c122cdae85f24a7dc7f59920b048b5"
        },
        {
          "path": "rendering_context.rs",
          "status": "changed",
          "tracked": true,
          "source": "diff; комментарий rendering_context.rs; git ef343c2; README.md",
          "reason": "Сделать GL context current до загрузки GL-функций.",
          "normalizedSha256": "101231cca9b675478d3806d63bff4cd0746a553bd7277dae3c4d635af8a75272"
        },
        {
          "path": "tests/compositor.rs",
          "status": "changed",
          "tracked": true,
          "source": "сравнение verified crates.io archive с tracked-копией; git ef343c2 (первое внесение копии, не доказательство мотивации)",
          "reason": "причина не восстановлена",
          "normalizedSha256": "826c322d8a40d025911792c5b1eb80ed40debebd0b274e804ad044a06c996bd4"
        }
      ]
    }
  ],
  "diskObservations": [
    {
      "crate": "winit",
      "path": "Cargo.lock",
      "status": "changed",
      "tracked": false,
      "ignoredByGit": true,
      "gitignoreRule": "/backend/patches/winit/Cargo.lock",
      "canonicalStatus": "removed",
      "policy": "optional-disk-only; never use for archive checksum; tracking it fails canonical status check",
      "source": "disk/archive diff; git ls-files; git check-ignore; git e95520b",
      "reason": "причина не восстановлена"
    }
  ]
}
```

## Контракт проверки

Команды из корня репозитория:

```sh
node scripts/supply/check-patches.mjs
node --test scripts/supply/check-patches.test.mjs
```

1. Прочитать единственный JSON-блок описи, проверить схему, уникальность имён и путей, допустимые статусы, точные версии и SHA-256; reject абсолютные пути и `..`. Проверить `[patch.crates-io]` и `package.name`/`package.version` локальных манифестов, отсутствие пропущенных/лишних patched crate.
2. **Обязательно** сверить все четыре версии с основным `backend/Cargo.lock`, даже когда `checksum` отсутствует. Отсутствие package или несовпадение версии — ошибка; отсутствие checksum в этих четырёх path-записях — документированное состояние, не разрешение пропустить проверку версии.
3. Найти доступные checksum точного `(name, version, registry source)` в основном и вложенных tracked lock-файлах четырёх копий. Сейчас независимые lock-пины surfman и servo-paint-api доступны в `backend/patches/servo-paint/Cargo.lock`. Любая найденная checksum должна совпасть с **пином документа**, exact-version записью индекса и скачанным архивом; нельзя выбрать удобный источник и проигнорировать конфликт. Без checksum у winit/servo-paint использовать pin документа + индекс; отсутствие lock-checksum описано выше. Не извлекать «собственную checksum» из path-package и не доверять disk-only lock winit.
4. Получить архив точной версии, проверить SHA-256 **до распаковки**. Проверить также кэш, не доверять имени файла. Распаковать в новую временную директорию с проверкой tar entries: единственный ожидаемый префикс `<name>-<version>/`, отсутствие traversal, абсолютных имён, ссылок, специальных файлов и дубликатов. Не распаковывать в репозиторий.
5. Сравнить объединение всех путей оригинала и tracked-копии, читая текущие bytes tracked-файлов с диска (не старые blobs HEAD). Tracked, но отсутствующий на диске файл — ошибка. Отдельно проверить дерево диска на непредусмотренные untracked/ignored добавления; listed changed/added передать сравнению статуса, равенства Git tracking полю `tracked` и content pin, чтобы корректные bytes без Git tracking дали именованный `tracking mismatch`. Применить только точные исключения выше.
6. Проверять равенство множеств **`(crate, path, status)`**, не только имён. `changed` требует наличия обоих файлов и различия; `added` — отсутствия в архиве и наличия локально; `removed` — наличия в архиве и отсутствия в tracked-копии. Удаление listed changed, восстановление removed, удаление added, возврат changed к оригиналу и любой переход статуса обязаны завершаться ошибкой. Сверить `normalizedSha256` changed/added: allowlist путей не разрешает произвольную новую правку в уже изменённом файле.
7. Exit **0** — проверка выполнена, совпала и временная распаковка удалена; **1** — несовпадение pin/версии/статуса/содержимого, неправильная опись, unsafe archive, неожиданный файл или отсутствующий обязательный документ/манифест/lock (ENOENT с точным repo-relative путём); **2** — проверку невозможно завершить из-за сети, timeout, недоступного/непригодного временного кэша, другого файлового I/O или отсутствующего распаковщика. Ошибка удаления временной распаковки сообщает `patches: temporary cleanup failed`: после успешной проверки даёт exit 2 без вывода OK, после неуспешной сохраняет исходный exit 1/2. Exit 2 не считать успехом или пропуском CI. Полученный архив с неверной checksum — exit 1, а не «сеть недоступна».

Offline-тесты без Rust/cargo: CRLF/LF-equivalence и бинарные байты; новая changed/added/removed разница; все переходы статусов (особенно удаление listed changed); изменение содержимого уже разрешённого changed/added; конфликт doc/index/lock checksum; отсутствие primary package и неверная primary version при отсутствии checksum; оба состояния disk-only winit lock и попытка сделать его tracked; hidden files, строгий ignore metadata и проверяемый Cargo.toml.orig; poisoned cache; archive traversal/links; сеть/cache failure и другой I/O с exit 2; удаление обязательных docs/манифестов/locks с exit 1 и точным относительным именем; успешный/неуспешный cleanup после exit 0/1/2; точные cold/warm/refetched counts. Fixtures и мутации только в OS temp, не в исходных копиях.

## Архивы, временный кэш и обслуживание

Checker использует только Node.js built-ins и внешние read-only Git и tar. Перед распаковкой gzip ограничен 256 MiB; проверяются сырые tar headers, checksum заголовка, безопасные имена, ожидаемый префикс, уникальность, типы regular/directory и нулевой конец архива. Links, special files, PAX и GNU link extensions отвергаются; GNU long-name headers разрешены только с последующим безопасным regular/directory entry (нужны реальному surfman архиву): это ограниченный контракт для этих опубликованных архивов, не универсальный tar reader. Распаковка через stdin `tar -xzf -` с временным `cwd` обходит Windows drive-colon. Git/tar spawn ограничен 20 с; fetch вместе с чтением body — 15 с, ответ — 64 MiB. Все diagnostics используют переносимые repo-relative идентификаторы или общие сообщения, без приватных абсолютных путей.

Кэш по умолчанию `os.tmpdir()/alef-patches-cache-v1`: ключ name/version/doc SHA-256, отдельные `.json` exact-version registry record и `.crate`. Атомарная запись через уникальный временный файл; на каждом чтении сверяются имя/версия/cksum record и hash архива. Warm cache не обращается к свежему индексу. Успешный вывод после cleanup содержит число крейтов из проверенного кэша (`N verified cache`) и число загруженных заново (`M fetched`); сумма равна числу проверенных крейтов. Cold/повреждённый кэш требует свежей exact-version записи индекса и архива: универсального утверждения «no fresh index» нет. Повреждённый кэш перезагружается; при недоступной сети — exit 2, неверная полученная checksum — exit 1. Новые extraction-директории удаляются в finally; verified cache сохраняется. Кэш Cargo, репозиторий и patches не заполняются. Export helpers и async `main({root, cache, fetch, git, stdout, stderr, cleanup})` позволяют offline fixtures с synthetic tar, fake fetch и fake Git state; реальный CLI всегда читает Git index. TOML читается ограниченно: generated Cargo.lock package string fields и существующие простые manifest sections/path entries, не полный TOML parser.

### winit

Summary: Windows signed drag coordinates, native maximized flags и lifecycle dragging; upstream 0.30.13. Rebase: перенести только три Windows source diff; проверить native события и отрицательные координаты; повторно решить удаление upstream Cargo.lock. Disk-only ignored lock не переносить в patch и не использовать как checksum.

### surfman

Summary: невидимое WGL helper window и last-user EGLDisplay termination; upstream 0.13.0. Rebase: проверить наличие upstream fixes отдельно для WGL/X11, перенести отсутствующие fixes; пересмотреть все удалённые Android/workflow файлы, не считать их автоматически необходимыми удалениями; сверить nested lock pin.

### servo-paint

Summary: per-Painter resize wait, root display list, timeout 250 мс, workspace isolation и текст MPL; upstream 0.6.0. Rebase: согласовать Painter API с upstream, перенести resize_wait и подключения, отдельно проверить resize визуально; пересмотреть .cargo-ok, workspace и лицензионные добавления, обновить nested lock pins.

### servo-paint-api

Summary: current GL context до загрузки функций, reference patterns и текст MPL; upstream 0.6.0. Rebase: проверить upstream порядок GL initialization, перенести только недостающий fix; отдельно оценить patterns и удаление Cargo.lock, обновить nested lock pin.

### Общий порядок обновления

При обновлении upstream или патча: отдельно согласовать версии; получить новый точный архив и checksum из реестра; проверить имеющиеся lock-пины; сравнить всё дерево без расширения исключений; пересмотреть каждое added/removed и каждую причину; обновить JSON, content pins и сведения о provenance одновременно с изменением копии. Не менять checksum ради прохождения проверки и не делать автообновление описи самим checker. Для неизвестной мотивации оставлять «причина не восстановлена», а не ссылаться на ненайденный коммит. Визуальные/платформенные тесты эффективности патчей — отдельный gate: эта проверка доказывает происхождение и отсутствие неучтённых изменений, не корректность рендеринга.
