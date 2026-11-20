//! HTTP + WebSocket API Mecode. Используется и отдельным бинарником `mecode`, и Tauri-оболочкой.
//!
//! Сервер слушает только 127.0.0.1 и отклоняет запросы с чужих сайтов: агенты выполняют команды
//! на машине, поэтому любая веб-страница не должна уметь писать в чат.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use mecode_core::{Hub, Target};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;

/// Источники, которым разрешено обращаться к API (кроме самого сервера).
const TAURI_ORIGINS: [&str; 3] = ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];

/// Папка данных: `%APPDATA%\mecode` на Windows, `~/.config/mecode` на Linux.
pub fn default_data_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("mecode")
}

pub fn router(hub: Arc<Hub>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(TAURI_ORIGINS.map(|o| HeaderValue::from_static(o)))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE])
        .allow_headers([header::CONTENT_TYPE]);

    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/rooms", get(list_rooms).post(create_room))
        .route("/api/rooms/{id}", get(get_room).patch(rename_room).delete(delete_room))
        .route("/api/rooms/{id}/messages", get(messages).post(send))
        .route("/api/rooms/{id}/stop", post(stop))
        .route("/api/rooms/{id}/target", put(set_target))
        .route("/api/events", get(events))
        .layer(middleware::from_fn(guard))
        .layer(cors)
        .with_state(hub)
}

/// Запускает сервер на 127.0.0.1:<port>. Возвращает фактический адрес (port 0 = любой свободный).
pub async fn serve(hub: Arc<Hub>, port: u16) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let app = router(hub);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("сервер остановился: {e}");
        }
    });
    Ok((addr, handle))
}

/// Защита от DNS rebinding и запросов с чужих страниц.
async fn guard(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let host_name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    if !matches!(host_name, "127.0.0.1" | "localhost" | "[::1]") {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        let same = origin == format!("http://{host}");
        if !same && !TAURI_ORIGINS.contains(&origin) {
            return (StatusCode::FORBIDDEN, "bad origin").into_response();
        }
    }
    next.run(req).await
}

struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("{:#}", self.0) }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

/// Проверяет, что CLI агентов установлены.
async fn health(State(hub): State<Arc<Hub>>) -> Json<serde_json::Value> {
    async fn version(bin: &str) -> Option<String> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("--version");
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let out = cmd.output().await.ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
    let cfg = hub.config();
    let (claude, codex) = tokio::join!(version(&cfg.claude.bin), version(&cfg.codex.bin));
    Json(json!({ "ok": true, "claude": claude, "codex": codex }))
}

async fn list_rooms(State(hub): State<Arc<Hub>>) -> Json<Vec<mecode_core::hub::RoomView>> {
    Json(hub.list_rooms())
}

#[derive(Deserialize)]
struct CreateRoom {
    #[serde(default)]
    title: String,
    workspace: String,
}

async fn create_room(State(hub): State<Arc<Hub>>, Json(body): Json<CreateRoom>) -> ApiResult<mecode_core::Room> {
    Ok(Json(hub.create_room(&body.title, &body.workspace)?))
}

async fn get_room(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> ApiResult<mecode_core::hub::RoomView> {
    Ok(Json(hub.room(&id)?))
}

#[derive(Deserialize)]
struct Rename {
    title: String,
}

async fn rename_room(State(hub): State<Arc<Hub>>, Path(id): Path<String>, Json(body): Json<Rename>) -> ApiResult<mecode_core::Room> {
    Ok(Json(hub.rename(&id, &body.title)?))
}

async fn delete_room(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> ApiResult<serde_json::Value> {
    hub.delete(&id)?;
    Ok(Json(json!({ "ok": true })))
}

async fn messages(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> ApiResult<Vec<mecode_core::Message>> {
    Ok(Json(hub.messages(&id)?))
}

#[derive(Deserialize)]
struct Send {
    text: String,
}

async fn send(State(hub): State<Arc<Hub>>, Path(id): Path<String>, Json(body): Json<Send>) -> ApiResult<mecode_core::Message> {
    Ok(Json(hub.send(&id, &body.text)?))
}

async fn stop(State(hub): State<Arc<Hub>>, Path(id): Path<String>) -> ApiResult<serde_json::Value> {
    hub.stop(&id)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SetTarget {
    target: Target,
}

async fn set_target(State(hub): State<Arc<Hub>>, Path(id): Path<String>, Json(body): Json<SetTarget>) -> ApiResult<serde_json::Value> {
    hub.set_target(&id, body.target)?;
    Ok(Json(json!({ "ok": true })))
}

async fn events(State(hub): State<Arc<Hub>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| stream_events(hub, socket))
}

async fn stream_events(hub: Arc<Hub>, mut socket: WebSocket) {
    let mut rx = hub.subscribe();
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    let Ok(text) = serde_json::to_string(&ev) else { continue };
                    if socket.send(WsMessage::Text(text.into())).await.is_err() { break; }
                }
                // Клиент отстал: просим его перечитать состояние через REST.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let resync = json!({ "type": "resync" }).to_string();
                    if socket.send(WsMessage::Text(resync.into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}
