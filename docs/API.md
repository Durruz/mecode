# Mecode API (контракт ядра для интерфейса)

Сервер: `cargo run -p mecode-server` → `http://127.0.0.1:7878` (порт в `%APPDATA%\mecode\config.json`).
Слушает только 127.0.0.1. Запросы с чужим `Origin` получают 403. Разрешены: тот же хост,
`tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`.

## Модель

```ts
type AgentId = "claude" | "codex";
type Author  = "user" | "claude" | "codex" | "system";
type Kind    = "chat" | "activity" | "error";   // activity = команда/правка файла агента
type Target  = "all" | "claude" | "codex";       // кому уходит сообщение без @упоминаний

interface Message { seq: number; author: Author; kind: Kind; text: string; ts: string /* ISO */;
                    attachments?: Attachment[]; plan?: true }
interface Attachment { id: string; name: string; path: string; size: number; is_image: boolean }
interface SavedPrompt { id: string; title: string; text: string }
interface Room {
  id: string; title: string; workspace: string; created: string;
  default_target: Target;
  sessions: Partial<Record<AgentId, string>>;  // служебное
  cursors:  Partial<Record<AgentId, number>>;  // служебное
}
interface RoomView extends Room { busy: AgentId[] }   // кто сейчас работает

// Дополнительные поля Room:
//   agents:  Record<AgentId, AgentSettings>          — всегда есть оба
//   context: Partial<Record<AgentId, ContextUsage>>  — появляется после первого хода агента
type Permission = "read" | "edits" | "auto" | "full";
interface AgentSettings { model: string /* "" = по умолчанию */; effort: string /* "" = по умолчанию */;
                          permission: Permission; fast: boolean /* только если ModelInfo.fast */ }
interface ContextUsage { used: number; window: number | null }

interface ModelInfo { id: string /* "" = по умолчанию */; name: string; description: string;
                      efforts: string[]; default_effort: string | null; is_default: boolean;
                      fast: boolean /* есть Fast mode: у Codex да, у Claude нет (CLI не даёт в режиме -p) */ }
interface Limits { plan: string | null /* "pro", "plus"… */;
                   windows: { id: "5h" | "7d"; used_percent: number; resets_at: number | null /* unix сек */ }[];
                   updated_at: string }
```

### Что значат разрешения

| permission | Claude Code | Codex |
|---|---|---|
| `read` | `dontAsk`: читает, всё остальное молча отклоняется | песочница `read-only` |
| `edits` | `acceptEdits`: правки файлов в папке проекта принимаются, команды и файлы вне папки отклоняются. Песочницы ОС нет | песочница ОС `workspace-write`: запись только в папку проекта |
| `auto` | `auto`: решения принимает классификатор Claude Code, может выйти за папку | `workspace-write` + автопроверка запросов (`approvals_reviewer=auto_review`) |
| `full` | `bypassPermissions`: без ограничений | без песочницы и подтверждений |

Режим `plan` у Claude не используется: в нём Claude Code подменяет выбранную модель на Sonnet.

## REST (JSON, `Content-Type: application/json`)

| Метод | Путь | Тело | Ответ |
|---|---|---|---|
| GET | `/api/health` | — | `{ok, claude: string\|null, codex: string\|null}` — версии CLI, `null` = не найден |
| GET | `/api/rooms` | — | `RoomView[]` (новые сверху) |
| POST | `/api/rooms` | `{title?, workspace}` | `Room`; `workspace` — существующая папка проекта |
| GET | `/api/rooms/{id}` | — | `RoomView` |
| PATCH | `/api/rooms/{id}` | `{title}` | `Room` — переименование |
| DELETE | `/api/rooms/{id}` | — | `{ok}` — удаляет чат и историю безвозвратно, агенты останавливаются. В UI спрашивать подтверждение |
| GET | `/api/rooms/{id}/messages` | — | `Message[]` |
| POST | `/api/rooms/{id}/messages` | `{text, attachments?: string[], plan?: boolean}` | `Message` (сообщение пользователя). `attachments` — id загруженных файлов. `plan` — режим плана: в этот ход агенты только читают и предлагают план |
| POST | `/api/rooms/{id}/attachments?name=<имя>` | сырые байты файла, до 25 МБ | `Attachment`. Файл уходит агентам с сообщением, где указан его id: путь к нему попадает в промпт, картинки Codex получает через `-i` |
| POST | `/api/rooms/{id}/handoff` | `{title?}` | `Room` — новый чат в той же папке, с теми же настройками агентов и историей первым системным сообщением; сессии агентов новые |
| GET | `/api/prompts` | — | `SavedPrompt[]` |
| POST | `/api/prompts` | `{id?, title?, text}` | `SavedPrompt` — без id создаёт, с id обновляет; пустой title = начало текста |
| DELETE | `/api/prompts/{id}` | — | `{ok}` |
| POST | `/api/rooms/{id}/stop` | — | `{ok}` — остановить всех агентов комнаты |
| PUT | `/api/rooms/{id}/target` | `{target}` | `{ok}` |

| PUT | `/api/rooms/{id}/agents/{claude\|codex}` | `AgentSettings` | `Room` — действует со следующего хода агента |
| GET | `/api/models` | — | `{claude: ModelInfo[], codex: ModelInfo[]}`. Claude — псевдонимы (всегда последние версии), Codex — из `codex app-server`, кэш 10 мин |
| GET | `/api/limits` | — | `{claude: Limits\|null, codex: Limits\|null}` — последние известные, хранятся между запусками |
| POST | `/api/limits/refresh` | — | то же, свежее. Для Claude делает крошечный запрос к Haiku (тратит каплю лимита) — только по кнопке. `?claude=false` — только Codex, бесплатно, можно раз в минуту |

Ошибка: HTTP 400 `{error: string}`.

Лимиты также обновляются сами после каждого хода агента (событие `limits`), а контекст — через `room_updated`.

## События: WebSocket `ws://127.0.0.1:7878/api/events`

Один сокет на все комнаты, JSON по одному событию в сообщении:

```ts
{ type: "message",      room_id: string, message: Message }
{ type: "agent_status", room_id: string, agent: AgentId, busy: boolean }
{ type: "room_updated", room: Room }
{ type: "room_deleted", room_id: string }
{ type: "delta",        room_id: string, agent: AgentId, text: string }
{ type: "limits",       agent: AgentId, limits: Limits }
{ type: "resync" }   // клиент отстал — перечитать комнаты/сообщения через REST
```

`delta` — текст, который агент печатает прямо сейчас, **накопленный целиком** (заменять, а не дописывать).
Показывать «печатает…»-пузырём; убирать, когда пришёл `message` с `kind: "chat"` от этого агента
или `agent_status` с `busy: false`. Сейчас поток идёт только у Claude; Codex CLI отдаёт реплики целиком.

Своё отправленное сообщение тоже приходит событием `message`: рисовать по событию, не по ответу POST,
иначе будет дубль. При обрыве — переподключиться и перечитать `/api/rooms/{id}/messages`.

## Правила маршрутизации (для подсказок в UI)

- `@claude`, `@codex`, `@all` (`@все`, `@оба`, `@клод`, `@кодекс`) в сообщении пользователя — кому отвечать.
  Без упоминаний — по `default_target` комнаты.
- Агент упоминает `@другого` → тот просыпается и видит реплику. Подряд без пользователя — не больше
  `max_agent_hops` (по умолчанию 8), потом системное сообщение о паузе.
- Агенты видят только `kind: "chat"`. `activity` и `error` — только для человека.

## Встраивание в Tauri

```rust
let hub = mecode_core::Hub::new(cfg, &data_dir)?;
let (addr, _server) = mecode_server::serve(hub, 0).await?;  // 0 = свободный порт; addr отдать во фронтенд
```

В CSP окна нужно разрешить `connect-src http://127.0.0.1:* ws://127.0.0.1:*`.
