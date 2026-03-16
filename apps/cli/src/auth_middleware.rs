use std::sync::Arc;

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::Mutex;
use tower_mcp::oauth::token::TokenClaims;
use trivia_core::MemoryStore;

use crate::acl::Acl;
use crate::oauth;

/// State needed by the auth middleware.
#[derive(Clone)]
pub struct AuthState {
    pub store: Arc<Mutex<MemoryStore>>,
    pub external_url: String,
    /// Fallback ACL when auth is disabled (from --share flag).
    pub fallback_acl: String,
    /// If true, auth is required on non-public routes.
    pub auth_enabled: bool,
}

/// Build a default `TokenClaims` from an ACL (for stdio/test, no HTTP middleware).
pub fn default_claims(acl: &Acl) -> TokenClaims {
    make_claims(None, &acl.to_string())
}

/// Build a `TokenClaims` carrying username and ACL for bridging into MCP.
fn make_claims(username: Option<String>, acl: &str) -> TokenClaims {
    let mut extra = std::collections::HashMap::new();
    extra.insert("acl".into(), serde_json::Value::String(acl.into()));
    TokenClaims {
        sub: username,
        iss: None,
        aud: None,
        exp: None,
        scope: None,
        client_id: None,
        extra,
    }
}

/// Resolve ACL + username from `TokenClaims`.
pub fn acl_from_claims(claims: &TokenClaims, fallback: &Acl) -> (Arc<Acl>, Option<String>) {
    let username = claims.sub.clone();
    let acl_str = claims.extra.get("acl").and_then(|v| v.as_str());
    match acl_str {
        Some(s) => (Arc::new(Acl::parse(s).unwrap_or_else(|_| Acl::closed())), username),
        None => (Arc::new(fallback.clone()), username),
    }
}

/// Middleware that enforces authentication when auth is enabled.
/// Inserts `TokenClaims` into HTTP request extensions so tower-mcp
/// bridges them into MCP `RequestContext` for tool handlers.
pub async fn require_auth(
    axum::extract::State(auth_state): axum::extract::State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    if !auth_state.auth_enabled {
        // No auth — insert default claims with fallback ACL
        request
            .extensions_mut()
            .insert(make_claims(None, &auth_state.fallback_acl));
        return next.run(request).await;
    }

    let headers = request.headers().clone();
    let oauth_state = oauth::OAuthState {
        store: auth_state.store.clone(),
        external_url: auth_state.external_url.clone(),
    };

    // Try Bearer token
    if let Ok(Some(user)) = oauth::extract_bearer_user(&oauth_state, &headers).await {
        request
            .extensions_mut()
            .insert(make_claims(Some(user.username), &user.acl));
        return next.run(request).await;
    }

    // Try session cookie
    if let Some(session_id) = oauth::extract_session_cookie(&headers) {
        let store = auth_state.store.lock().await;
        if let Ok(Some((_sess, user))) = store.get_session(&session_id) {
            drop(store);
            request
                .extensions_mut()
                .insert(make_claims(Some(user.username), &user.acl));
            return next.run(request).await;
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({ "error": "authentication required" })),
    )
        .into_response()
}
