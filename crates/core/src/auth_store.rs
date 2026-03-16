use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::MemoryStore;

// --- Data types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub acl: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthProvider {
    pub id: i64,
    pub name: String,
    pub provider_type: String,
    pub client_id: String,
    pub client_secret: String,
    pub enabled: bool,
    pub config: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIdentity {
    pub id: i64,
    pub user_id: i64,
    pub provider_id: i64,
    pub provider_username: String,
    pub provider_user_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct OAuthCode {
    pub code: String,
    pub client_id: String,
    pub user_id: i64,
    pub code_challenge: String,
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub user_id: i64,
    pub expires_at: DateTime<Utc>,
}

// --- Helpers ---

fn generate_random_string(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn parse_dt(s: &str) -> DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_default()
}

// --- MemoryStore auth methods ---

impl MemoryStore {
    // ========== Users ==========

    pub fn create_user(&self, username: &str, acl: &str) -> Result<User> {
        self.conn().execute(
            "INSERT INTO users (username, acl) VALUES (?1, ?2)",
            params![username, acl],
        )?;
        self.get_user_by_username(username)?
            .ok_or_else(|| anyhow!("failed to create user"))
    }

    pub fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, username, acl, created_at, updated_at FROM users WHERE username = ?1",
        )?;
        let user = stmt
            .query_row(params![username], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    acl: row.get(2)?,
                    created_at: parse_dt(&row.get::<_, String>(3)?),
                    updated_at: parse_dt(&row.get::<_, String>(4)?),
                })
            })
            .ok();
        Ok(user)
    }

    pub fn get_user_by_id(&self, id: i64) -> Result<Option<User>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, username, acl, created_at, updated_at FROM users WHERE id = ?1",
        )?;
        let user = stmt
            .query_row(params![id], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    acl: row.get(2)?,
                    created_at: parse_dt(&row.get::<_, String>(3)?),
                    updated_at: parse_dt(&row.get::<_, String>(4)?),
                })
            })
            .ok();
        Ok(user)
    }

    pub fn update_user_acl(&self, username: &str, acl: &str) -> Result<()> {
        let rows = self.conn().execute(
            "UPDATE users SET acl = ?1, updated_at = datetime('now') WHERE username = ?2",
            params![acl, username],
        )?;
        if rows == 0 {
            return Err(anyhow!("user not found: {}", username));
        }
        Ok(())
    }

    pub fn list_users(&self) -> Result<Vec<User>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT id, username, acl, created_at, updated_at FROM users ORDER BY username")?;
        let users = stmt
            .query_map([], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    acl: row.get(2)?,
                    created_at: parse_dt(&row.get::<_, String>(3)?),
                    updated_at: parse_dt(&row.get::<_, String>(4)?),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(users)
    }

    pub fn delete_user(&self, username: &str) -> Result<bool> {
        let rows = self
            .conn()
            .execute("DELETE FROM users WHERE username = ?1", params![username])?;
        Ok(rows > 0)
    }

    // ========== OAuth Providers ==========

    pub fn create_provider(
        &self,
        name: &str,
        provider_type: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<OAuthProvider> {
        self.conn().execute(
            "INSERT INTO oauth_providers (name, provider_type, client_id, client_secret) VALUES (?1, ?2, ?3, ?4)",
            params![name, provider_type, client_id, client_secret],
        )?;
        self.get_provider_by_name(name)?
            .ok_or_else(|| anyhow!("failed to create provider"))
    }

    pub fn get_provider_by_name(&self, name: &str) -> Result<Option<OAuthProvider>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, name, provider_type, client_id, client_secret, enabled, config, created_at
             FROM oauth_providers WHERE name = ?1",
        )?;
        let provider = stmt
            .query_row(params![name], |row| {
                Ok(OAuthProvider {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider_type: row.get(2)?,
                    client_id: row.get(3)?,
                    client_secret: row.get(4)?,
                    enabled: row.get::<_, i64>(5)? != 0,
                    config: row.get(6)?,
                    created_at: parse_dt(&row.get::<_, String>(7)?),
                })
            })
            .ok();
        Ok(provider)
    }

    pub fn list_providers(&self) -> Result<Vec<OAuthProvider>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, name, provider_type, client_id, client_secret, enabled, config, created_at
             FROM oauth_providers ORDER BY name",
        )?;
        let providers = stmt
            .query_map([], |row| {
                Ok(OAuthProvider {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider_type: row.get(2)?,
                    client_id: row.get(3)?,
                    client_secret: row.get(4)?,
                    enabled: row.get::<_, i64>(5)? != 0,
                    config: row.get(6)?,
                    created_at: parse_dt(&row.get::<_, String>(7)?),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(providers)
    }

    pub fn delete_provider(&self, name: &str) -> Result<bool> {
        let rows = self
            .conn()
            .execute("DELETE FROM oauth_providers WHERE name = ?1", params![name])?;
        Ok(rows > 0)
    }

    // ========== User Identities ==========

    pub fn link_identity(
        &self,
        user_id: i64,
        provider_id: i64,
        provider_username: &str,
        provider_user_id: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO user_identities (user_id, provider_id, provider_username, provider_user_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, provider_id, provider_username, provider_user_id],
        )?;
        Ok(())
    }

    pub fn get_user_by_provider_identity(
        &self,
        provider_id: i64,
        provider_user_id: &str,
    ) -> Result<Option<User>> {
        let mut stmt = self.conn().prepare(
            "SELECT u.id, u.username, u.acl, u.created_at, u.updated_at
             FROM users u
             JOIN user_identities ui ON u.id = ui.user_id
             WHERE ui.provider_id = ?1 AND ui.provider_user_id = ?2",
        )?;
        let user = stmt
            .query_row(params![provider_id, provider_user_id], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    acl: row.get(2)?,
                    created_at: parse_dt(&row.get::<_, String>(3)?),
                    updated_at: parse_dt(&row.get::<_, String>(4)?),
                })
            })
            .ok();
        Ok(user)
    }

    pub fn list_identities_for_user(&self, user_id: i64) -> Result<Vec<UserIdentity>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, user_id, provider_id, provider_username, provider_user_id, created_at
             FROM user_identities WHERE user_id = ?1",
        )?;
        let identities = stmt
            .query_map(params![user_id], |row| {
                Ok(UserIdentity {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    provider_id: row.get(2)?,
                    provider_username: row.get(3)?,
                    provider_user_id: row.get(4)?,
                    created_at: parse_dt(&row.get::<_, String>(5)?),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(identities)
    }

    // ========== OAuth Clients (DCR) ==========

    pub fn register_client(
        &self,
        redirect_uris: &[String],
        client_name: Option<&str>,
    ) -> Result<(OAuthClient, Option<String>)> {
        let client_id = generate_random_string(32);
        // Generate a secret for confidential clients
        let secret = generate_random_string(48);
        let secret_hash = sha256_hex(&secret);
        let uris_json = serde_json::to_string(redirect_uris)?;

        self.conn().execute(
            "INSERT INTO oauth_clients (client_id, client_secret_hash, redirect_uris, client_name)
             VALUES (?1, ?2, ?3, ?4)",
            params![client_id, secret_hash, uris_json, client_name],
        )?;

        let client = OAuthClient {
            client_id: client_id.clone(),
            redirect_uris: redirect_uris.to_vec(),
            client_name: client_name.map(String::from),
            created_at: Utc::now(),
        };
        Ok((client, Some(secret)))
    }

    pub fn get_client(&self, client_id: &str) -> Result<Option<OAuthClient>> {
        let mut stmt = self.conn().prepare(
            "SELECT client_id, redirect_uris, client_name, created_at
             FROM oauth_clients WHERE client_id = ?1",
        )?;
        let client = stmt
            .query_row(params![client_id], |row| {
                let uris_json: String = row.get(1)?;
                let redirect_uris: Vec<String> =
                    serde_json::from_str(&uris_json).unwrap_or_default();
                Ok(OAuthClient {
                    client_id: row.get(0)?,
                    redirect_uris,
                    client_name: row.get(2)?,
                    created_at: parse_dt(&row.get::<_, String>(3)?),
                })
            })
            .ok();
        Ok(client)
    }

    pub fn verify_client_secret(&self, client_id: &str, secret: &str) -> Result<bool> {
        let hash = sha256_hex(secret);
        let stored_hash: Option<String> = self
            .conn()
            .query_row(
                "SELECT client_secret_hash FROM oauth_clients WHERE client_id = ?1",
                params![client_id],
                |row| row.get(0),
            )
            .ok();
        Ok(stored_hash.as_deref() == Some(&hash))
    }

    // ========== Auth Codes ==========

    pub fn create_auth_code(
        &self,
        client_id: &str,
        user_id: i64,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<String> {
        let code = generate_random_string(48);
        let expires_at = Utc::now() + Duration::minutes(10);
        let expires_str = expires_at.format("%Y-%m-%d %H:%M:%S").to_string();

        self.conn().execute(
            "INSERT INTO oauth_codes (code, client_id, user_id, code_challenge, redirect_uri, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![code, client_id, user_id, code_challenge, redirect_uri, expires_str],
        )?;
        Ok(code)
    }

    pub fn consume_auth_code(&self, code: &str) -> Result<OAuthCode> {
        let mut stmt = self.conn().prepare(
            "SELECT code, client_id, user_id, code_challenge, redirect_uri, expires_at, used
             FROM oauth_codes WHERE code = ?1",
        )?;
        let row = stmt
            .query_row(params![code], |row| {
                Ok((
                    OAuthCode {
                        code: row.get(0)?,
                        client_id: row.get(1)?,
                        user_id: row.get(2)?,
                        code_challenge: row.get(3)?,
                        redirect_uri: row.get(4)?,
                        expires_at: parse_dt(&row.get::<_, String>(5)?),
                    },
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|_| anyhow!("invalid authorization code"))?;

        let (auth_code, used) = row;
        if used != 0 {
            return Err(anyhow!("authorization code already used"));
        }
        if auth_code.expires_at < Utc::now() {
            return Err(anyhow!("authorization code expired"));
        }

        self.conn().execute(
            "UPDATE oauth_codes SET used = 1 WHERE code = ?1",
            params![code],
        )?;

        Ok(auth_code)
    }

    // ========== Tokens ==========

    pub fn create_token_pair(&self, client_id: &str, user_id: i64) -> Result<TokenPair> {
        let access_token = generate_random_string(48);
        let refresh_token = generate_random_string(48);
        let access_hash = sha256_hex(&access_token);
        let refresh_hash = sha256_hex(&refresh_token);
        let expires_at = Utc::now() + Duration::hours(24);
        let expires_str = expires_at.format("%Y-%m-%d %H:%M:%S").to_string();

        self.conn().execute(
            "INSERT INTO oauth_tokens (access_token_hash, refresh_token_hash, client_id, user_id, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![access_hash, refresh_hash, client_id, user_id, expires_str],
        )?;

        Ok(TokenPair {
            access_token,
            refresh_token,
            expires_at,
        })
    }

    pub fn get_user_by_access_token(&self, token: &str) -> Result<Option<User>> {
        let hash = sha256_hex(token);
        let mut stmt = self.conn().prepare(
            "SELECT u.id, u.username, u.acl, u.created_at, u.updated_at
             FROM users u
             JOIN oauth_tokens t ON u.id = t.user_id
             WHERE t.access_token_hash = ?1 AND t.expires_at > datetime('now')",
        )?;
        let user = stmt
            .query_row(params![hash], |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    acl: row.get(2)?,
                    created_at: parse_dt(&row.get::<_, String>(3)?),
                    updated_at: parse_dt(&row.get::<_, String>(4)?),
                })
            })
            .ok();
        Ok(user)
    }

    pub fn get_user_by_refresh_token(&self, token: &str) -> Result<Option<(User, String)>> {
        let hash = sha256_hex(token);
        let mut stmt = self.conn().prepare(
            "SELECT u.id, u.username, u.acl, u.created_at, u.updated_at, t.client_id
             FROM users u
             JOIN oauth_tokens t ON u.id = t.user_id
             WHERE t.refresh_token_hash = ?1",
        )?;
        let result = stmt
            .query_row(params![hash], |row| {
                Ok((
                    User {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        acl: row.get(2)?,
                        created_at: parse_dt(&row.get::<_, String>(3)?),
                        updated_at: parse_dt(&row.get::<_, String>(4)?),
                    },
                    row.get::<_, String>(5)?,
                ))
            })
            .ok();
        Ok(result)
    }

    pub fn revoke_refresh_token(&self, token: &str) -> Result<()> {
        let hash = sha256_hex(token);
        self.conn().execute(
            "DELETE FROM oauth_tokens WHERE refresh_token_hash = ?1",
            params![hash],
        )?;
        Ok(())
    }

    pub fn cleanup_expired_tokens(&self) -> Result<usize> {
        let rows = self.conn().execute(
            "DELETE FROM oauth_tokens WHERE expires_at <= datetime('now')",
            [],
        )?;
        Ok(rows)
    }

    // ========== Sessions ==========

    pub fn create_session(&self, user_id: i64) -> Result<Session> {
        let session_id = generate_random_string(48);
        let expires_at = Utc::now() + Duration::days(30);
        let expires_str = expires_at.format("%Y-%m-%d %H:%M:%S").to_string();

        self.conn().execute(
            "INSERT INTO sessions (session_id, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![session_id, user_id, expires_str],
        )?;

        Ok(Session {
            session_id,
            user_id,
            expires_at,
        })
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<(Session, User)>> {
        let mut stmt = self.conn().prepare(
            "SELECT s.session_id, s.user_id, s.expires_at,
                    u.id, u.username, u.acl, u.created_at, u.updated_at
             FROM sessions s
             JOIN users u ON s.user_id = u.id
             WHERE s.session_id = ?1 AND s.expires_at > datetime('now')",
        )?;
        let result = stmt
            .query_row(params![session_id], |row| {
                Ok((
                    Session {
                        session_id: row.get(0)?,
                        user_id: row.get(1)?,
                        expires_at: parse_dt(&row.get::<_, String>(2)?),
                    },
                    User {
                        id: row.get(3)?,
                        username: row.get(4)?,
                        acl: row.get(5)?,
                        created_at: parse_dt(&row.get::<_, String>(6)?),
                        updated_at: parse_dt(&row.get::<_, String>(7)?),
                    },
                ))
            })
            .ok();
        Ok(result)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn cleanup_expired_sessions(&self) -> Result<usize> {
        let rows = self.conn().execute(
            "DELETE FROM sessions WHERE expires_at <= datetime('now')",
            [],
        )?;
        Ok(rows)
    }

    // ========== Auth status ==========

    /// Returns true if any OAuth providers are configured and enabled.
    pub fn has_auth_providers(&self) -> Result<bool> {
        let count: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM oauth_providers WHERE enabled = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// List enabled providers (name + type only, no secrets).
    pub fn list_enabled_providers(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn().prepare(
            "SELECT name, provider_type FROM oauth_providers WHERE enabled = 1 ORDER BY name",
        )?;
        let providers = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(providers)
    }

    pub fn cleanup_expired_codes(&self) -> Result<usize> {
        let rows = self.conn().execute(
            "DELETE FROM oauth_codes WHERE expires_at <= datetime('now')",
            [],
        )?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> MemoryStore {
        MemoryStore::in_memory().unwrap()
    }

    #[test]
    fn user_crud() {
        let store = test_store();
        let user = store.create_user("alice", "*:update").unwrap();
        assert_eq!(user.username, "alice");
        assert_eq!(user.acl, "*:update");

        let fetched = store.get_user_by_username("alice").unwrap().unwrap();
        assert_eq!(fetched.id, user.id);

        let fetched = store.get_user_by_id(user.id).unwrap().unwrap();
        assert_eq!(fetched.username, "alice");

        store.update_user_acl("alice", "project:read,*:none").unwrap();
        let updated = store.get_user_by_username("alice").unwrap().unwrap();
        assert_eq!(updated.acl, "project:read,*:none");

        let users = store.list_users().unwrap();
        assert_eq!(users.len(), 1);

        assert!(store.delete_user("alice").unwrap());
        assert!(store.get_user_by_username("alice").unwrap().is_none());
    }

    #[test]
    fn provider_crud() {
        let store = test_store();
        let prov = store
            .create_provider("github", "github", "id123", "secret456")
            .unwrap();
        assert_eq!(prov.name, "github");
        assert_eq!(prov.provider_type, "github");

        let fetched = store.get_provider_by_name("github").unwrap().unwrap();
        assert_eq!(fetched.client_id, "id123");

        let providers = store.list_providers().unwrap();
        assert_eq!(providers.len(), 1);

        assert!(store.delete_provider("github").unwrap());
        assert!(store.get_provider_by_name("github").unwrap().is_none());
    }

    #[test]
    fn identity_linking() {
        let store = test_store();
        let user = store.create_user("bob", "*:read").unwrap();
        let prov = store
            .create_provider("github", "github", "id", "secret")
            .unwrap();
        store
            .link_identity(user.id, prov.id, "bobgithub", "12345")
            .unwrap();

        let found = store
            .get_user_by_provider_identity(prov.id, "12345")
            .unwrap()
            .unwrap();
        assert_eq!(found.username, "bob");

        let identities = store.list_identities_for_user(user.id).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].provider_username, "bobgithub");
    }

    #[test]
    fn dcr_client_flow() {
        let store = test_store();
        let (client, secret) = store
            .register_client(&["http://localhost/callback".into()], Some("Test App"))
            .unwrap();
        assert!(!client.client_id.is_empty());
        let secret = secret.unwrap();

        let fetched = store.get_client(&client.client_id).unwrap().unwrap();
        assert_eq!(fetched.client_name.as_deref(), Some("Test App"));
        assert_eq!(fetched.redirect_uris, vec!["http://localhost/callback"]);

        assert!(store.verify_client_secret(&client.client_id, &secret).unwrap());
        assert!(!store.verify_client_secret(&client.client_id, "wrong").unwrap());
    }

    #[test]
    fn auth_code_flow() {
        let store = test_store();
        let user = store.create_user("charlie", "*:update").unwrap();
        let (client, _) = store
            .register_client(&["http://localhost/cb".into()], None)
            .unwrap();

        let code = store
            .create_auth_code(&client.client_id, user.id, "challenge123", "http://localhost/cb")
            .unwrap();
        assert!(!code.is_empty());

        let consumed = store.consume_auth_code(&code).unwrap();
        assert_eq!(consumed.user_id, user.id);
        assert_eq!(consumed.code_challenge, "challenge123");

        // Second consume should fail (already used)
        assert!(store.consume_auth_code(&code).is_err());
    }

    #[test]
    fn token_flow() {
        let store = test_store();
        let user = store.create_user("dave", "*:read").unwrap();
        let (client, _) = store
            .register_client(&["http://localhost/cb".into()], None)
            .unwrap();

        let pair = store.create_token_pair(&client.client_id, user.id).unwrap();
        assert!(!pair.access_token.is_empty());

        let found = store
            .get_user_by_access_token(&pair.access_token)
            .unwrap()
            .unwrap();
        assert_eq!(found.username, "dave");

        assert!(store
            .get_user_by_access_token("nonexistent")
            .unwrap()
            .is_none());
    }

    #[test]
    fn session_flow() {
        let store = test_store();
        let user = store.create_user("eve", "*:read").unwrap();

        let session = store.create_session(user.id).unwrap();
        assert!(!session.session_id.is_empty());

        let (sess, found_user) = store.get_session(&session.session_id).unwrap().unwrap();
        assert_eq!(found_user.username, "eve");
        assert_eq!(sess.user_id, user.id);

        store.delete_session(&session.session_id).unwrap();
        assert!(store.get_session(&session.session_id).unwrap().is_none());
    }

    #[test]
    fn has_auth_providers_check() {
        let store = test_store();
        assert!(!store.has_auth_providers().unwrap());

        store
            .create_provider("github", "github", "id", "secret")
            .unwrap();
        assert!(store.has_auth_providers().unwrap());
    }
}
