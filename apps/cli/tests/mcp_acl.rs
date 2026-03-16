use std::sync::{Arc, LazyLock};

use axum::body::Body;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower::ServiceExt;
use tower_mcp::transport::http::HttpTransport;

use trivia_cli::acl::Acl;
use trivia_cli::mcp::build_mcp_router;
use trivia_core::{Embedder, MemoryStore, TriviaConfig};

// Shared embedder — model loading is expensive, do it once across all tests.
static EMBEDDER: LazyLock<Arc<Mutex<Embedder>>> = LazyLock::new(|| {
    Arc::new(Mutex::new(Embedder::new().unwrap()))
});

/// Build an MCP HTTP app with the given ACL.
fn test_app(acl: Acl) -> (axum::Router, Arc<Mutex<MemoryStore>>) {
    let store = Arc::new(Mutex::new(MemoryStore::in_memory().unwrap()));
    let mcp = build_mcp_router(
        store.clone(),
        EMBEDDER.clone(),
        TriviaConfig::default(),
        Arc::new(acl),
    );
    let router = HttpTransport::new(mcp)
        .disable_origin_validation()
        .into_router();
    (router, store)
}

/// Seed memories with distinct tags for ACL testing.
/// Uses real embeddings so recall KNN actually works.
async fn seed(store: &Arc<Mutex<MemoryStore>>) {
    let e = EMBEDDER.lock().await;
    let emb1 = e.embed("test fact").unwrap();
    let emb2 = e.embed("private fact").unwrap();
    let emb3 = e.embed("project fact").unwrap();
    drop(e);

    let s = store.lock().await;
    s.memorize("test fact", "hello world", &["test".into()], &emb1)
        .unwrap();
    s.memorize("private fact", "secret stuff", &["private".into()], &emb2)
        .unwrap();
    s.memorize(
        "project fact",
        "project data",
        &["project".into()],
        &emb3,
    )
    .unwrap();
}

/// POST a JSON-RPC request. Returns (parsed response, session ID).
async fn post(app: &axum::Router, session: Option<&str>, body: Value) -> (Value, String) {
    let mut req = axum::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json");
    if let Some(sid) = session {
        req = req.header("mcp-session-id", sid);
    }
    let req = req.body(Body::from(body.to_string())).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    if text.is_empty() {
        return (Value::Null, sid);
    }
    (parse_response(&text), sid)
}

/// Parse JSON from plain JSON or SSE-wrapped response.
fn parse_response(text: &str) -> Value {
    if text.trim_start().starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            return v;
        }
    }
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                return v;
            }
        }
    }
    panic!("Could not parse MCP response:\n{text}");
}

/// Initialize an MCP session, returning the session ID.
async fn init(app: &axum::Router) -> String {
    let (_, sid) = post(
        app,
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0.1"},
            },
        }),
    )
    .await;
    assert!(!sid.is_empty(), "missing session ID from initialize");

    // Send initialized notification
    let _ = post(
        app,
        Some(&sid),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }),
    )
    .await;
    sid
}

/// Call an MCP tool, returning the JSON-RPC response.
async fn call_tool(app: &axum::Router, sid: &str, tool: &str, args: Value) -> Value {
    let (resp, _) = post(
        app,
        Some(sid),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args},
        }),
    )
    .await;
    resp
}

/// Extract text content from a successful tools/call result.
fn result_text(resp: &Value) -> &str {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected text content in result: {resp}"))
}

/// Check if the response is an error (JSON-RPC error or tool isError).
fn is_error(resp: &Value) -> bool {
    resp.get("error").is_some() || resp["result"]["isError"].as_bool().unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn open_acl_memorize_and_recall() {
    let (app, _) = test_app(Acl::open());
    let sid = init(&app).await;

    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "integration test", "content": "this is a test memory", "tags": ["test"]}),
    )
    .await;
    assert!(!is_error(&resp), "memorize should succeed: {resp}");
    assert!(result_text(&resp).contains("Memorized"));

    let resp = call_tool(&app, &sid, "recall", json!({"query": "integration test"})).await;
    assert!(!is_error(&resp), "recall should succeed: {resp}");
    assert!(
        result_text(&resp).contains("integration test"),
        "should find the memory"
    );
}

#[tokio::test]
async fn read_only_share_filters_recall() {
    let (app, store) = test_app(Acl::parse("test:read,*:none").unwrap());
    seed(&store).await;
    let sid = init(&app).await;

    let resp = call_tool(&app, &sid, "recall", json!({"query": "fact"})).await;
    assert!(!is_error(&resp), "recall failed: {resp}");
    let text = result_text(&resp);
    assert!(text.contains("test fact"), "should see test-tagged memory, got: {text}");
    assert!(
        !text.contains("private fact"),
        "should NOT see private-tagged memory"
    );
    assert!(
        !text.contains("project fact"),
        "should NOT see project-tagged memory"
    );
}

#[tokio::test]
async fn read_only_share_denies_memorize() {
    let (app, store) = test_app(Acl::parse("test:read,*:none").unwrap());
    seed(&store).await;
    let sid = init(&app).await;

    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "new fact", "content": "should be denied", "tags": ["test"]}),
    )
    .await;
    assert!(
        is_error(&resp),
        "memorize should be denied in read-only mode: {resp}"
    );
}

#[tokio::test]
async fn read_only_share_filters_list_tags() {
    let (app, store) = test_app(Acl::parse("test:read,*:none").unwrap());
    seed(&store).await;
    let sid = init(&app).await;

    let resp = call_tool(&app, &sid, "list-tags", json!({})).await;
    assert!(!is_error(&resp));
    let text = result_text(&resp);
    assert!(text.contains("test"), "should see 'test' tag");
    assert!(!text.contains("private"), "should NOT see 'private' tag");
    assert!(!text.contains("project"), "should NOT see 'project' tag");
}

#[tokio::test]
async fn mixed_share_recall_sees_all() {
    let (app, store) = test_app(Acl::parse("project:update,*:read").unwrap());
    seed(&store).await;
    let sid = init(&app).await;

    let resp = call_tool(&app, &sid, "recall", json!({"query": "fact"})).await;
    assert!(!is_error(&resp));
    let text = result_text(&resp);
    assert!(text.contains("test fact"), "should see test memory");
    assert!(text.contains("private fact"), "should see private memory");
    assert!(text.contains("project fact"), "should see project memory");
}

#[tokio::test]
async fn mixed_share_memorize_with_update_tag() {
    let (app, _) = test_app(Acl::parse("project:update,*:read").unwrap());
    let sid = init(&app).await;

    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "new project note", "content": "important", "tags": ["project"]}),
    )
    .await;
    assert!(
        !is_error(&resp),
        "memorize with update-level tag should succeed: {resp}"
    );
}

#[tokio::test]
async fn mixed_share_memorize_without_update_tag_denied() {
    let (app, _) = test_app(Acl::parse("project:update,*:read").unwrap());
    let sid = init(&app).await;

    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "general note", "content": "should fail", "tags": ["general"]}),
    )
    .await;
    assert!(
        is_error(&resp),
        "memorize without update-level tag should be denied: {resp}"
    );
}

#[tokio::test]
async fn shared_mode_blocks_import() {
    let (app, _) = test_app(Acl::parse("test:update,*:read").unwrap());
    let sid = init(&app).await;

    let resp = call_tool(
        &app,
        &sid,
        "import",
        json!({"directory": "/tmp/nonexistent"}),
    )
    .await;
    assert!(
        is_error(&resp),
        "import should be blocked in shared mode: {resp}"
    );
}

#[tokio::test]
async fn shared_mode_suppresses_merge_info() {
    let (app, _) = test_app(Acl::parse("test:update,*:read").unwrap());
    let sid = init(&app).await;

    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "merge candidate alpha", "content": "first version", "tags": ["test"]}),
    )
    .await;
    assert!(!is_error(&resp));
    let text = result_text(&resp);
    assert!(
        !text.contains("Nearby"),
        "should suppress neighbor info in shared mode"
    );

    // Second memorize with similar mnemonic should NOT auto-merge
    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "merge candidate alpha v2", "content": "second version", "tags": ["test"]}),
    )
    .await;
    assert!(!is_error(&resp));
    let text = result_text(&resp);
    assert!(
        !text.contains("merged"),
        "should NOT auto-merge in shared mode"
    );
    assert!(
        text.starts_with("Memorized:"),
        "should just report memorized"
    );
}

#[tokio::test]
async fn closed_acl_denies_everything() {
    let (app, store) = test_app(Acl::closed());
    seed(&store).await;
    let sid = init(&app).await;

    // Recall returns nothing (all filtered)
    let resp = call_tool(&app, &sid, "recall", json!({"query": "fact"})).await;
    assert!(!is_error(&resp));
    assert_eq!(result_text(&resp), "No memories found.");

    // Memorize denied
    let resp = call_tool(
        &app,
        &sid,
        "memorize",
        json!({"mnemonic": "denied", "content": "nope", "tags": ["anything"]}),
    )
    .await;
    assert!(
        is_error(&resp),
        "memorize should be denied with closed ACL: {resp}"
    );

    // List-tags returns nothing
    let resp = call_tool(&app, &sid, "list-tags", json!({})).await;
    assert!(!is_error(&resp));
    assert_eq!(result_text(&resp), "No tags found.");
}
