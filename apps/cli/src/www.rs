use std::sync::Arc;

use anyhow::Result;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tower_mcp::transport::http::HttpTransport;
use trivia_core::{AuthBackend, Embedder, MemoryBackend, TriviaConfig};

use crate::acl::Acl;
use crate::auth_middleware::{AuthState, require_auth};
use crate::oauth::{self, OAuthState};

static WWW_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/www/dist");

/// Normalize a base path: empty stays empty, otherwise ensure leading `/` and
/// strip trailing `/`. So `""`, `"/"`, `"trivia"`, and `"/trivia/"` all produce
/// either `""` or `"/trivia"`.
pub fn normalize_base_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

struct AppState {
    store: Arc<dyn MemoryBackend>,
    embedder: Arc<Mutex<Embedder>>,
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

pub async fn serve(
    memory: Arc<dyn MemoryBackend>,
    auth: Arc<dyn AuthBackend>,
    embedder: Embedder,
    bind_addr: &str,
    base_path: &str,
    config: TriviaConfig,
    acl: Acl,
) -> Result<()> {
    let embedder = Arc::new(Mutex::new(embedder));
    let base_path = base_path.to_string();

    // Determine external URL for OAuth redirects
    let external_url = config
        .external_url
        .clone()
        .unwrap_or_else(|| format!("http://{bind_addr}"));

    // Check if auth providers are configured
    let auth_enabled = auth.has_auth_providers().await.unwrap_or(false);

    let state = Arc::new(AppState {
        store: memory.clone(),
        embedder: embedder.clone(),
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
        .route("/api/tags", get(list_tags))
        .route("/api/links", post(create_link).delete(remove_link))
        .route(
            "/api/memories/{mnemonic}/mnemonics",
            post(add_mnemonic_handler).delete(remove_mnemonic_handler),
        );

    // Mount MCP over HTTP at /mcp
    let acl = Arc::new(acl);
    let mcp_router = crate::mcp::build_mcp_router(
        memory.clone(),
        embedder,
        config.clone(),
        acl.clone(),
    );
    // Auto-reinitialize unknown/stale session IDs instead of returning
    // JSON-RPC -32005 SessionNotFound. Without this, a client reconnecting
    // with a session ID we no longer hold (server restart, session expiry,
    // or eviction) gets hard-failed. Per-user authorization rides on the
    // OAuth bearer token via `require_auth`, not the MCP session identity,
    // so recovering the session with synthetic client info is safe here.
    let mcp = HttpTransport::new(mcp_router)
        .disable_origin_validation()
        .auto_reinitialize_sessions(true)
        .into_router_at("/mcp");

    // Auth middleware state
    let auth_state = AuthState {
        store: auth.clone(),
        external_url: external_url.clone(),
        fallback_acl: acl.to_string(),
        auth_enabled,
    };

    // OAuth routes (always public, no auth middleware)
    let oauth_state = OAuthState {
        store: auth.clone(),
        external_url: external_url.clone(),
        base_path: base_path.clone(),
    };
    let oauth_routes = oauth::router().with_state(oauth_state);

    // Protected routes: API + MCP get auth middleware when auth is enabled
    let protected = api
        .with_state(state)
        .merge(mcp)
        .layer(middleware::from_fn_with_state(
            auth_state.clone(),
            require_auth,
        ));

    let acl_desc = if auth_enabled {
        "OAuth (per-user ACL)"
    } else if acl.is_open() {
        "open (all tools allowed)"
    } else {
        "restricted by --share ACL"
    };
    eprintln!("MCP endpoint at {base_path}/mcp ({acl_desc})");
    if auth_enabled {
        eprintln!("Auth enabled — OAuth providers configured");
    }

    let static_base = base_path.clone();
    let make_static_svc = || {
        let bp = static_base.clone();
        get(move |uri: axum::http::Uri| {
            let bp = bp.clone();
            async move { static_handler(uri, bp).await }
        })
    };
    let inner = protected
        .merge(oauth_routes)
        .fallback(make_static_svc());

    let app = if base_path.is_empty() {
        inner
    } else {
        // `Router::nest("/trivia", inner)` matches `/trivia` and
        // `/trivia/<rest>`, but not the bare `/trivia/` (axum's nested
        // catch-all requires a non-empty `rest`). Catch that case explicitly.
        Router::new()
            .route(&format!("{base_path}/"), make_static_svc())
            .nest(&base_path, inner)
    }
    .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    eprintln!("Listening on http://{bind_addr}{base_path}");
    axum::serve(listener, app).await?;
    Ok(())
}

// --- API handlers ---

async fn list_memories(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let summaries = state.store.list_all_summaries().await?;
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
    let mut embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&body.mnemonic)?;
    drop(embedder);
    state.store.memorize(&body.mnemonic, &body.content, &body.tags, &embedding).await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({"ok": true}))))
}

async fn get_memory(
    State(state): State<Arc<AppState>>,
    Path(mnemonic): Path<String>,
) -> AppResult<Response> {
    match state.store.get_memory_by_mnemonic(&mnemonic).await? {
        Some(mem) => Ok(axum::Json(mem).into_response()),
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

#[derive(Deserialize)]
struct UpdateMemoryReq {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    /// If set, rename the mnemonic
    mnemonic: Option<String>,
}

async fn update_memory(
    State(state): State<Arc<AppState>>,
    Path(old_mnemonic): Path<String>,
    axum::Json(body): axum::Json<UpdateMemoryReq>,
) -> AppResult<Response> {
    let new_mnemonic = body.mnemonic.as_deref().unwrap_or(&old_mnemonic);
    let renaming = new_mnemonic != old_mnemonic;

    let mut embedder = state.embedder.lock().await;
    let embedding = embedder.embed(new_mnemonic)?;
    drop(embedder);

    if renaming {
        state.store.rename_memory(&old_mnemonic, new_mnemonic, &embedding).await?;
    }
    state.store.update_memory(new_mnemonic, &body.content, &body.tags, &embedding).await?;

    if renaming {
        Ok(axum::Json(serde_json::json!({"ok": true, "mnemonic": new_mnemonic})).into_response())
    } else {
        Ok(axum::Json(serde_json::json!({"ok": true})).into_response())
    }
}

async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Path(mnemonic): Path<String>,
) -> AppResult<impl IntoResponse> {
    let deleted = state.store.delete_memory(&mnemonic).await?;
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
    state.store.rate(&mnemonic, body.useful).await?;
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mnemonics: Vec<String>,
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
    let summaries = state.store.list_all_summaries().await?;
    let links = state.store.get_all_links().await?;

    let nodes: Vec<GraphNode> = summaries
        .into_iter()
        .map(|s| GraphNode {
            mnemonic: s.mnemonic,
            content: s.content,
            tags: s.tags,
            mnemonics: s.mnemonics,
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
    /// Comma-separated tag filter
    #[serde(default)]
    tags: Option<String>,
}

fn default_limit() -> usize {
    10
}

async fn search_memories(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> AppResult<impl IntoResponse> {
    let mut embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&params.q)?;
    drop(embedder);
    let tag_list: Option<Vec<String>> = params
        .tags
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect());
    let results = state
        .store
        .recall(&embedding, params.limit, tag_list.as_deref(), None, None)
        .await?;
    Ok(axum::Json(results))
}

async fn list_tags(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let tags = state.store.list_tags().await?;
    Ok(axum::Json(tags))
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
    let mut embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&body.keep)?;
    drop(embedder);
    state.store.merge(&body.keep, &body.discard, &embedding).await?;
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
    state.store.link(&body.source, &body.target, &body.link_type).await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({"ok": true}))))
}

async fn remove_link(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<LinkReq>,
) -> AppResult<impl IntoResponse> {
    state.store.unlink(&body.source, &body.target, &body.link_type).await?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

#[derive(Deserialize)]
struct MnemonicReq {
    text: String,
}

async fn add_mnemonic_handler(
    State(state): State<Arc<AppState>>,
    Path(title): Path<String>,
    axum::Json(body): axum::Json<MnemonicReq>,
) -> AppResult<impl IntoResponse> {
    let mut embedder = state.embedder.lock().await;
    let embedding = embedder.embed(&body.text)?;
    drop(embedder);
    state.store.add_mnemonic(&title, &body.text, &embedding).await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({"ok": true}))))
}

async fn remove_mnemonic_handler(
    State(state): State<Arc<AppState>>,
    Path(title): Path<String>,
    axum::Json(body): axum::Json<MnemonicReq>,
) -> AppResult<impl IntoResponse> {
    state.store.remove_mnemonic(&title, &body.text).await?;
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

async fn static_handler(uri: axum::http::Uri, base_path: String) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try exact file first (non-HTML assets are served as-is)
    if let Some(file) = WWW_DIR.get_file(path) {
        let ext = path.rsplit('.').next().unwrap_or("");
        if ext == "html" {
            return html_response(file.contents(), &base_path);
        }
        return (
            [(header::CONTENT_TYPE, mime_from_ext(ext))],
            file.contents(),
        )
            .into_response();
    }

    // SPA fallback: serve index.html
    match WWW_DIR.get_file("index.html") {
        Some(file) => html_response(file.contents(), &base_path),
        None => (StatusCode::NOT_FOUND, "frontend not built — run: cd apps/cli/www && npm run build").into_response(),
    }
}

/// Rewrite an HTML payload so absolute `/assets/...` URLs and the embedded
/// `__TRIVIA_BASE__` global match the configured base path.
fn html_response(bytes: &[u8], base_path: &str) -> Response {
    let raw = std::str::from_utf8(bytes).unwrap_or("");
    let body = if base_path.is_empty() {
        // Still inject the global so the SPA can read a consistent value.
        inject_base_script(raw, "")
    } else {
        let rewritten = raw
            .replace("=\"/assets/", &format!("=\"{base_path}/assets/"))
            .replace("='/assets/", &format!("='{base_path}/assets/"));
        inject_base_script(&rewritten, base_path)
    };
    Html(body).into_response()
}

fn inject_base_script(html: &str, base_path: &str) -> String {
    let snippet = format!(
        "<script>window.__TRIVIA_BASE__={};</script>",
        serde_json::to_string(base_path).unwrap_or_else(|_| "\"\"".to_string()),
    );
    if let Some(idx) = html.find("</head>") {
        let mut out = String::with_capacity(html.len() + snippet.len());
        out.push_str(&html[..idx]);
        out.push_str(&snippet);
        out.push_str(&html[idx..]);
        out
    } else {
        format!("{snippet}{html}")
    }
}
