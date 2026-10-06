# M7 — Дистрибуция

Обзор: `../FRAMEWORK-PLAN.md`. Зависит от M3 (генерический бинарник `alef`, File Manager как JS-приложение) и CI из M0.4.

## Цель

Разработчик без Rust: `npm create alef-tron` → `npm run dev` → `npm run build` → `npm run bundle` даёт установщик для Windows, macOS, Linux.

## Пакеты npm (scope `@alef-tron`)

| Пакет | Содержимое |
|---|---|
| `@alef-tron/api` | JS API (`packages/api`), сборка ESM + `.d.ts` |
| `@alef-tron/tools` | CLI `alef-tron`: `dev`, `build`, `bundle`, `check`; зависит от `@ktav-lang/ktav` для проверки `alef.ktav` |
| `@alef-tron/runtime-win32-x64`, `-darwin-arm64`, `-darwin-x64`, `-linux-x64` | бинарник `alef` под платформу; `os`/`cpu` в `package.json` |
| `@alef-tron/runtime` | мета-пакет: `optionalDependencies` на платформенные, выбор бинарника при запуске (как esbuild) |
| `create-alef-tron` | шаблон: `alef.ktav` (разделы безопасности закрыты), Vite/Rsbuild на выбор, пример вызова API |

Организацию `alef-tron` в npm зарегистрировать до первой публикации.

## Команды `@alef-tron/tools`

- `dev` — проверить `alef.ktav`, запустить dev-сервер фронтенда (команда из `package.json` или конфига) и `alef --app . --dev-url http://127.0.0.1:<port>`; HMR; закрытие окна останавливает dev-сервер.
- `build` — проверить манифест, собрать фронтенд, сложить `dist/` + `alef.ktav` в каталог приложения.
- `bundle` — поставить рядом бинарник runtime и ассеты, иконки, метаданные; собрать установщики:
  - Windows: NSIS или MSI (WiX), AppUserModelID, регистрация deep links, подпись (signtool, если есть сертификат);
  - macOS: `.app` (Info.plist: id, схемы, описания прав TCC), `.dmg`, codesign + notarization (если есть учётные данные);
  - Linux: AppImage и `.deb`, `.desktop` с `MimeType` для deep links.
- `check` — проверка манифеста и окружения.

Ассеты: каталог рядом с бинарником (MVP); позже — встраивание в ресурс бинарника.

## CI и выпуск

- Матрица из M0.4; релизный workflow по тегу: сборка runtime на 4 конфигурациях → артефакты → публикация npm-пакетов `@alef-tron/runtime-*`, затем `@alef-tron/runtime`, `@alef-tron/api`, `@alef-tron/tools`, `create-alef-tron` с согласованной версией.
- Версия протокола транспорта и версия `@alef-tron/api` проверяются рукопожатием `runtime.hello`.
- Политика версий: semver; до 1.0 — minor может ломать API, фиксируется в CHANGELOG.

## `updater`

```ts
updater.check(): Promise<{ available: boolean, version?, notes? }>
updater.download({ onProgress? }): Promise<void>
updater.install(): Promise<never>    // перезапуск
```

- Манифест: `updater: { endpoint: https://..., publicKey: <ed25519> }`; обновления подписываются ключом разработчика, runtime проверяет подпись (ed25519) до установки.
- Windows/Linux AppImage — замена файлов/перезапуск; macOS — замена `.app`; `.deb` — через пакетный менеджер (только уведомление).

## Размер

Release-бинарник runtime ~118 MB (Windows, Servo 0.6). Цели: strip символов, LTO в релизе, сжатие в установщике; замер на каждой платформе в CI.

## Приёмка

| Проверка | Как |
|---|---|
| Чистая машина без Rust: `npm create alef-tron`, `dev`, `build`, `bundle` | CI-job на трёх ОС |
| Установщик ставит, запускает, удаляет приложение | ручная на трёх ОС |
| File Manager собран как обычное Alef-приложение и установлен | ручная |
| `updater`: обновление с подписью ставится; с неверной подписью — отказ | e2e |
| npm-пакеты публикуются из CI по тегу | dry-run публикации |

## Риски

- Подпись и нотаризация macOS требуют Apple Developer аккаунта; без них — неподписанные сборки с предупреждением Gatekeeper.
- Время сборки Servo в CI — кэш обязателен; релизная сборка на 4 платформах — часы.
