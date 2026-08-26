//! Pluggable storage backends for Trivia.
//!
//! [`MemoryBackend`] abstracts the semantic-memory layer: storage, retrieval,
//! and the *initial* KNN search. The composite-scoring **rerank** is a pure,
//! backend-agnostic step ([`crate::rerank`]) shared by every implementation —
//! backends only return candidates; they never rank them.
//!
//! [`AuthBackend`] abstracts the web server's auth/OAuth/session store. It is a
//! separate concern from memory: the SQLite backend implements both, while a
//! cloud memory backend (e.g. S3 Vectors) leaves auth to a local SQLite store.
//!
//! Two implementations ship today:
//! - [`sqlite::SqliteBackend`] — the default, wrapping [`crate::MemoryStore`].
//! - `s3vectors::S3VectorsBackend` — Amazon S3 Vectors (behind the `s3vectors`
//!   cargo feature).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::Utc;

use crate::auth_store::{OAuthClient, OAuthCode, OAuthProvider, Session, TokenPair, User, UserIdentity};
use crate::config::TriviaConfig;
use crate::embedder::Embedder;
use crate::export::ImportResult;
use crate::store::{
    EditResult, Memory, MemoryLink, MemorizeResult, MergeCandidate, MemorySummary,
    RecallCandidates, ScoringConfig, TagCount, rerank,
};

pub mod sqlite;
pub use sqlite::SqliteBackend;

#[cfg(feature = "s3vectors")]
pub mod s3vectors;
#[cfg(feature = "s3vectors")]
pub use s3vectors::{S3VectorsBackend, S3VectorsConfig};

/// The semantic-memory storage + retrieval layer.
///
/// Implementors provide the primitives — vector upsert/delete, the initial KNN
/// candidate fetch, and record CRUD. `recall` is provided: it runs the initial
/// KNN via [`recall_candidates`](MemoryBackend::recall_candidates), applies the
/// pure [`rerank`], then bumps recall stats. Signatures mirror
/// [`crate::MemoryStore`], made async so a network-backed store fits naturally.
#[async_trait]
pub trait MemoryBackend: Send + Sync {
    /// Scoring weights used by the provided `recall` reranker.
    fn scoring(&self) -> &ScoringConfig;

    async fn memorize(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<MemorizeResult>;

    async fn memorize_with_options(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
        skip_merge: bool,
    ) -> Result<MemorizeResult>;

    /// Initial KNN retrieval: nearest candidates plus the metadata reranking
    /// needs (tags, stats, links) and any lexical-match set. Unscored.
    async fn recall_candidates(
        &self,
        embedding: &[f32],
        limit: usize,
        tags: Option<&[String]>,
        fts_query: Option<&str>,
        exclude_tags: Option<&[String]>,
    ) -> Result<RecallCandidates>;

    /// Increment recall_count / last_recalled_at for the given memory titles.
    async fn bump_recall_stats(&self, titles: &[String]) -> Result<()>;

    /// Recall = initial KNN → pure rerank → stats bump. Provided; do not
    /// override unless a backend can fuse ranking into retrieval.
    async fn recall(
        &self,
        embedding: &[f32],
        limit: usize,
        tags: Option<&[String]>,
        fts_query: Option<&str>,
        exclude_tags: Option<&[String]>,
    ) -> Result<Vec<Memory>> {
        let RecallCandidates {
            mut memories,
            fts_matches,
        } = self
            .recall_candidates(embedding, limit, tags, fts_query, exclude_tags)
            .await?;

        rerank(&mut memories, self.scoring(), &fts_matches, Utc::now());
        memories.truncate(limit);

        let titles: Vec<String> = memories.iter().map(|m| m.mnemonic.clone()).collect();
        self.bump_recall_stats(&titles).await?;

        Ok(memories)
    }

    async fn get_memory_by_mnemonic(&self, title: &str) -> Result<Option<Memory>>;
    async fn delete_memory(&self, title: &str) -> Result<bool>;

    async fn rate(&self, title: &str, useful: bool) -> Result<()>;
    async fn rate_batch(&self, titles: &[String], useful: bool) -> Result<Vec<String>>;

    async fn link(&self, source: &str, target: &str, link_type: &str) -> Result<()>;
    async fn unlink(&self, source: &str, target: &str, link_type: &str) -> Result<()>;
    async fn get_links(&self, title: &str) -> Result<Vec<MemoryLink>>;
    async fn get_all_links(&self) -> Result<Vec<MemoryLink>>;

    async fn merge(&self, keep: &str, discard: &str, embedding: &[f32]) -> Result<()>;

    async fn add_mnemonic(&self, title: &str, text: &str, embedding: &[f32]) -> Result<()>;
    async fn remove_mnemonic(&self, title: &str, text: &str) -> Result<()>;

    async fn update_memory(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<()>;

    async fn rename_memory(
        &self,
        old_title: &str,
        new_title: &str,
        embedding: &[f32],
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<EditResult>;

    async fn rename_tag(&self, old_tag: &str, new_tag: &str) -> Result<usize>;
    async fn list_tags(&self) -> Result<Vec<TagCount>>;
    async fn list_all_summaries(&self) -> Result<Vec<MemorySummary>>;

    async fn find_nearest(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude_title: &str,
    ) -> Result<Vec<(String, f64)>>;

    async fn find_merge_candidates(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude: &HashSet<String>,
        limit: usize,
    ) -> Result<Vec<MergeCandidate>>;

    async fn export(&self, dir: &Path, tags: Option<&[String]>) -> Result<()> {
        self.export_filtered(dir, tags, &|_| true).await
    }

    /// Export only memories whose tags pass `filter` (used for ACL-gated share).
    ///
    /// Default impl writes a portable, human-readable markdown file per memory
    /// (frontmatter + body, links referenced by mnemonic). Backends with richer
    /// identity — like SQLite's UUID roundtrip — override this.
    async fn export_filtered(
        &self,
        dir: &Path,
        tags: Option<&[String]>,
        filter: &(dyn for<'a> Fn(&'a [String]) -> bool + Send + Sync),
    ) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        for summary in self.list_all_summaries().await? {
            if let Some(want) = tags
                && !want.iter().any(|t| summary.tags.contains(t))
            {
                continue;
            }
            if !filter(&summary.tags) {
                continue;
            }
            let Some(mem) = self.get_memory_by_mnemonic(&summary.mnemonic).await? else {
                continue;
            };
            let file = portable::to_markdown(&mem);
            let name = format!("{}.md", crate::export::slugify(&mem.mnemonic));
            std::fs::write(dir.join(name), file)?;
        }
        Ok(())
    }

    /// Import markdown files written by [`export`](MemoryBackend::export).
    ///
    /// Default impl re-embeds each mnemonic and memorizes it, then reconnects
    /// links by mnemonic in a second pass. Backends that persist UUIDs (SQLite)
    /// override this for exact roundtrips.
    async fn import(&self, dir: &Path, embedder: &mut Embedder) -> Result<ImportResult> {
        if !dir.is_dir() {
            return Err(anyhow::anyhow!("not a directory: {}", dir.display()));
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        entries.sort_by_key(|e| e.path());

        let parsed: Vec<portable::PortableMemory> = entries
            .iter()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|raw| portable::from_markdown(&raw))
            .collect();

        let mut result = ImportResult::default();
        // Pass 1: memories + aliases.
        for pm in &parsed {
            let existed = self.get_memory_by_mnemonic(&pm.mnemonic).await?.is_some();
            let embedding = embedder.embed(&pm.mnemonic)?;
            self.memorize(&pm.mnemonic, &pm.content, &pm.tags, &embedding).await?;
            for alias in &pm.mnemonics {
                if alias == &pm.mnemonic {
                    continue;
                }
                let alias_emb = embedder.embed(alias)?;
                let _ = self.add_mnemonic(&pm.mnemonic, alias, &alias_emb).await;
            }
            if existed {
                result.updated += 1;
            } else {
                result.created += 1;
            }
        }
        // Pass 2: links by mnemonic.
        for pm in &parsed {
            for link in &pm.links {
                let _ = self.link(&pm.mnemonic, &link.target, &link.link_type).await;
            }
        }
        Ok(result)
    }
}

/// The memory + auth backend pair used by the web server.
pub struct Backends {
    pub memory: Arc<dyn MemoryBackend>,
    pub auth: Arc<dyn AuthBackend>,
}

/// Resolve the backend name from (in priority order) an explicit override, the
/// `TRIVIA_BACKEND` env var, config, then the "sqlite" default.
fn resolve_backend_name(config: &TriviaConfig, override_name: Option<&str>) -> String {
    override_name
        .map(str::to_string)
        .or_else(|| std::env::var("TRIVIA_BACKEND").ok())
        .or_else(|| config.backend.clone())
        .unwrap_or_else(|| "sqlite".to_string())
        .to_lowercase()
}

#[cfg(feature = "s3vectors")]
fn env_or(config: Option<&str>, var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|s| !s.is_empty()).or_else(|| config.map(str::to_string))
}

/// Construct the memory backend selected by config/env/override.
///
/// `db_path` is the SQLite database path (used by the sqlite backend). Selecting
/// "s3vectors" without the `s3vectors` cargo feature is an error.
pub async fn build_memory_backend(
    config: &TriviaConfig,
    db_path: &Path,
    scoring: ScoringConfig,
    override_name: Option<&str>,
) -> Result<Arc<dyn MemoryBackend>> {
    match resolve_backend_name(config, override_name).as_str() {
        "sqlite" => Ok(Arc::new(SqliteBackend::open(db_path, scoring)?)),
        "s3vectors" | "s3" => build_s3_memory_backend(config, scoring).await,
        other => Err(anyhow!(
            "unknown backend '{other}' (expected 'sqlite' or 's3vectors')"
        )),
    }
}

/// Construct both the memory and auth backends. In sqlite mode a single store
/// serves both (one connection); otherwise auth uses a local SQLite database.
pub async fn build_backends(
    config: &TriviaConfig,
    db_path: &Path,
    scoring: ScoringConfig,
    override_name: Option<&str>,
) -> Result<Backends> {
    match resolve_backend_name(config, override_name).as_str() {
        "sqlite" => {
            let store = Arc::new(SqliteBackend::open(db_path, scoring)?);
            Ok(Backends {
                memory: store.clone(),
                auth: store,
            })
        }
        "s3vectors" | "s3" => {
            let memory = build_s3_memory_backend(config, scoring).await?;
            // Auth stays local: the web server always needs a relational store
            // for users/tokens/sessions, which S3 Vectors cannot provide.
            let auth: Arc<dyn AuthBackend> =
                Arc::new(SqliteBackend::open(db_path, ScoringConfig::default())?);
            Ok(Backends { memory, auth })
        }
        other => Err(anyhow!(
            "unknown backend '{other}' (expected 'sqlite' or 's3vectors')"
        )),
    }
}

#[cfg(feature = "s3vectors")]
async fn build_s3_memory_backend(
    config: &TriviaConfig,
    scoring: ScoringConfig,
) -> Result<Arc<dyn MemoryBackend>> {
    let bucket = env_or(config.s3vectors.bucket.as_deref(), "TRIVIA_S3_BUCKET").ok_or_else(|| {
        anyhow!("s3vectors backend requires a bucket ([s3vectors].bucket or TRIVIA_S3_BUCKET)")
    })?;
    let index = env_or(config.s3vectors.index.as_deref(), "TRIVIA_S3_INDEX").ok_or_else(|| {
        anyhow!("s3vectors backend requires an index ([s3vectors].index or TRIVIA_S3_INDEX)")
    })?;
    let region = env_or(config.s3vectors.region.as_deref(), "TRIVIA_S3_REGION");
    let cfg = s3vectors::S3VectorsConfig {
        bucket,
        index,
        region,
    };
    Ok(Arc::new(s3vectors::S3VectorsBackend::connect(cfg, scoring).await?))
}

#[cfg(not(feature = "s3vectors"))]
async fn build_s3_memory_backend(
    _config: &TriviaConfig,
    _scoring: ScoringConfig,
) -> Result<Arc<dyn MemoryBackend>> {
    Err(anyhow!(
        "this build was compiled without S3 Vectors support; rebuild with \
         `--features s3vectors` to use backend = \"s3vectors\""
    ))
}

/// Portable markdown (de)serialization for the default export/import path.
mod portable {
    use crate::store::Memory;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Serialize, Deserialize)]
    struct Frontmatter {
        mnemonic: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mnemonics: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<Link>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Link {
        target: String,
        #[serde(rename = "type")]
        link_type: String,
    }

    pub struct PortableLink {
        pub target: String,
        pub link_type: String,
    }

    pub struct PortableMemory {
        pub mnemonic: String,
        pub content: String,
        pub tags: Vec<String>,
        pub mnemonics: Vec<String>,
        pub links: Vec<PortableLink>,
    }

    pub fn to_markdown(mem: &Memory) -> String {
        let mnemonics: Vec<String> = mem
            .mnemonics
            .iter()
            .filter(|m| **m != mem.mnemonic)
            .cloned()
            .collect();
        // Only outbound links, to avoid writing each edge twice.
        let links: Vec<Link> = mem
            .links
            .iter()
            .filter(|l| l.source_mnemonic == mem.mnemonic)
            .map(|l| Link {
                target: l.target_mnemonic.clone(),
                link_type: l.link_type.clone(),
            })
            .collect();
        let fm = Frontmatter {
            mnemonic: mem.mnemonic.clone(),
            mnemonics,
            tags: mem.tags.clone(),
            links,
        };
        let yaml = serde_norway::to_string(&fm).unwrap_or_default();
        format!("---\n{yaml}---\n\n{}", mem.content)
    }

    pub fn from_markdown(raw: &str) -> Option<PortableMemory> {
        let rest = raw.strip_prefix("---\n")?;
        let end = rest.find("\n---")?;
        let yaml = &rest[..end];
        let body = rest[end + 4..].trim_start_matches('\n');
        let fm: Frontmatter = serde_norway::from_str(yaml).ok()?;
        Some(PortableMemory {
            mnemonic: fm.mnemonic,
            content: body.to_string(),
            tags: fm.tags,
            mnemonics: fm.mnemonics,
            links: fm
                .links
                .into_iter()
                .map(|l| PortableLink {
                    target: l.target,
                    link_type: l.link_type,
                })
                .collect(),
        })
    }
}

/// The web server's auth / OAuth / session store. SQLite-only today.
#[async_trait]
pub trait AuthBackend: Send + Sync {
    async fn create_user(&self, username: &str, acl: &str) -> Result<User>;
    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn get_user_by_id(&self, id: i64) -> Result<Option<User>>;
    async fn update_user_acl(&self, username: &str, acl: &str) -> Result<()>;
    async fn list_users(&self) -> Result<Vec<User>>;
    async fn delete_user(&self, username: &str) -> Result<bool>;

    async fn create_provider(
        &self,
        name: &str,
        provider_type: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<OAuthProvider>;
    async fn get_provider_by_name(&self, name: &str) -> Result<Option<OAuthProvider>>;
    async fn list_providers(&self) -> Result<Vec<OAuthProvider>>;
    async fn delete_provider(&self, name: &str) -> Result<bool>;

    async fn link_identity(
        &self,
        user_id: i64,
        provider_id: i64,
        provider_username: &str,
        provider_user_id: &str,
    ) -> Result<()>;
    async fn get_user_by_provider_identity(
        &self,
        provider_id: i64,
        provider_user_id: &str,
    ) -> Result<Option<User>>;
    async fn list_identities_for_user(&self, user_id: i64) -> Result<Vec<UserIdentity>>;

    async fn register_client(
        &self,
        redirect_uris: &[String],
        client_name: Option<&str>,
    ) -> Result<(OAuthClient, Option<String>)>;
    async fn get_client(&self, client_id: &str) -> Result<Option<OAuthClient>>;
    async fn verify_client_secret(&self, client_id: &str, secret: &str) -> Result<bool>;

    async fn create_auth_code(
        &self,
        client_id: &str,
        user_id: i64,
        code_challenge: &str,
        redirect_uri: &str,
    ) -> Result<String>;
    async fn consume_auth_code(&self, code: &str) -> Result<OAuthCode>;

    async fn create_token_pair(&self, client_id: &str, user_id: i64) -> Result<TokenPair>;
    async fn get_user_by_access_token(&self, token: &str) -> Result<Option<User>>;
    async fn get_user_by_refresh_token(&self, token: &str) -> Result<Option<(User, String)>>;
    async fn revoke_refresh_token(&self, token: &str) -> Result<()>;
    async fn cleanup_expired_tokens(&self) -> Result<usize>;

    async fn create_session(&self, user_id: i64) -> Result<Session>;
    async fn get_session(&self, session_id: &str) -> Result<Option<(Session, User)>>;
    async fn delete_session(&self, session_id: &str) -> Result<()>;
    async fn cleanup_expired_sessions(&self) -> Result<usize>;

    async fn has_auth_providers(&self) -> Result<bool>;
    async fn list_enabled_providers(&self) -> Result<Vec<(String, String)>>;
    async fn cleanup_expired_codes(&self) -> Result<usize>;
}
