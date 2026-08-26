//! Amazon S3 Vectors implementation of [`MemoryBackend`].
//!
//! S3 Vectors is a network-backed, KNN-only vector store. It provides the
//! *initial retrieval* — [`recall_candidates`](MemoryBackend::recall_candidates)
//! runs `QueryVectors`; the pure [`crate::rerank`] then scores the results, just
//! as it does for SQLite. It has no lexical/FTS search, so `fts_matches` is
//! always empty and the FTS boost is inert (documented limitation).
//!
//! **Data model.** One vector per mnemonic; the parent memory's full record is
//! denormalized into each mnemonic vector's metadata under a single
//! non-filterable key, `record` (a JSON blob). The primary mnemonic's vector
//! uses the memory title as its key; aliases key on their own text. Tag and
//! link graphs live inside the record, so link edges are stored on *both*
//! endpoints for O(1) reads during recall.
//!
//! **Provisioning.** The vector bucket and index are expected to already exist
//! (e.g. via Terraform's `aws_s3vectors_vector_bucket` / index resources), with
//! dimension 384, the `euclidean` distance metric (to match SQLite-vec's L2 and
//! the `similarity = 1 - distance` rerank convention), and `record` marked as a
//! non-filterable metadata key. The backend validates access on connect but does
//! not create infrastructure.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3vectors::Client;
use aws_sdk_s3vectors::types::{PutInputVector, VectorData};
use aws_smithy_types::Document;

use super::MemoryBackend;
use crate::store::{
    EditResult, Memory, MemoryLink, MemorizeNeighbor, MemorizeResult, MemorySummary,
    MergeCandidate, RecallCandidates, ScoringConfig, TagCount,
};

// Mirror the SQLite backend's auto-merge/auto-link thresholds so behavior is
// consistent across backends (euclidean distance).
const AUTO_LINK_THRESHOLD: f64 = 0.3;
const AUTO_MERGE_THRESHOLD: f64 = 0.15;
// Neighbors to consider for auto-merge/link on memorize (matches SQLite's fan-out).
const MEMORIZE_NEIGHBORS: usize = 18;

/// Connection parameters for the S3 Vectors backend.
#[derive(Debug, Clone)]
pub struct S3VectorsConfig {
    pub bucket: String,
    pub index: String,
    pub region: Option<String>,
}

/// The denormalized memory record stored as JSON in each vector's metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct S3Record {
    title: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    mnemonics: Vec<String>,
    uuid: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    recall_count: i64,
    #[serde(default)]
    last_recalled_at: Option<String>,
    #[serde(default)]
    useful_count: i64,
    #[serde(default)]
    not_useful_count: i64,
    #[serde(default)]
    links: Vec<S3Link>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct S3Link {
    source: String,
    target: String,
    #[serde(rename = "type")]
    link_type: String,
    created_at: String,
}

pub struct S3VectorsBackend {
    client: Client,
    bucket: String,
    index: String,
    scoring: ScoringConfig,
}

impl S3VectorsBackend {
    /// Connect to an existing S3 vector bucket + index and validate access.
    pub async fn connect(cfg: S3VectorsConfig, scoring: ScoringConfig) -> Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(region) = &cfg.region {
            loader = loader.region(Region::new(region.clone()));
        }
        let shared = loader.load().await;
        let client = Client::new(&shared);

        let backend = Self {
            client,
            bucket: cfg.bucket,
            index: cfg.index,
            scoring,
        };

        // Fail fast with a helpful message if the index isn't reachable.
        backend
            .client
            .get_index()
            .vector_bucket_name(&backend.bucket)
            .index_name(&backend.index)
            .send()
            .await
            .map_err(|e| {
                anyhow!(
                    "cannot access S3 vector index '{}' in bucket '{}': {e}. \
                     Ensure the bucket and index exist (e.g. provisioned via Terraform) \
                     with dimension 384, the euclidean distance metric, and 'record' \
                     configured as a non-filterable metadata key.",
                    backend.index,
                    backend.bucket
                )
            })?;

        Ok(backend)
    }

    // ---- low-level S3 Vectors helpers ----

    async fn query(&self, embedding: &[f32], top_k: usize) -> Result<Vec<(f64, S3Record)>> {
        let out = self
            .client
            .query_vectors()
            .vector_bucket_name(&self.bucket)
            .index_name(&self.index)
            .query_vector(VectorData::Float32(embedding.to_vec()))
            .top_k(cap_top_k(top_k))
            .return_metadata(true)
            .return_distance(true)
            .send()
            .await
            .map_err(|e| anyhow!("s3vectors query_vectors: {e}"))?;

        let mut results = Vec::new();
        for v in out.vectors() {
            if let Some(rec) = parse_record(v.metadata()) {
                results.push((v.distance().unwrap_or(0.0) as f64, rec));
            }
        }
        Ok(results)
    }

    /// Fetch mnemonic vectors (with embeddings) for the given keys.
    async fn get_memory_vectors(
        &self,
        keys: &[String],
    ) -> Result<Vec<(String, Vec<f32>, S3Record)>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let out = self
            .client
            .get_vectors()
            .vector_bucket_name(&self.bucket)
            .index_name(&self.index)
            .set_keys(Some(keys.to_vec()))
            .return_data(true)
            .return_metadata(true)
            .send()
            .await
            .map_err(|e| anyhow!("s3vectors get_vectors: {e}"))?;

        let mut results = Vec::new();
        for v in out.vectors() {
            let data = match v.data() {
                Some(VectorData::Float32(d)) => d.clone(),
                _ => continue,
            };
            if let Some(rec) = parse_record(v.metadata()) {
                results.push((v.key().to_string(), data, rec));
            }
        }
        Ok(results)
    }

    /// Load a single memory's record by any of its mnemonic keys.
    async fn load_record(&self, key: &str) -> Result<Option<S3Record>> {
        let out = self
            .client
            .get_vectors()
            .vector_bucket_name(&self.bucket)
            .index_name(&self.index)
            .keys(key.to_string())
            .return_metadata(true)
            .send()
            .await
            .map_err(|e| anyhow!("s3vectors get_vectors: {e}"))?;
        Ok(out.vectors().iter().find_map(|v| parse_record(v.metadata())))
    }

    /// Enumerate every memory (deduped by title) via paginated `ListVectors`.
    async fn list_all_records(&self) -> Result<Vec<S3Record>> {
        let mut token: Option<String> = None;
        let mut by_title: HashMap<String, S3Record> = HashMap::new();
        loop {
            let mut req = self
                .client
                .list_vectors()
                .vector_bucket_name(&self.bucket)
                .index_name(&self.index)
                .return_metadata(true)
                .max_results(500);
            if let Some(t) = &token {
                req = req.next_token(t.clone());
            }
            let out = req
                .send()
                .await
                .map_err(|e| anyhow!("s3vectors list_vectors: {e}"))?;
            for v in out.vectors() {
                if let Some(rec) = parse_record(v.metadata()) {
                    by_title.entry(rec.title.clone()).or_insert(rec);
                }
            }
            match out.next_token() {
                Some(t) => token = Some(t.to_string()),
                None => break,
            }
        }
        Ok(by_title.into_values().collect())
    }

    async fn put_inputs(&self, inputs: Vec<PutInputVector>) -> Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }
        // S3 Vectors caps PutVectors at 500 vectors per call.
        for chunk in inputs.chunks(500) {
            let mut req = self
                .client
                .put_vectors()
                .vector_bucket_name(&self.bucket)
                .index_name(&self.index);
            for v in chunk {
                req = req.vectors(v.clone());
            }
            req.send()
                .await
                .map_err(|e| anyhow!("s3vectors put_vectors: {e}"))?;
        }
        Ok(())
    }

    async fn put_one(&self, key: String, data: Vec<f32>, record: &S3Record) -> Result<()> {
        self.put_inputs(vec![put_input(key, data, record)?]).await
    }

    async fn delete_keys(&self, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.client
            .delete_vectors()
            .vector_bucket_name(&self.bucket)
            .index_name(&self.index)
            .set_keys(Some(keys.to_vec()))
            .send()
            .await
            .map_err(|e| anyhow!("s3vectors delete_vectors: {e}"))?;
        Ok(())
    }

    /// Re-put every mnemonic vector of a memory with `record`, preserving each
    /// vector's existing embedding. Used for metadata-only mutations.
    async fn rewrite_memory(&self, record: &S3Record) -> Result<()> {
        let existing = self.get_memory_vectors(&record.mnemonics).await?;
        let mut inputs = Vec::new();
        for (key, data, _) in existing {
            inputs.push(put_input(key, data, record)?);
        }
        self.put_inputs(inputs).await
    }

    /// Load, mutate, and rewrite a memory record in place. Returns false if the
    /// memory doesn't exist.
    async fn mutate_record(
        &self,
        title: &str,
        f: impl FnOnce(&mut S3Record) + Send,
    ) -> Result<bool> {
        let Some(mut rec) = self.load_record(title).await? else {
            return Ok(false);
        };
        f(&mut rec);
        self.rewrite_memory(&rec).await?;
        Ok(true)
    }

    async fn memorize_inner(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
        skip_merge: bool,
    ) -> Result<MemorizeResult> {
        let now = now_ts();

        // Is this mnemonic already stored?
        let existing = self
            .get_memory_vectors(std::slice::from_ref(&mnemonic.to_string()))
            .await?
            .into_iter()
            .next();
        let self_title = existing.as_ref().map(|(_, _, r)| r.title.clone());

        // Neighbors (queried before the upsert so a new memory never self-matches).
        let raw_neighbors = self.query(embedding, MEMORIZE_NEIGHBORS).await?;
        let mut seen = HashSet::new();
        let mut neighbors: Vec<(f64, S3Record)> = Vec::new();
        for (dist, rec) in raw_neighbors {
            if Some(&rec.title) == self_title.as_ref() {
                continue;
            }
            if self_title.is_none() && rec.title == mnemonic {
                continue;
            }
            if seen.insert(rec.title.clone()) {
                neighbors.push((dist, rec));
            }
        }

        // Upsert the memory record.
        let memory_title = if let Some((_, _, mut rec)) = existing {
            rec.content = content.to_string();
            rec.tags = tags.to_vec();
            rec.updated_at = now.clone();
            let others: Vec<String> =
                rec.mnemonics.iter().filter(|m| *m != mnemonic).cloned().collect();
            let existing_others = self.get_memory_vectors(&others).await?;
            let mut inputs = vec![put_input(mnemonic.to_string(), embedding.to_vec(), &rec)?];
            for (key, data, _) in existing_others {
                inputs.push(put_input(key, data, &rec)?);
            }
            self.put_inputs(inputs).await?;
            rec.title.clone()
        } else {
            let rec = S3Record {
                title: mnemonic.to_string(),
                content: content.to_string(),
                tags: tags.to_vec(),
                mnemonics: vec![mnemonic.to_string()],
                uuid: Uuid::new_v4().to_string(),
                created_at: now.clone(),
                updated_at: now.clone(),
                recall_count: 0,
                last_recalled_at: None,
                useful_count: 0,
                not_useful_count: 0,
                links: Vec::new(),
            };
            self.put_one(mnemonic.to_string(), embedding.to_vec(), &rec).await?;
            mnemonic.to_string()
        };

        let result_neighbors: Vec<MemorizeNeighbor> = if skip_merge {
            Vec::new()
        } else {
            neighbors
                .iter()
                .filter(|(d, _)| *d < AUTO_LINK_THRESHOLD)
                .map(|(d, r)| MemorizeNeighbor {
                    mnemonic: r.title.clone(),
                    distance: *d,
                    tags: r.tags.clone(),
                })
                .collect()
        };

        // Auto-merge the single closest neighbor under the merge threshold;
        // otherwise auto-link everything under the link threshold.
        let merge_candidate = if skip_merge {
            None
        } else {
            neighbors.iter().find(|(d, _)| *d < AUTO_MERGE_THRESHOLD).cloned()
        };

        let merged_with = if let Some((_, cand)) = merge_candidate {
            self.merge(&memory_title, &cand.title, embedding).await?;
            Some(cand.title)
        } else {
            for (d, r) in &neighbors {
                if *d < AUTO_LINK_THRESHOLD {
                    self.link(&memory_title, &r.title, "related").await?;
                }
            }
            None
        };

        Ok(MemorizeResult {
            merged_with,
            neighbors: result_neighbors,
        })
    }
}

#[async_trait]
impl MemoryBackend for S3VectorsBackend {
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
        self.memorize_inner(mnemonic, content, tags, embedding, false).await
    }

    async fn memorize_with_options(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
        skip_merge: bool,
    ) -> Result<MemorizeResult> {
        self.memorize_inner(mnemonic, content, tags, embedding, skip_merge).await
    }

    async fn recall_candidates(
        &self,
        embedding: &[f32],
        limit: usize,
        tags: Option<&[String]>,
        _fts_query: Option<&str>,
        exclude_tags: Option<&[String]>,
    ) -> Result<RecallCandidates> {
        // Overfetch for reranking; widen when tag-filtering client-side.
        let base = limit.max(1) * 5;
        let fetch = if tags.is_some() { base * 4 } else { base };

        let hits = self.query(embedding, fetch).await?;
        let mut seen = HashSet::new();
        let mut memories: Vec<Memory> = Vec::new();
        for (dist, rec) in hits {
            if !seen.insert(rec.title.clone()) {
                continue;
            }
            if let Some(want) = tags
                && !want.iter().any(|t| rec.tags.contains(t))
            {
                continue;
            }
            if let Some(excl) = exclude_tags
                && excl.iter().any(|t| rec.tags.contains(t))
            {
                continue;
            }
            memories.push(record_to_memory(rec, dist));
        }

        // S3 Vectors has no lexical search — the FTS boost is inert here.
        Ok(RecallCandidates {
            memories,
            fts_matches: HashSet::new(),
        })
    }

    async fn bump_recall_stats(&self, titles: &[String]) -> Result<()> {
        let now = now_ts();
        for title in titles {
            let _ = self
                .mutate_record(title, |r| {
                    r.recall_count += 1;
                    r.last_recalled_at = Some(now.clone());
                })
                .await?;
        }
        Ok(())
    }

    async fn get_memory_by_mnemonic(&self, title: &str) -> Result<Option<Memory>> {
        Ok(self.load_record(title).await?.map(|r| record_to_memory(r, 0.0)))
    }

    async fn delete_memory(&self, title: &str) -> Result<bool> {
        let Some(rec) = self.load_record(title).await? else {
            return Ok(false);
        };
        // Detach edges from partners.
        let partners = link_partners(&rec.links, &rec.title);
        for p in partners {
            if let Some(mut pr) = self.load_record(&p).await? {
                pr.links
                    .retain(|l| l.source != rec.title && l.target != rec.title);
                self.rewrite_memory(&pr).await?;
            }
        }
        self.delete_keys(&rec.mnemonics).await?;
        Ok(true)
    }

    async fn rate(&self, title: &str, useful: bool) -> Result<()> {
        let found = self
            .mutate_record(title, |r| {
                if useful {
                    r.useful_count += 1;
                } else {
                    r.not_useful_count += 1;
                }
            })
            .await?;
        if !found {
            return Err(anyhow!("memory not found: {title}"));
        }
        Ok(())
    }

    async fn rate_batch(&self, titles: &[String], useful: bool) -> Result<Vec<String>> {
        let mut not_found = Vec::new();
        for title in titles {
            let found = self
                .mutate_record(title, |r| {
                    if useful {
                        r.useful_count += 1;
                    } else {
                        r.not_useful_count += 1;
                    }
                })
                .await?;
            if !found {
                not_found.push(title.clone());
            }
        }
        Ok(not_found)
    }

    async fn link(&self, source: &str, target: &str, link_type: &str) -> Result<()> {
        if source == target {
            return Ok(());
        }
        let mut src = self
            .load_record(source)
            .await?
            .ok_or_else(|| anyhow!("source not found: {source}"))?;
        let mut tgt = self
            .load_record(target)
            .await?
            .ok_or_else(|| anyhow!("target not found: {target}"))?;
        // Use canonical titles so aliases resolve to their memory.
        let edge = S3Link {
            source: src.title.clone(),
            target: tgt.title.clone(),
            link_type: link_type.to_string(),
            created_at: now_ts(),
        };
        if src.title == tgt.title {
            return Ok(());
        }
        if push_unique_edge(&mut src.links, &edge) {
            self.rewrite_memory(&src).await?;
        }
        if push_unique_edge(&mut tgt.links, &edge) {
            self.rewrite_memory(&tgt).await?;
        }
        Ok(())
    }

    async fn unlink(&self, source: &str, target: &str, link_type: &str) -> Result<()> {
        let matches = |l: &S3Link| {
            l.source == source && l.target == target && l.link_type == link_type
        };
        if let Some(mut src) = self.load_record(source).await? {
            let before = src.links.len();
            src.links.retain(|l| !matches(l));
            if src.links.len() != before {
                self.rewrite_memory(&src).await?;
            }
        }
        if let Some(mut tgt) = self.load_record(target).await? {
            let before = tgt.links.len();
            tgt.links.retain(|l| !matches(l));
            if tgt.links.len() != before {
                self.rewrite_memory(&tgt).await?;
            }
        }
        Ok(())
    }

    async fn get_links(&self, title: &str) -> Result<Vec<MemoryLink>> {
        Ok(self
            .load_record(title)
            .await?
            .map(|r| r.links.iter().map(link_to_memory_link).collect())
            .unwrap_or_default())
    }

    async fn get_all_links(&self) -> Result<Vec<MemoryLink>> {
        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for rec in self.list_all_records().await? {
            for l in &rec.links {
                let key = (l.source.clone(), l.target.clone(), l.link_type.clone());
                if seen.insert(key) {
                    links.push(link_to_memory_link(l));
                }
            }
        }
        Ok(links)
    }

    async fn merge(&self, keep: &str, discard: &str, embedding: &[f32]) -> Result<()> {
        let keep_rec = self
            .load_record(keep)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {keep}"))?;
        let discard_rec = self
            .load_record(discard)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {discard}"))?;
        let keep_title = keep_rec.title.clone();
        let discard_title = discard_rec.title.clone();

        let mut merged = keep_rec.clone();
        merged.content = format!("{}\n\n{}", keep_rec.content, discard_rec.content);
        for t in &discard_rec.tags {
            if !merged.tags.contains(t) {
                merged.tags.push(t.clone());
            }
        }
        for m in &discard_rec.mnemonics {
            if !merged.mnemonics.contains(m) {
                merged.mnemonics.push(m.clone());
            }
        }
        merged.updated_at = now_ts();
        let combined: Vec<S3Link> = keep_rec
            .links
            .iter()
            .cloned()
            .chain(discard_rec.links.iter().cloned())
            .collect();
        merged.links = retarget_links(combined, &discard_title, &keep_title);

        // Rewrite all of keep's + discard's mnemonic vectors under the merged
        // record (discard's vectors become keep's aliases). The primary keeps
        // its freshly re-embedded vector.
        let existing = self.get_memory_vectors(&merged.mnemonics).await?;
        let mut inputs = Vec::new();
        for (key, data, _) in existing {
            let data = if key == keep_title {
                embedding.to_vec()
            } else {
                data
            };
            inputs.push(put_input(key, data, &merged)?);
        }
        self.put_inputs(inputs).await?;

        // Retarget partners that referenced discard.
        for p in link_partners(&discard_rec.links, &discard_title) {
            if p == keep_title {
                continue;
            }
            if let Some(mut pr) = self.load_record(&p).await? {
                pr.links = retarget_links(pr.links, &discard_title, &keep_title);
                self.rewrite_memory(&pr).await?;
            }
        }
        Ok(())
    }

    async fn add_mnemonic(&self, title: &str, text: &str, embedding: &[f32]) -> Result<()> {
        let mut rec = self
            .load_record(title)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {title}"))?;
        if !rec.mnemonics.iter().any(|m| m == text) {
            rec.mnemonics.push(text.to_string());
        }
        rec.updated_at = now_ts();
        let others: Vec<String> =
            rec.mnemonics.iter().filter(|m| *m != text).cloned().collect();
        let existing_others = self.get_memory_vectors(&others).await?;
        let mut inputs = vec![put_input(text.to_string(), embedding.to_vec(), &rec)?];
        for (key, data, _) in existing_others {
            inputs.push(put_input(key, data, &rec)?);
        }
        self.put_inputs(inputs).await
    }

    async fn remove_mnemonic(&self, title: &str, text: &str) -> Result<()> {
        let mut rec = self
            .load_record(title)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {title}"))?;
        if rec.mnemonics.len() <= 1 {
            return Err(anyhow!("cannot remove the last mnemonic"));
        }
        if text == rec.title {
            return Err(anyhow!("cannot remove the primary mnemonic"));
        }
        if !rec.mnemonics.iter().any(|m| m == text) {
            return Ok(());
        }
        rec.mnemonics.retain(|m| m != text);
        rec.updated_at = now_ts();
        self.delete_keys(std::slice::from_ref(&text.to_string())).await?;
        self.rewrite_memory(&rec).await
    }

    async fn update_memory(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<()> {
        let mut rec = self
            .load_record(title)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {title}"))?;
        rec.content = content.to_string();
        rec.tags = tags.to_vec();
        rec.updated_at = now_ts();
        let primary = rec.title.clone();
        let others: Vec<String> =
            rec.mnemonics.iter().filter(|m| **m != primary).cloned().collect();
        let existing_others = self.get_memory_vectors(&others).await?;
        let mut inputs = vec![put_input(primary, embedding.to_vec(), &rec)?];
        for (key, data, _) in existing_others {
            inputs.push(put_input(key, data, &rec)?);
        }
        self.put_inputs(inputs).await
    }

    async fn rename_memory(
        &self,
        old_title: &str,
        new_title: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let mut rec = self
            .load_record(old_title)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {old_title}"))?;
        if !self
            .get_memory_vectors(std::slice::from_ref(&new_title.to_string()))
            .await?
            .is_empty()
        {
            return Err(anyhow!("title already exists: {new_title}"));
        }
        let old_primary = rec.title.clone();
        let original_links = rec.links.clone();

        let vectors = self.get_memory_vectors(&rec.mnemonics).await?;
        rec.title = new_title.to_string();
        for m in rec.mnemonics.iter_mut() {
            if *m == old_primary {
                *m = new_title.to_string();
            }
        }
        rec.links = retarget_links(rec.links.clone(), &old_primary, new_title);
        rec.updated_at = now_ts();

        let mut inputs = Vec::new();
        for (key, data, _) in vectors {
            if key == old_primary {
                inputs.push(put_input(new_title.to_string(), embedding.to_vec(), &rec)?);
            } else {
                inputs.push(put_input(key, data, &rec)?);
            }
        }
        self.put_inputs(inputs).await?;
        self.delete_keys(std::slice::from_ref(&old_primary)).await?;

        for p in link_partners(&original_links, &old_primary) {
            if let Some(mut pr) = self.load_record(&p).await? {
                pr.links = retarget_links(pr.links, &old_primary, new_title);
                self.rewrite_memory(&pr).await?;
            }
        }
        Ok(())
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
        if self.load_record(title).await?.is_none() {
            return Err(anyhow!("memory not found: {title}"));
        }

        // Tags first.
        if !add_tags.is_empty() || !remove_tags.is_empty() {
            self.mutate_record(title, |r| {
                for t in add_tags {
                    if !r.tags.contains(t) {
                        r.tags.push(t.clone());
                    }
                }
                r.tags.retain(|t| !remove_tags.contains(t));
            })
            .await?;
        }

        // Add/remove aliases while the title is still stable.
        for (i, text) in add_mnemonics.iter().enumerate() {
            let emb = mnemonic_embeddings
                .get(i)
                .ok_or_else(|| anyhow!("missing embedding for mnemonic '{text}'"))?;
            self.add_mnemonic(title, text, emb).await?;
        }
        for text in remove_mnemonics {
            self.remove_mnemonic(title, text).await?;
        }

        // Rename last.
        let final_title = if let Some(new_t) = new_title {
            let emb = new_embedding
                .ok_or_else(|| anyhow!("new_embedding required when changing title"))?;
            self.rename_memory(title, new_t, emb).await?;
            new_t.to_string()
        } else {
            title.to_string()
        };

        let rec = self
            .load_record(&final_title)
            .await?
            .ok_or_else(|| anyhow!("memory not found: {final_title}"))?;

        Ok(EditResult {
            old_mnemonic: title.to_string(),
            new_mnemonic: final_title,
            tags: rec.tags,
            mnemonics: rec.mnemonics,
            re_embedded: new_title.is_some(),
        })
    }

    async fn rename_tag(&self, old_tag: &str, new_tag: &str) -> Result<usize> {
        let mut count = 0;
        for rec in self.list_all_records().await? {
            if !rec.tags.iter().any(|t| t == old_tag) {
                continue;
            }
            self.mutate_record(&rec.title, |r| {
                r.tags.retain(|t| t != old_tag);
                if !r.tags.iter().any(|t| t == new_tag) {
                    r.tags.push(new_tag.to_string());
                }
            })
            .await?;
            count += 1;
        }
        Ok(count)
    }

    async fn list_tags(&self) -> Result<Vec<TagCount>> {
        let mut counts: HashMap<String, i64> = HashMap::new();
        for rec in self.list_all_records().await? {
            for t in rec.tags {
                *counts.entry(t).or_insert(0) += 1;
            }
        }
        let mut tags: Vec<TagCount> = counts
            .into_iter()
            .map(|(tag, count)| TagCount { tag, count })
            .collect();
        tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        Ok(tags)
    }

    async fn list_all_summaries(&self) -> Result<Vec<MemorySummary>> {
        let mut records = self.list_all_records().await?;
        records.sort_by(|a, b| {
            b.recall_count
                .cmp(&a.recall_count)
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        Ok(records
            .into_iter()
            .map(|r| MemorySummary {
                mnemonic: r.title,
                content: r.content,
                tags: r.tags,
                mnemonics: r.mnemonics,
                recall_count: r.recall_count,
                useful_count: r.useful_count,
                not_useful_count: r.not_useful_count,
            })
            .collect())
    }

    async fn find_nearest(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude_title: &str,
    ) -> Result<Vec<(String, f64)>> {
        let hits = self.query(embedding, MEMORIZE_NEIGHBORS).await?;
        let mut seen = HashSet::new();
        Ok(hits
            .into_iter()
            .filter(|(dist, rec)| {
                rec.title != exclude_title && *dist < threshold && seen.insert(rec.title.clone())
            })
            .map(|(dist, rec)| (rec.title, dist))
            .collect())
    }

    async fn find_merge_candidates(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude: &HashSet<String>,
        limit: usize,
    ) -> Result<Vec<MergeCandidate>> {
        let hits = self.query(embedding, (limit + exclude.len() + 1) * 3).await?;
        let mut seen = HashSet::new();
        Ok(hits
            .into_iter()
            .filter(|(dist, rec)| {
                !exclude.contains(&rec.title)
                    && *dist < threshold
                    && seen.insert(rec.title.clone())
            })
            .take(limit)
            .map(|(dist, rec)| MergeCandidate {
                mnemonic: rec.title,
                content: rec.content,
                tags: rec.tags,
                distance: dist,
                recall_count: rec.recall_count,
            })
            .collect())
    }
}

// ---- free helpers ----

fn cap_top_k(n: usize) -> i32 {
    n.clamp(1, 100) as i32
}

fn now_ts() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|n| n.and_utc())
        .unwrap_or_default()
}

fn put_input(key: String, data: Vec<f32>, record: &S3Record) -> Result<PutInputVector> {
    Ok(PutInputVector::builder()
        .key(key)
        .data(VectorData::Float32(data))
        .metadata(record_metadata(record)?)
        .build()?)
}

fn record_metadata(record: &S3Record) -> Result<Document> {
    let json = serde_json::to_string(record)?;
    let mut map = HashMap::new();
    map.insert("record".to_string(), Document::String(json));
    Ok(Document::Object(map))
}

fn parse_record(doc: Option<&Document>) -> Option<S3Record> {
    match doc? {
        Document::Object(map) => match map.get("record")? {
            Document::String(s) => serde_json::from_str(s).ok(),
            _ => None,
        },
        _ => None,
    }
}

fn record_to_memory(rec: S3Record, distance: f64) -> Memory {
    let links = rec.links.iter().map(link_to_memory_link).collect();
    Memory {
        mnemonic: rec.title,
        content: rec.content,
        tags: rec.tags,
        mnemonics: rec.mnemonics,
        distance,
        score: 0.0,
        created_at: parse_ts(&rec.created_at),
        updated_at: parse_ts(&rec.updated_at),
        recall_count: rec.recall_count,
        last_recalled_at: rec.last_recalled_at.as_deref().map(parse_ts),
        useful_count: rec.useful_count,
        not_useful_count: rec.not_useful_count,
        links,
    }
}

fn link_to_memory_link(l: &S3Link) -> MemoryLink {
    MemoryLink {
        source_mnemonic: l.source.clone(),
        target_mnemonic: l.target.clone(),
        link_type: l.link_type.clone(),
        created_at: parse_ts(&l.created_at),
    }
}

/// Append `edge` if no equivalent edge already exists. Returns whether it changed.
fn push_unique_edge(links: &mut Vec<S3Link>, edge: &S3Link) -> bool {
    if links
        .iter()
        .any(|l| l.source == edge.source && l.target == edge.target && l.link_type == edge.link_type)
    {
        return false;
    }
    links.push(edge.clone());
    true
}

/// Rename `from` → `to` in every edge, dropping resulting self-loops and dups.
fn retarget_links(links: Vec<S3Link>, from: &str, to: &str) -> Vec<S3Link> {
    let mut out: Vec<S3Link> = Vec::new();
    for mut l in links {
        if l.source == from {
            l.source = to.to_string();
        }
        if l.target == from {
            l.target = to.to_string();
        }
        if l.source == l.target {
            continue;
        }
        push_unique_edge(&mut out, &l);
    }
    out
}

/// Distinct memory titles connected to `title` by the given edges.
fn link_partners(links: &[S3Link], title: &str) -> HashSet<String> {
    let mut partners = HashSet::new();
    for l in links {
        for endpoint in [&l.source, &l.target] {
            if endpoint != title {
                partners.insert(endpoint.clone());
            }
        }
    }
    partners
}
