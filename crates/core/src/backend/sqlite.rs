//! SQLite-backed implementation of [`MemoryBackend`] and [`AuthBackend`].
//!
//! Thin async wrapper around the synchronous [`MemoryStore`]. A `tokio::Mutex`
//! serializes access to the single `rusqlite` connection; each method locks and
//! delegates to the existing, well-tested store logic.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{AuthBackend, MemoryBackend};
use crate::auth_store::{OAuthClient, OAuthCode, OAuthProvider, Session, TokenPair, User, UserIdentity};
use crate::embedder::Embedder;
use crate::export::ImportResult;
use crate::store::{
    EditResult, Memory, MemoryLink, MemoryStore, MemorizeResult, MergeCandidate, MemorySummary,
    RecallCandidates, ScoringConfig, TagCount,
};

pub struct SqliteBackend {
    store: Mutex<MemoryStore>,
    scoring: ScoringConfig,
}

impl SqliteBackend {
    /// Open (or create) a SQLite-backed store at `db_path`.
    pub fn open(db_path: &Path, scoring: ScoringConfig) -> Result<Self> {
        let mut store = MemoryStore::new(db_path)?;
        store.set_boost_tags(scoring.boost_tags.clone());
        Ok(Self {
            store: Mutex::new(store),
            scoring,
        })
    }

    /// In-memory store, primarily for tests.
    pub fn in_memory(scoring: ScoringConfig) -> Result<Self> {
        let mut store = MemoryStore::in_memory()?;
        store.set_boost_tags(scoring.boost_tags.clone());
        Ok(Self {
            store: Mutex::new(store),
            scoring,
        })
    }
}

#[async_trait]
impl MemoryBackend for SqliteBackend {
    fn scoring(&self) -> &ScoringConfig {
        &self.scoring
    }

    async fn memorize(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<MemorizeResult> {
        self.store.lock().await.memorize(mnemonic, content, tags, embedding)
    }

    async fn memorize_with_options(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
        skip_merge: bool,
    ) -> Result<MemorizeResult> {
        self.store
            .lock()
            .await
            .memorize_with_options(mnemonic, content, tags, embedding, skip_merge)
    }

    async fn recall_candidates(
        &self,
        embedding: &[f32],
        limit: usize,
        tags: Option<&[String]>,
        fts_query: Option<&str>,
        exclude_tags: Option<&[String]>,
    ) -> Result<RecallCandidates> {
        self.store
            .lock()
            .await
            .recall_candidates(embedding, limit, tags, fts_query, exclude_tags)
    }

    async fn bump_recall_stats(&self, titles: &[String]) -> Result<()> {
        let refs: Vec<&str> = titles.iter().map(String::as_str).collect();
        self.store.lock().await.bump_recall_stats(&refs)
    }

    async fn get_memory_by_mnemonic(&self, title: &str) -> Result<Option<Memory>> {
        self.store.lock().await.get_memory_by_mnemonic(title)
    }

    async fn delete_memory(&self, title: &str) -> Result<bool> {
        self.store.lock().await.delete_memory(title)
    }

    async fn rate(&self, title: &str, useful: bool) -> Result<()> {
        self.store.lock().await.rate(title, useful)
    }

    async fn rate_batch(&self, titles: &[String], useful: bool) -> Result<Vec<String>> {
        self.store.lock().await.rate_batch(titles, useful)
    }

    async fn link(&self, source: &str, target: &str, link_type: &str) -> Result<()> {
        self.store.lock().await.link(source, target, link_type)
    }

    async fn unlink(&self, source: &str, target: &str, link_type: &str) -> Result<()> {
        self.store.lock().await.unlink(source, target, link_type)
    }

    async fn get_links(&self, title: &str) -> Result<Vec<MemoryLink>> {
        self.store.lock().await.get_links(title)
    }

    async fn get_all_links(&self) -> Result<Vec<MemoryLink>> {
        self.store.lock().await.get_all_links()
    }

    async fn merge(&self, keep: &str, discard: &str, embedding: &[f32]) -> Result<()> {
        self.store.lock().await.merge(keep, discard, embedding)
    }

    async fn add_mnemonic(&self, title: &str, text: &str, embedding: &[f32]) -> Result<()> {
        self.store.lock().await.add_mnemonic(title, text, embedding)
    }

    async fn remove_mnemonic(&self, title: &str, text: &str) -> Result<()> {
        self.store.lock().await.remove_mnemonic(title, text)
    }

    async fn update_memory(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<()> {
        self.store.lock().await.update_memory(title, content, tags, embedding)
    }

    async fn rename_memory(
        &self,
        old_title: &str,
        new_title: &str,
        embedding: &[f32],
    ) -> Result<()> {
        self.store.lock().await.rename_memory(old_title, new_title, embedding)
    }

    async fn edit_memory(
        &self,
        title: &str,
        new_title: Option<&str>,
        add_tags: &[String],
        remove_tags: &[String],
        new_embedding: Option<&[f32]>,
        add_mnemonics: &[String],
        remove_mnemonics: &[String],
        mnemonic_embeddings: &[Vec<f32>],
    ) -> Result<EditResult> {
        self.store.lock().await.edit_memory(
            title,
            new_title,
            add_tags,
            remove_tags,
            new_embedding,
            add_mnemonics,
            remove_mnemonics,
            mnemonic_embeddings,
        )
    }

    async fn rename_tag(&self, old_tag: &str, new_tag: &str) -> Result<usize> {
        self.store.lock().await.rename_tag(old_tag, new_tag)
    }

    async fn list_tags(&self) -> Result<Vec<TagCount>> {
        self.store.lock().await.list_tags()
    }

    async fn list_all_summaries(&self) -> Result<Vec<MemorySummary>> {
        self.store.lock().await.list_all_summaries()
    }

    async fn find_nearest(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude_title: &str,
    ) -> Result<Vec<(String, f64)>> {
        self.store.lock().await.find_nearest(embedding, threshold, exclude_title)
    }

    async fn find_merge_candidates(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude: &HashSet<String>,
        limit: usize,
    ) -> Result<Vec<MergeCandidate>> {
        self.store
            .lock()
            .await
            .find_merge_candidates(embedding, threshold, exclude, limit)
    }

    async fn export(&self, dir: &Path, tags: Option<&[String]>) -> Result<()> {
        self.store.lock().await.export(dir, tags)
    }

    async fn export_filtered(
        &self,
        dir: &Path,
        tags: Option<&[String]>,
        filter: &(dyn for<'a> Fn(&'a [String]) -> bool + Send + Sync),
    ) -> Result<()> {
        self.store.lock().await.export_filtered(dir, tags, filter)
    }

    async fn import(&self, dir: &Path, embedder: &mut Embedder) -> Result<ImportResult> {
        self.store.lock().await.import(dir, embedder)
    }
}

#[async_trait]
impl AuthBackend for SqliteBackend {
    async fn create_user(&self, username: &str, acl: &str) -> Result<User> {
        self.store.lock().await.create_user(username, acl)
    }

    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        self.store.lock().await.get_user_by_username(username)
    }

    async fn get_user_by_id(&self, id: i64) -> Result<Option<User>> {
        self.store.lock().await.get_user_by_id(id)
    }

    async fn update_user_acl(&self, username: &str, acl: &str) -> Result<()> {
        self.store.lock().await.update_user_acl(username, acl)
    }

    async fn list_users(&self) -> Result<Vec<User>> {
        self.store.lock().await.list_users()
    }

    async fn delete_user(&self, username: &str) -> Result<bool> {
        self.store.lock().await.delete_user(username)
    }

    async fn create_provider(
        &self,
        name: &str,
        provider_type: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<OAuthProvider> {
        self.store
            .lock()
            .await
            .create_provider(name, provider_type, client_id, client_secret)
    }

    async fn get_provider_by_name(&self, name: &str) -> Result<Option<OAuthProvider>> {
        self.store.lock().await.get_provider_by_name(name)
    }

    async fn list_providers(&self) -> Result<Vec<OAuthProvider>> {
        self.store.lock().await.list_providers()
    }

    async fn delete_provider(&self, name: &str) -> Result<bool> {
        self.store.lock().await.delete_provider(name)
    }

    async fn link_identity(
        &self,
        user_id: i64,
        provider_id: i64,
        provider_username: &str,
        provider_user_id: &str,
    ) -> Result<()> {
        self.store
            .lock()
            .await
            .link_identity(user_id, provider_id, provider_username, provider_user_id)
    }

    async fn get_user_by_provider_identity(
        &self,
        provider_id: i64,
        provider_user_id: &str,
    ) -> Result<Option<User>> {
        self.store
            .lock()
            .await
            .get_user_by_provider_identity(provider_id, provider_user_id)
    }

    async fn list_identities_for_user(&self, user_id: i64) -> Result<Vec<UserIdentity>> {
        self.store.lock().await.list_identities_for_user(user_id)
    }

    async fn register_client(
        &self,
        redirect_uris: &[String],
        client_name: Option<&str>,
    ) -> Result<(OAuthClient, Option<String>)> {
        self.store.lock().await.register_client(redirect_uris, client_name)
    }

    async fn get_client(&self, client_id: &str) -> Result<Option<OAuthClient>> {
        self.store.lock().await.get_client(client_id)
    }

    async fn verify_client_secret(&self, client_id: &str, secret: &str) -> Result<bool> {
        self.store.lock().await.verify_client_secret(client_id, secret)
    }

    async fn create_auth_code(
        &self,
        client_id: &str,
        user_id: i64,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<String> {
        self.store
            .lock()
            .await
            .create_auth_code(client_id, user_id, code_challenge, redirect_uri)
    }

    async fn consume_auth_code(&self, code: &str) -> Result<OAuthCode> {
        self.store.lock().await.consume_auth_code(code)
    }

    async fn create_token_pair(&self, client_id: &str, user_id: i64) -> Result<TokenPair> {
        self.store.lock().await.create_token_pair(client_id, user_id)
    }

    async fn get_user_by_access_token(&self, token: &str) -> Result<Option<User>> {
        self.store.lock().await.get_user_by_access_token(token)
    }

    async fn get_user_by_refresh_token(&self, token: &str) -> Result<Option<(User, String)>> {
        self.store.lock().await.get_user_by_refresh_token(token)
    }

    async fn revoke_refresh_token(&self, token: &str) -> Result<()> {
        self.store.lock().await.revoke_refresh_token(token)
    }

    async fn cleanup_expired_tokens(&self) -> Result<usize> {
        self.store.lock().await.cleanup_expired_tokens()
    }

    async fn create_session(&self, user_id: i64) -> Result<Session> {
        self.store.lock().await.create_session(user_id)
    }

    async fn get_session(&self, session_id: &str) -> Result<Option<(Session, User)>> {
        self.store.lock().await.get_session(session_id)
    }

    async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.store.lock().await.delete_session(session_id)
    }

    async fn cleanup_expired_sessions(&self) -> Result<usize> {
        self.store.lock().await.cleanup_expired_sessions()
    }

    async fn has_auth_providers(&self) -> Result<bool> {
        self.store.lock().await.has_auth_providers()
    }

    async fn list_enabled_providers(&self) -> Result<Vec<(String, String)>> {
        self.store.lock().await.list_enabled_providers()
    }

    async fn cleanup_expired_codes(&self) -> Result<usize> {
        self.store.lock().await.cleanup_expired_codes()
    }
}
