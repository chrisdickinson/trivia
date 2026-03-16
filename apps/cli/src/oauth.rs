use std::sync::Arc;

use anyhow::Result;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use trivia_core::MemoryStore;

use crate::providers::Provider;

pub type SharedStore = Arc<Mutex<MemoryStore>>;

#[derive(Clone)]
pub struct OAuthState {
    pub store: SharedStore,
    pub external_url: String,
}

pub fn router() -> Router<OAuthState> {
    Router::new()
        // Discovery
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata),
        )
        // DCR
        .route("/oauth/register", post(register_client))
        // Authorization flow
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/callback/{provider}", get(oauth_callback))
        .route("/oauth/token", post(token_exchange))
        // Web UI auth
        .route("/auth/login/{provider}", get(auth_login))
        .route("/auth/callback/{provider}", get(auth_callback))
        .route("/auth/logout", post(auth_logout))
        .route("/auth/me", get(auth_me))
        .route("/auth/providers", get(list_providers))
}

// --- Discovery ---

#[derive(Serialize)]
struct ServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
}

async fn server_metadata(State(state): State<OAuthState>) -> impl IntoResponse {
    let base = &state.external_url;
    axum::Json(ServerMetadata {
        issuer: base.clone(),
        authorization_endpoint: format!("{base}/oauth/authorize"),
        token_endpoint: format!("{base}/oauth/token"),
        registration_endpoint: format!("{base}/oauth/register"),
        response_types_supported: vec!["code".into()],
        grant_types_supported: vec!["authorization_code".into(), "refresh_token".into()],
        code_challenge_methods_supported: vec!["S256".into()],
    })
}

// --- DCR ---

#[derive(Deserialize)]
struct RegisterRequest {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
}

#[derive(Serialize)]
struct RegisterResponse {
    client_id: String,
    client_secret: Option<String>,
    redirect_uris: Vec<String>,
    client_name: Option<String>,
}

async fn register_client(
    State(state): State<OAuthState>,
    axum::Json(body): axum::Json<RegisterRequest>,
) -> Result<impl IntoResponse, AppError> {
    if body.redirect_uris.is_empty() {
        return Err(AppError::bad_request("redirect_uris must not be empty"));
    }

    let store = state.store.lock().await;
    let (client, secret) = store.register_client(&body.redirect_uris, body.client_name.as_deref())?;

    Ok((
        StatusCode::CREATED,
        axum::Json(RegisterResponse {
            client_id: client.client_id,
            client_secret: secret,
            redirect_uris: client.redirect_uris,
            client_name: client.client_name,
        }),
    ))
}

// --- Authorization ---

#[derive(Deserialize)]
struct AuthorizeParams {
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: Option<String>,
    response_type: Option<String>,
    /// Which provider to use (defaults to first available)
    provider: Option<String>,
}

async fn authorize(
    State(state): State<OAuthState>,
    Query(params): Query<AuthorizeParams>,
) -> Result<Response, AppError> {
    // Validate response_type
    if params.response_type.as_deref().unwrap_or("code") != "code" {
        return Err(AppError::bad_request("unsupported response_type"));
    }
    if params
        .code_challenge_method
        .as_deref()
        .unwrap_or("S256")
        != "S256"
    {
        return Err(AppError::bad_request(
            "unsupported code_challenge_method (only S256)",
        ));
    }

    let store = state.store.lock().await;

    // Validate client
    let client = store
        .get_client(&params.client_id)?
        .ok_or_else(|| AppError::bad_request("unknown client_id"))?;
    if !client.redirect_uris.contains(&params.redirect_uri) {
        return Err(AppError::bad_request("redirect_uri not registered"));
    }

    // Find provider
    let providers = store.list_providers()?;
    let db_provider = if let Some(name) = &params.provider {
        providers
            .iter()
            .find(|p| p.name == *name && p.enabled)
            .ok_or_else(|| AppError::bad_request("provider not found or disabled"))?
    } else {
        providers
            .iter()
            .find(|p| p.enabled)
            .ok_or_else(|| AppError::bad_request("no OAuth providers configured"))?
    };

    let provider = Provider::from_db(db_provider)?;
    drop(store);

    // Build OAuth state that encodes our pending authorization
    // Format: <random>:<client_id>:<redirect_uri>:<code_challenge>:<original_state>
    let oauth_state = format!(
        "{}:{}:{}:{}:{}",
        trivia_core::auth_store::sha256_hex(&format!("{}{}", params.client_id, params.state)),
        params.client_id,
        base64_encode(&params.redirect_uri),
        params.code_challenge,
        params.state,
    );

    let callback_uri = format!(
        "{}/oauth/callback/{}",
        state.external_url, db_provider.name
    );
    let auth_url = provider.authorize_url(&oauth_state, &callback_uri);

    Ok(Redirect::temporary(&auth_url).into_response())
}

#[derive(Deserialize)]
struct CallbackParams {
    code: String,
    state: String,
}

async fn oauth_callback(
    State(state): State<OAuthState>,
    Path(provider_name): Path<String>,
    Query(params): Query<CallbackParams>,
) -> Result<Response, AppError> {
    // Parse compound state
    let parts: Vec<&str> = params.state.splitn(5, ':').collect();
    if parts.len() != 5 {
        return Err(AppError::bad_request("invalid state parameter"));
    }
    let (_hash, client_id, redirect_uri_b64, code_challenge, original_state) =
        (parts[0], parts[1], parts[2], parts[3], parts[4]);
    let redirect_uri = base64_decode(redirect_uri_b64)
        .map_err(|_| AppError::bad_request("invalid state encoding"))?;

    let store = state.store.lock().await;

    // Load provider
    let db_provider = store
        .get_provider_by_name(&provider_name)?
        .ok_or_else(|| AppError::bad_request("unknown provider"))?;
    let provider = Provider::from_db(&db_provider)?;

    let callback_uri = format!(
        "{}/oauth/callback/{}",
        state.external_url, provider_name
    );
    drop(store);

    // Exchange code with provider
    let provider_token = provider.exchange_code(&params.code, &callback_uri).await?;
    let provider_user = provider.get_user_info(&provider_token).await?;

    let store = state.store.lock().await;

    // Look up user by provider identity
    let user = store
        .get_user_by_provider_identity(db_provider.id, &provider_user.provider_user_id)?
        .ok_or_else(|| {
            AppError::status(
                StatusCode::FORBIDDEN,
                format!(
                    "no trivia user linked to {} account '{}' — an admin must add you first",
                    provider_name, provider_user.username
                ),
            )
        })?;

    // Create auth code for the client
    let auth_code =
        store.create_auth_code(client_id, user.id, code_challenge, &redirect_uri)?;

    // Redirect back to client with code
    let sep = if redirect_uri.contains('?') { "&" } else { "?" };
    let redirect_target = format!(
        "{redirect_uri}{sep}code={auth_code}&state={original_state}"
    );

    Ok(Redirect::temporary(&redirect_target).into_response())
}

// --- Token Exchange ---

#[derive(Deserialize)]
#[allow(dead_code)]
struct TokenRequest {
    grant_type: String,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: i64,
    refresh_token: String,
}

async fn token_exchange(
    State(state): State<OAuthState>,
    axum::Json(body): axum::Json<TokenRequest>,
) -> Result<impl IntoResponse, AppError> {
    match body.grant_type.as_str() {
        "authorization_code" => {
            let code = body
                .code
                .as_deref()
                .ok_or_else(|| AppError::bad_request("missing code"))?;
            let verifier = body
                .code_verifier
                .as_deref()
                .ok_or_else(|| AppError::bad_request("missing code_verifier"))?;

            let store = state.store.lock().await;
            let auth_code = store.consume_auth_code(code)?;

            // Verify PKCE: SHA256(verifier) == challenge
            let computed_challenge = pkce_challenge(verifier);
            if computed_challenge != auth_code.code_challenge {
                return Err(AppError::bad_request("PKCE verification failed"));
            }

            // Verify redirect_uri matches
            if let Some(uri) = &body.redirect_uri {
                if *uri != auth_code.redirect_uri {
                    return Err(AppError::bad_request("redirect_uri mismatch"));
                }
            }

            let pair = store.create_token_pair(&auth_code.client_id, auth_code.user_id)?;
            let expires_in = (pair.expires_at - chrono::Utc::now()).num_seconds();

            Ok(axum::Json(TokenResponse {
                access_token: pair.access_token,
                token_type: "Bearer".into(),
                expires_in,
                refresh_token: pair.refresh_token,
            }))
        }
        "refresh_token" => {
            let refresh = body
                .refresh_token
                .as_deref()
                .ok_or_else(|| AppError::bad_request("missing refresh_token"))?;

            let store = state.store.lock().await;
            let (user, client_id) = store
                .get_user_by_refresh_token(refresh)?
                .ok_or_else(|| AppError::bad_request("invalid refresh_token"))?;

            // Revoke old token pair
            store.revoke_refresh_token(refresh)?;

            // Issue new pair
            let pair = store.create_token_pair(&client_id, user.id)?;
            let expires_in = (pair.expires_at - chrono::Utc::now()).num_seconds();

            Ok(axum::Json(TokenResponse {
                access_token: pair.access_token,
                token_type: "Bearer".into(),
                expires_in,
                refresh_token: pair.refresh_token,
            }))
        }
        other => Err(AppError::bad_request(&format!(
            "unsupported grant_type: {other}"
        ))),
    }
}

// --- Web UI Auth ---

async fn auth_login(
    State(state): State<OAuthState>,
    Path(provider_name): Path<String>,
) -> Result<Response, AppError> {
    let store = state.store.lock().await;
    let db_provider = store
        .get_provider_by_name(&provider_name)?
        .ok_or_else(|| AppError::bad_request("unknown provider"))?;
    let provider = Provider::from_db(&db_provider)?;
    drop(store);

    // Generate a random state for CSRF protection
    let csrf_state = trivia_core::auth_store::sha256_hex(
        &format!("webui-{}", rand::random::<u64>()),
    );
    let callback_uri = format!("{}/auth/callback/{}", state.external_url, provider_name);
    let auth_url = provider.authorize_url(&csrf_state, &callback_uri);

    Ok(Redirect::temporary(&auth_url).into_response())
}

async fn auth_callback(
    State(state): State<OAuthState>,
    Path(provider_name): Path<String>,
    Query(params): Query<CallbackParams>,
) -> Result<Response, AppError> {
    let store = state.store.lock().await;
    let db_provider = store
        .get_provider_by_name(&provider_name)?
        .ok_or_else(|| AppError::bad_request("unknown provider"))?;
    let provider = Provider::from_db(&db_provider)?;

    let callback_uri = format!("{}/auth/callback/{}", state.external_url, provider_name);
    drop(store);

    let provider_token = provider.exchange_code(&params.code, &callback_uri).await?;
    let provider_user = provider.get_user_info(&provider_token).await?;

    let store = state.store.lock().await;

    let user = store
        .get_user_by_provider_identity(db_provider.id, &provider_user.provider_user_id)?
        .ok_or_else(|| {
            AppError::status(
                StatusCode::FORBIDDEN,
                format!(
                    "no trivia user linked to {} account '{}' — an admin must add you first",
                    provider_name, provider_user.username
                ),
            )
        })?;

    let session = store.create_session(user.id)?;

    // Set cookie and redirect to /
    let cookie = format!(
        "trivia_session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000",
        session.session_id
    );

    Ok((
        [(axum::http::header::SET_COOKIE, cookie)],
        Redirect::temporary("/"),
    )
        .into_response())
}

async fn auth_logout(State(state): State<OAuthState>, headers: axum::http::HeaderMap) -> Response {
    if let Some(session_id) = extract_session_cookie(&headers) {
        let store = state.store.lock().await;
        let _ = store.delete_session(&session_id);
    }

    let clear = "trivia_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0";
    (
        [(axum::http::header::SET_COOKIE, clear)],
        axum::Json(serde_json::json!({"ok": true})),
    )
        .into_response()
}

#[derive(Serialize)]
struct MeResponse {
    username: String,
    acl: String,
}

async fn auth_me(
    State(state): State<OAuthState>,
    headers: axum::http::HeaderMap,
) -> Result<Response, AppError> {
    // Try bearer token first
    if let Some(user) = extract_bearer_user(&state, &headers).await? {
        return Ok(axum::Json(MeResponse {
            username: user.username,
            acl: user.acl,
        })
        .into_response());
    }

    // Try session cookie
    if let Some(session_id) = extract_session_cookie(&headers) {
        let store = state.store.lock().await;
        if let Some((_sess, user)) = store.get_session(&session_id)? {
            return Ok(axum::Json(MeResponse {
                username: user.username,
                acl: user.acl,
            })
            .into_response());
        }
    }

    Err(AppError::status(StatusCode::UNAUTHORIZED, "not authenticated"))
}

async fn list_providers(
    State(state): State<OAuthState>,
) -> Result<impl IntoResponse, AppError> {
    let store = state.store.lock().await;
    let providers = store.list_enabled_providers()?;
    let names: Vec<&str> = providers.iter().map(|(name, _)| name.as_str()).collect();
    Ok(axum::Json(serde_json::json!({ "providers": names })))
}

// --- Helpers ---

pub fn extract_session_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("trivia_session=") {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub async fn extract_bearer_user(
    state: &OAuthState,
    headers: &axum::http::HeaderMap,
) -> Result<Option<trivia_core::User>> {
    let auth_header = match headers.get("authorization") {
        Some(h) => h.to_str().unwrap_or(""),
        None => return Ok(None),
    };
    let token = match auth_header.strip_prefix("Bearer ") {
        Some(t) => t,
        None => return Ok(None),
    };
    let store = state.store.lock().await;
    Ok(store.get_user_by_access_token(token)?)
}

fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    base64_url_encode(&hash)
}

fn base64_url_encode(data: &[u8]) -> String {
    let encoded = base64_encode_bytes(data);
    encoded
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

fn base64_encode(s: &str) -> String {
    base64_encode_bytes(s.as_bytes())
}

fn base64_encode_bytes(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[(n >> 18 & 0x3F) as usize] as char);
        result.push(CHARS[(n >> 12 & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[(n >> 6 & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(s: &str) -> Result<String> {
    const DECODE: [u8; 128] = {
        let mut table = [255u8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < 64 {
            table[chars[i] as usize] = i as u8;
            i += 1;
        }
        // Also handle URL-safe variants
        table[b'-' as usize] = 62;
        table[b'_' as usize] = 63;
        table
    };

    let input: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut output = Vec::new();

    for chunk in input.chunks(4) {
        let vals: Vec<u8> = chunk
            .iter()
            .map(|&b| {
                if (b as usize) < 128 {
                    DECODE[b as usize]
                } else {
                    255
                }
            })
            .collect();
        if vals.iter().any(|&v| v == 255) {
            anyhow::bail!("invalid base64");
        }
        let n = (vals[0] as u32) << 18
            | (vals.get(1).copied().unwrap_or(0) as u32) << 12
            | (vals.get(2).copied().unwrap_or(0) as u32) << 6
            | (vals.get(3).copied().unwrap_or(0) as u32);
        output.push((n >> 16 & 0xFF) as u8);
        if chunk.len() > 2 {
            output.push((n >> 8 & 0xFF) as u8);
        }
        if chunk.len() > 3 {
            output.push((n & 0xFF) as u8);
        }
    }

    String::from_utf8(output).map_err(|e| anyhow::anyhow!("invalid utf8 in base64: {e}"))
}

// --- Error type ---

pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    pub fn bad_request(msg: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.to_string(),
        }
    }

    pub fn status(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            message: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}
