use std::sync::Arc;

use anyhow::Result;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use trivia_core::{Embedder, MemoryStore};

static WWW_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/www/dist");

struct AppState {
    store: tokio::sync::Mutex<MemoryStore>,
    embedder: tokio::sync::Mutex<Embedder>,
}

type AppResult<T> = std::result::Result<T, AppError>;

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

pub async fn serve(store: MemoryStore, embedder: Embedder, port: u16) -> Result<()> {
    let state = Arc::new(AppState {
        store: tokio::sync::Mutex::new(store),
        embedder: tokio::sync::Mutex::new(embedder),
    });

    let api = Router::new()
        .route("/api/memories/merge", post(merge_memories))
        .route("/api/memories/{mnemonic}/rate", post(rate_memory))
        .route("/api/memories", get(list_memories).post(create_memory))
        .route(
            "/api/memories/{mnemonic}",
            get(get_memory).put(update_memory).delete(delete_memory),
        )
        .route("/api/graph", get(get_graph))
        .route("/api/search", get(search_memories))
        .route("/api/links", post(create_link).delete(remove_link));

    let app = api
        .fallback(get(static_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    eprintln!("Listening on http://localhost:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

// --- API handlers ---

async fn list_memories(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let store = state.store.lock().await;
    let summaries = store.list_all_summaries()?;
    Ok(axum::Json(summaries))
}

#[derive(Deserialize)]
struct CreateMemoryReq {
    mnemonic: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

async fn create_memory(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<CreateMemoryReq>,
) -> AppResult<impl IntoResponse> {
    let embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&body.mnemonic)?;
    drop(embedder);
    let store = state.store.lock().await;
    store.memorize(&body.mnemonic, &body.content, &body.tags, &embedding)?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({"ok": true}))))
}

async fn get_memory(
    State(state): State<Arc<AppState>>,
    Path(mnemonic): Path<String>,
) -> AppResult<Response> {
    let store = state.store.lock().await;
    match store.get_memory_by_mnemonic(&mnemonic)? {
        Some(mem) => Ok(axum::Json(mem).into_response()),
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

#[derive(Deserialize)]
struct UpdateMemoryReq {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

async fn update_memory(
    State(state): State<Arc<AppState>>,
    Path(mnemonic): Path<String>,
    axum::Json(body): axum::Json<UpdateMemoryReq>,
) -> AppResult<impl IntoResponse> {
    let embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&mnemonic)?;
    drop(embedder);
    let store = state.store.lock().await;
    store.update_memory(&mnemonic, &body.content, &body.tags, &embedding)?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Path(mnemonic): Path<String>,
) -> AppResult<impl IntoResponse> {
    let store = state.store.lock().await;
    let deleted = store.delete_memory(&mnemonic)?;
    if deleted {
        Ok(axum::Json(serde_json::json!({"ok": true})).into_response())
    } else {
        Ok(StatusCode::NOT_FOUND.into_response())
    }
}

#[derive(Deserialize)]
struct RateReq {
    useful: bool,
}

async fn rate_memory(
    State(state): State<Arc<AppState>>,
    Path(mnemonic): Path<String>,
    axum::Json(body): axum::Json<RateReq>,
) -> AppResult<impl IntoResponse> {
    let store = state.store.lock().await;
    store.rate(&mnemonic, body.useful)?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

#[derive(Serialize)]
struct GraphResponse {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Serialize)]
struct GraphNode {
    mnemonic: String,
    content: String,
    tags: Vec<String>,
    recall_count: i64,
    useful_count: i64,
    not_useful_count: i64,
}

#[derive(Serialize)]
struct GraphEdge {
    source: String,
    target: String,
    link_type: String,
}

async fn get_graph(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let store = state.store.lock().await;
    let summaries = store.list_all_summaries()?;
    let links = store.get_all_links()?;

    let nodes: Vec<GraphNode> = summaries
        .into_iter()
        .map(|s| GraphNode {
            mnemonic: s.mnemonic,
            content: s.content,
            tags: s.tags,
            recall_count: s.recall_count,
            useful_count: s.useful_count,
            not_useful_count: s.not_useful_count,
        })
        .collect();

    let edges: Vec<GraphEdge> = links
        .into_iter()
        .map(|l| GraphEdge {
            source: l.source_mnemonic,
            target: l.target_mnemonic,
            link_type: l.link_type,
        })
        .collect();

    Ok(axum::Json(GraphResponse { nodes, edges }))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

async fn search_memories(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> AppResult<impl IntoResponse> {
    let embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&params.q)?;
    drop(embedder);
    let store = state.store.lock().await;
    let results = store.recall(&embedding, params.limit, None)?;
    Ok(axum::Json(results))
}

#[derive(Deserialize)]
struct MergeReq {
    keep: String,
    discard: String,
}

async fn merge_memories(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<MergeReq>,
) -> AppResult<impl IntoResponse> {
    let embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&body.keep)?;
    drop(embedder);
    let store = state.store.lock().await;
    store.merge(&body.keep, &body.discard, &embedding)?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

#[derive(Deserialize)]
struct LinkReq {
    source: String,
    target: String,
    #[serde(default = "default_link_type")]
    link_type: String,
}

fn default_link_type() -> String {
    "related".to_string()
}

async fn create_link(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<LinkReq>,
) -> AppResult<impl IntoResponse> {
    let store = state.store.lock().await;
    store.link(&body.source, &body.target, &body.link_type)?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({"ok": true}))))
}

async fn remove_link(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<LinkReq>,
) -> AppResult<impl IntoResponse> {
    let store = state.store.lock().await;
    store.unlink(&body.source, &body.target, &body.link_type)?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

// --- Static file serving ---

fn mime_from_ext(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html",
        "js" => "application/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try exact file first
    if let Some(file) = WWW_DIR.get_file(path) {
        let ext = path.rsplit('.').next().unwrap_or("");
        return (
            [(header::CONTENT_TYPE, mime_from_ext(ext))],
            file.contents(),
        )
            .into_response();
    }

    // SPA fallback: serve index.html
    match WWW_DIR.get_file("index.html") {
        Some(file) => Html(std::str::from_utf8(file.contents()).unwrap_or("")).into_response(),
        None => (StatusCode::NOT_FOUND, "frontend not built — run: cd apps/cli/www && npm run build").into_response(),
    }
}
