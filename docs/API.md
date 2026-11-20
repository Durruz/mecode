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

interface Message { seq: number; author: Author; kind: Kind; text: string; ts: string /* ISO */ }
interface Room {
  id: string; title: string; workspace: string; created: string;
  default_target: Target;
  sessions: Partial<Record<AgentId, string>>;  // служебное
  cursors:  Partial<Record<AgentId, number>>;  // служебное
}
interface RoomView extends Room { busy: AgentId[] }   // кто сейчас работает
```

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
| POST | `/api/rooms/{id}/messages` | `{text}` | `Message` (сообщение пользователя) |
| POST | `/api/rooms/{id}/stop` | — | `{ok}` — остановить всех агентов комнаты |
| PUT | `/api/rooms/{id}/target` | `{target}` | `{ok}` |

Ошибка: HTTP 400 `{error: string}`.

## События: WebSocket `ws://127.0.0.1:7878/api/events`

Один сокет на все комнаты, JSON по одному событию в сообщении:

```ts
{ type: "message",      room_id: string, message: Message }
{ type: "agent_status", room_id: string, agent: AgentId, busy: boolean }
{ type: "room_updated", room: Room }
{ type: "room_deleted", room_id: string }
{ type: "delta",        room_id: string, agent: AgentId, text: string }
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
