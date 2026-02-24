use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::{Deserialize, Serialize};
use sqlite_vec::sqlite3_vec_init;
use std::path::Path;
use std::sync::Once;
use uuid::Uuid;
use zerocopy::AsBytes;

static VEC_INIT: Once = Once::new();

const AUTO_LINK_THRESHOLD: f64 = 0.3;
const AUTO_LINK_MAX_NEIGHBORS: usize = 5;
const AUTO_MERGE_THRESHOLD: f64 = 0.15;

#[derive(Debug, Clone)]
pub struct ScoringConfig {
    pub similarity_weight: f64,
    pub recency_weight: f64,
    pub frequency_weight: f64,
    pub link_weight: f64,
    pub half_life_days: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            similarity_weight: 1.0,
            recency_weight: 0.1,
            frequency_weight: 0.05,
            link_weight: 0.1,
            half_life_days: 7.0,
        }
    }
}

fn register_sqlite_vec() {
    VEC_INIT.call_once(|| unsafe {
        #[allow(clippy::missing_transmute_annotations)]
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    });
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub mnemonic: String,
    pub content: String,
    pub tags: Vec<String>,
    pub distance: f64,
    pub score: f64,
    pub updated_at: String,
    pub recall_count: i64,
    pub last_recalled_at: Option<String>,
    pub links: Vec<MemoryLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLink {
    pub source_mnemonic: String,
    pub target_mnemonic: String,
    pub link_type: String,
    pub created_at: String,
}

pub struct MemoryStore {
    conn: Connection,
    scoring: ScoringConfig,
}

fn open_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(())
}

impl MemoryStore {
    pub fn new(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating db directory: {}", parent.display()))?;
        }

        let conn = Connection::open(db_path)
            .with_context(|| format!("opening database: {}", db_path.display()))?;
        open_connection(&conn)?;
        let store = Self {
            conn,
            scoring: ScoringConfig::default(),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        open_connection(&conn)?;
        let store = Self {
            conn,
            scoring: ScoringConfig::default(),
        };
        store.migrate()?;
        Ok(store)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mnemonic TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL,
                tags TEXT DEFAULT '[]',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now')),
                recall_count INTEGER NOT NULL DEFAULT 0,
                last_recalled_at TEXT
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(
                memory_id INTEGER PRIMARY KEY,
                embedding float[384]
            );

            CREATE TABLE IF NOT EXISTS memory_links (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                target_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                link_type TEXT NOT NULL CHECK(link_type IN ('related', 'supersedes', 'derived_from')),
                created_at TEXT DEFAULT (datetime('now')),
                UNIQUE(source_id, target_id, link_type)
            );",
        )?;

        // Handle existing DBs that lack the new columns
        let add_column = |sql: &str| -> Result<()> {
            match self.conn.execute_batch(sql) {
                Ok(_) => Ok(()),
                Err(e) if e.to_string().contains("duplicate column") => Ok(()),
                Err(e) => Err(e.into()),
            }
        };
        add_column("ALTER TABLE memories ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0;")?;
        add_column("ALTER TABLE memories ADD COLUMN last_recalled_at TEXT;")?;
        add_column("ALTER TABLE memories ADD COLUMN uuid TEXT;")?;

        // Backfill UUIDs for existing rows
        self.conn.execute_batch(
            "UPDATE memories SET uuid = lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)),2) || '-' || substr('89ab', abs(random()) % 4 + 1, 1) || substr(hex(randomblob(2)),2) || '-' || hex(randomblob(6))) WHERE uuid IS NULL;"
        )?;

        self.conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_uuid ON memories(uuid);"
        )?;

        Ok(())
    }

    pub fn memorize(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<()> {
        let tags_json = serde_json::to_string(tags)?;

        let tx = self.conn.unchecked_transaction()?;

        // Upsert the memory text
        let new_uuid = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO memories (mnemonic, content, tags, uuid)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(mnemonic) DO UPDATE SET
                content = excluded.content,
                tags = excluded.tags,
                updated_at = datetime('now')",
            params![mnemonic, content, tags_json, new_uuid],
        )?;

        let memory_id: i64 = tx.query_row(
            "SELECT id FROM memories WHERE mnemonic = ?1",
            params![mnemonic],
            |row| row.get(0),
        )?;

        // Delete existing vector if any, then insert new one
        tx.execute(
            "DELETE FROM memory_vectors WHERE memory_id = ?1",
            params![memory_id],
        )?;
        tx.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
            params![memory_id, embedding.as_bytes()],
        )?;

        // After inserting the vector, find nearby memories for auto-linking
        let neighbors: Vec<(i64, f64)> = {
            let mut stmt = tx.prepare(
                "SELECT v.memory_id, v.distance
                 FROM memory_vectors v
                 WHERE v.embedding MATCH ?1
                 AND v.k = ?2
                 ORDER BY v.distance",
            )?;
            stmt.query_map(
                params![embedding.as_bytes(), AUTO_LINK_MAX_NEIGHBORS + 1],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };

        // Check for auto-merge candidate (closest neighbor below merge threshold)
        let merge_candidate: Option<(i64, String, String, String)> = neighbors
            .iter()
            .filter(|(nid, dist)| *nid != memory_id && *dist < AUTO_MERGE_THRESHOLD)
            .next()
            .map(|(nid, _)| {
                tx.query_row(
                    "SELECT id, mnemonic, content, tags FROM memories WHERE id = ?1",
                    params![nid],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
            })
            .transpose()?;

        if let Some((old_id, _old_mnemonic, old_content, old_tags_json)) = merge_candidate {
            // Concatenate content: new + old
            let merged_content = format!("{content}\n\n{old_content}");
            // Union tags
            let old_tags: Vec<String> =
                serde_json::from_str(&old_tags_json).unwrap_or_default();
            let mut merged_tags: Vec<String> = tags.to_vec();
            for t in old_tags {
                if !merged_tags.contains(&t) {
                    merged_tags.push(t);
                }
            }
            let merged_tags_json = serde_json::to_string(&merged_tags)?;

            // Update the new memory with merged content and tags
            tx.execute(
                "UPDATE memories SET content = ?1, tags = ?2, updated_at = datetime('now') WHERE id = ?3",
                params![merged_content, merged_tags_json, memory_id],
            )?;

            // Transfer links from old to new
            tx.execute(
                "UPDATE OR IGNORE memory_links SET source_id = ?1 WHERE source_id = ?2",
                params![memory_id, old_id],
            )?;
            tx.execute(
                "UPDATE OR IGNORE memory_links SET target_id = ?1 WHERE target_id = ?2",
                params![memory_id, old_id],
            )?;
            // Clean up any self-links created by transfer
            tx.execute(
                "DELETE FROM memory_links WHERE source_id = target_id",
                [],
            )?;

            // Create supersedes link
            tx.execute(
                "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type) VALUES (?1, ?2, 'supersedes')",
                params![memory_id, old_id],
            )?;

            // Delete old memory (CASCADE handles vectors + remaining links)
            tx.execute("DELETE FROM memories WHERE id = ?1", params![old_id])?;
        } else {
            // No merge — just auto-link
            for (neighbor_id, distance) in &neighbors {
                if *neighbor_id != memory_id && *distance < AUTO_LINK_THRESHOLD {
                    tx.execute(
                        "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type)
                         VALUES (?1, ?2, 'related')",
                        params![memory_id, neighbor_id],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn link(
        &self,
        source_mnemonic: &str,
        target_mnemonic: &str,
        link_type: &str,
    ) -> Result<()> {
        let source_id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM memories WHERE mnemonic = ?1",
                params![source_mnemonic],
                |row| row.get(0),
            )
            .map_err(|_| anyhow!("source mnemonic not found: {}", source_mnemonic))?;

        let target_id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM memories WHERE mnemonic = ?1",
                params![target_mnemonic],
                |row| row.get(0),
            )
            .map_err(|_| anyhow!("target mnemonic not found: {}", target_mnemonic))?;

        self.conn.execute(
            "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type)
             VALUES (?1, ?2, ?3)",
            params![source_id, target_id, link_type],
        )?;

        Ok(())
    }

    pub fn unlink(
        &self,
        source_mnemonic: &str,
        target_mnemonic: &str,
        link_type: &str,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM memory_links
             WHERE source_id = (SELECT id FROM memories WHERE mnemonic = ?1)
             AND target_id = (SELECT id FROM memories WHERE mnemonic = ?2)
             AND link_type = ?3",
            params![source_mnemonic, target_mnemonic, link_type],
        )?;
        Ok(())
    }

    pub fn get_links(&self, mnemonic: &str) -> Result<Vec<MemoryLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.mnemonic, t.mnemonic, ml.link_type, ml.created_at
             FROM memory_links ml
             JOIN memories s ON s.id = ml.source_id
             JOIN memories t ON t.id = ml.target_id
             WHERE s.mnemonic = ?1 OR t.mnemonic = ?1",
        )?;

        let links = stmt
            .query_map(params![mnemonic], |row| {
                Ok(MemoryLink {
                    source_mnemonic: row.get(0)?,
                    target_mnemonic: row.get(1)?,
                    link_type: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(links)
    }

    pub fn find_nearest(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude_mnemonic: &str,
    ) -> Result<Vec<(String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.mnemonic, v.distance
             FROM memory_vectors v
             JOIN memories m ON m.id = v.memory_id
             WHERE v.embedding MATCH ?1
             AND v.k = ?2
             ORDER BY v.distance",
        )?;

        let results: Vec<(String, f64)> = stmt
            .query_map(
                params![embedding.as_bytes(), AUTO_LINK_MAX_NEIGHBORS + 1],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(mnemonic, distance)| mnemonic != exclude_mnemonic && *distance < threshold)
            .collect();

        Ok(results)
    }

    /// Merge two memories: keep absorbs discard's content, tags, and links.
    /// The embedding should be the re-embedded mnemonic of `keep`.
    pub fn merge(&self, keep: &str, discard: &str, embedding: &[f32]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let (keep_id, keep_content, keep_tags_json): (i64, String, String) = tx
            .query_row(
                "SELECT id, content, tags FROM memories WHERE mnemonic = ?1",
                params![keep],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow!("mnemonic not found: {}", keep))?;

        let (discard_id, discard_content, discard_tags_json): (i64, String, String) = tx
            .query_row(
                "SELECT id, content, tags FROM memories WHERE mnemonic = ?1",
                params![discard],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| anyhow!("mnemonic not found: {}", discard))?;

        // Concatenate content
        let merged_content = format!("{keep_content}\n\n{discard_content}");

        // Union tags
        let keep_tags: Vec<String> = serde_json::from_str(&keep_tags_json).unwrap_or_default();
        let discard_tags: Vec<String> =
            serde_json::from_str(&discard_tags_json).unwrap_or_default();
        let mut merged_tags = keep_tags;
        for t in discard_tags {
            if !merged_tags.contains(&t) {
                merged_tags.push(t);
            }
        }
        let merged_tags_json = serde_json::to_string(&merged_tags)?;

        // Update keep with merged content/tags
        tx.execute(
            "UPDATE memories SET content = ?1, tags = ?2, updated_at = datetime('now') WHERE id = ?3",
            params![merged_content, merged_tags_json, keep_id],
        )?;

        // Re-embed
        tx.execute(
            "DELETE FROM memory_vectors WHERE memory_id = ?1",
            params![keep_id],
        )?;
        tx.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
            params![keep_id, embedding.as_bytes()],
        )?;

        // Transfer links from discard to keep
        tx.execute(
            "UPDATE OR IGNORE memory_links SET source_id = ?1 WHERE source_id = ?2",
            params![keep_id, discard_id],
        )?;
        tx.execute(
            "UPDATE OR IGNORE memory_links SET target_id = ?1 WHERE target_id = ?2",
            params![keep_id, discard_id],
        )?;
        tx.execute(
            "DELETE FROM memory_links WHERE source_id = target_id",
            [],
        )?;

        // Create supersedes link
        tx.execute(
            "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type) VALUES (?1, ?2, 'supersedes')",
            params![keep_id, discard_id],
        )?;

        // Delete discard
        tx.execute("DELETE FROM memories WHERE id = ?1", params![discard_id])?;

        tx.commit()?;
        Ok(())
    }

    pub fn recall(
        &self,
        query_embedding: &[f32],
        limit: usize,
        tags: Option<&[String]>,
    ) -> Result<Vec<Memory>> {
        // Overfetch 3x for composite scoring reranking
        let base_fetch = limit * 3;
        let fetch_limit = match tags {
            Some(_) => base_fetch * 4,
            None => base_fetch,
        };

        let query = "SELECT m.mnemonic, m.content, m.tags, v.distance, m.updated_at, m.recall_count, m.last_recalled_at
             FROM memory_vectors v
             JOIN memories m ON m.id = v.memory_id
             WHERE v.embedding MATCH ?1
             AND v.k = ?2
             ORDER BY v.distance";

        let mut stmt = self.conn.prepare(query)?;

        let rows = stmt
            .query_map(params![query_embedding.as_bytes(), fetch_limit], |row| {
                Ok(MemoryRow {
                    mnemonic: row.get(0)?,
                    content: row.get(1)?,
                    tags_json: row.get(2)?,
                    distance: row.get(3)?,
                    updated_at: row.get(4)?,
                    recall_count: row.get(5)?,
                    last_recalled_at: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut memories: Vec<Memory> = rows
            .into_iter()
            .map(|row| {
                let row_tags: Vec<String> =
                    serde_json::from_str(&row.tags_json).unwrap_or_default();
                Memory {
                    mnemonic: row.mnemonic,
                    content: row.content,
                    tags: row_tags,
                    distance: row.distance,
                    score: 0.0,
                    updated_at: row.updated_at,
                    recall_count: row.recall_count,
                    last_recalled_at: row.last_recalled_at,
                    links: Vec::new(),
                }
            })
            .filter(|mem| match tags {
                Some(filter_tags) => filter_tags.iter().any(|t| mem.tags.contains(t)),
                None => true,
            })
            .collect();

        // Populate links for each candidate
        for mem in &mut memories {
            mem.links = self.get_links(&mem.mnemonic)?;
        }

        // Compute composite scores
        // Pre-collect similarity map to avoid borrow conflict
        let similarity_map: std::collections::HashMap<String, f64> = memories
            .iter()
            .map(|m| (m.mnemonic.clone(), 1.0 - m.distance))
            .collect();
        let lambda = (2.0_f64).ln() / self.scoring.half_life_days;
        let now = self
            .conn
            .query_row("SELECT datetime('now')", [], |row| row.get::<_, String>(0))?;

        for mem in &mut memories {
            let similarity = 1.0 - mem.distance;

            let recency = match &mem.last_recalled_at {
                Some(ts) => {
                    let days = days_between(ts, &now);
                    (-lambda * days).exp()
                }
                None => 0.0,
            };

            let frequency = (1.0 + mem.recall_count as f64).ln();

            // Link boost: sum similarity of linked candidates (cap 3)
            let link_boost: f64 = mem
                .links
                .iter()
                .filter_map(|l| {
                    let other = if l.source_mnemonic == mem.mnemonic {
                        &l.target_mnemonic
                    } else {
                        &l.source_mnemonic
                    };
                    similarity_map.get(other).copied()
                })
                .take(3)
                .sum();

            mem.score = self.scoring.similarity_weight * similarity
                + self.scoring.recency_weight * recency
                + self.scoring.frequency_weight * frequency
                + self.scoring.link_weight * link_boost;
        }

        // Sort by score descending, take limit
        memories.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        memories.truncate(limit);

        // Update recall stats for all returned memories
        let mnemonics: Vec<&str> = memories.iter().map(|m| m.mnemonic.as_str()).collect();
        if !mnemonics.is_empty() {
            let placeholders: Vec<String> =
                (1..=mnemonics.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "UPDATE memories SET recall_count = recall_count + 1, last_recalled_at = datetime('now') WHERE mnemonic IN ({})",
                placeholders.join(", ")
            );
            let params: Vec<&dyn rusqlite::types::ToSql> = mnemonics
                .iter()
                .map(|m| m as &dyn rusqlite::types::ToSql)
                .collect();
            self.conn.execute(&sql, params.as_slice())?;
        }

        Ok(memories)
    }

    pub fn list_all_summaries(&self) -> Result<Vec<MemorySummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT mnemonic, content, tags, recall_count
             FROM memories
             ORDER BY recall_count DESC, updated_at DESC",
        )?;

        let results = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(mnemonic, content, tags_json, recall_count)| {
                let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
                MemorySummary {
                    mnemonic,
                    content,
                    tags,
                    recall_count,
                }
            })
            .collect();

        Ok(results)
    }

    pub fn find_merge_candidates(
        &self,
        embedding: &[f32],
        threshold: f64,
        exclude: &std::collections::HashSet<String>,
        limit: usize,
    ) -> Result<Vec<MergeCandidate>> {
        let fetch = limit + exclude.len() + 1;
        let mut stmt = self.conn.prepare(
            "SELECT m.mnemonic, m.content, m.tags, v.distance, m.recall_count
             FROM memory_vectors v
             JOIN memories m ON m.id = v.memory_id
             WHERE v.embedding MATCH ?1
             AND v.k = ?2
             ORDER BY v.distance",
        )?;

        let results = stmt
            .query_map(params![embedding.as_bytes(), fetch], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(mnemonic, _, _, distance, _)| {
                !exclude.contains(mnemonic) && *distance < threshold
            })
            .take(limit)
            .map(|(mnemonic, content, tags_json, distance, recall_count)| {
                let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
                MergeCandidate {
                    mnemonic,
                    content,
                    tags,
                    distance,
                    recall_count,
                }
            })
            .collect();

        Ok(results)
    }

    pub fn get_memory_by_mnemonic(&self, mnemonic: &str) -> Result<Option<Memory>> {
        let row = self.conn.query_row(
            "SELECT m.mnemonic, m.content, m.tags, m.updated_at, m.recall_count, m.last_recalled_at
             FROM memories m
             WHERE m.mnemonic = ?1",
            params![mnemonic],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        );

        match row {
            Ok((mnemonic, content, tags_json, updated_at, recall_count, last_recalled_at)) => {
                let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
                let links = self.get_links(&mnemonic)?;
                Ok(Some(Memory {
                    mnemonic,
                    content,
                    tags,
                    distance: 0.0,
                    score: 0.0,
                    updated_at,
                    recall_count,
                    last_recalled_at,
                    links,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_memory(
        &self,
        mnemonic: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<()> {
        let tags_json = serde_json::to_string(tags)?;
        let tx = self.conn.unchecked_transaction()?;

        let memory_id: i64 = tx
            .query_row(
                "SELECT id FROM memories WHERE mnemonic = ?1",
                params![mnemonic],
                |row| row.get(0),
            )
            .map_err(|_| anyhow!("mnemonic not found: {}", mnemonic))?;

        tx.execute(
            "UPDATE memories SET content = ?1, tags = ?2, updated_at = datetime('now') WHERE id = ?3",
            params![content, tags_json, memory_id],
        )?;

        tx.execute(
            "DELETE FROM memory_vectors WHERE memory_id = ?1",
            params![memory_id],
        )?;
        tx.execute(
            "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
            params![memory_id, embedding.as_bytes()],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn delete_memory(&self, mnemonic: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM memories WHERE mnemonic = ?1",
            params![mnemonic],
        )?;
        Ok(rows > 0)
    }

    pub fn get_all_links(&self) -> Result<Vec<MemoryLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.mnemonic, t.mnemonic, ml.link_type, ml.created_at
             FROM memory_links ml
             JOIN memories s ON s.id = ml.source_id
             JOIN memories t ON t.id = ml.target_id",
        )?;

        let links = stmt
            .query_map([], |row| {
                Ok(MemoryLink {
                    source_mnemonic: row.get(0)?,
                    target_mnemonic: row.get(1)?,
                    link_type: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(links)
    }
}

/// Parse SQLite datetime strings and return days elapsed between them.
fn days_between(earlier: &str, later: &str) -> f64 {
    // SQLite datetime format: "YYYY-MM-DD HH:MM:SS"
    fn parse_timestamp(s: &str) -> Option<f64> {
        let parts: Vec<&str> = s.split(|c| c == '-' || c == ' ' || c == ':').collect();
        if parts.len() < 6 {
            return None;
        }
        let year: f64 = parts[0].parse().ok()?;
        let month: f64 = parts[1].parse().ok()?;
        let day: f64 = parts[2].parse().ok()?;
        let hour: f64 = parts[3].parse().ok()?;
        let min: f64 = parts[4].parse().ok()?;
        let sec: f64 = parts[5].parse().ok()?;
        // Approximate days since epoch (good enough for deltas)
        Some(year * 365.25 + month * 30.44 + day + (hour + min / 60.0 + sec / 3600.0) / 24.0)
    }
    match (parse_timestamp(earlier), parse_timestamp(later)) {
        (Some(e), Some(l)) => (l - e).max(0.0),
        _ => 0.0,
    }
}

struct MemoryRow {
    mnemonic: String,
    content: String,
    tags_json: String,
    distance: f64,
    updated_at: String,
    recall_count: i64,
    last_recalled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySummary {
    pub mnemonic: String,
    pub content: String,
    pub tags: Vec<String>,
    pub recall_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeCandidate {
    pub mnemonic: String,
    pub content: String,
    pub tags: Vec<String>,
    pub distance: f64,
    pub recall_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memorize_and_recall() -> Result<()> {
        let store = MemoryStore::in_memory()?;

        // 384-dim fake embeddings
        let emb1: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb2: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.01).collect();
        let query: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.005).collect();

        store.memorize("test::fact1", "Rust is a systems language", &[], &emb1)?;
        store.memorize(
            "test::fact2",
            "SQLite is an embedded database",
            &["db".into()],
            &emb2,
        )?;

        let results = store.recall(&query, 5, None)?;
        assert_eq!(results.len(), 2);

        // Both should be returned, closest first
        assert!(results[0].distance <= results[1].distance);

        // Pre-update snapshot: count=0, never recalled
        assert_eq!(results[0].recall_count, 0);
        assert_eq!(results[1].recall_count, 0);
        assert!(results[0].last_recalled_at.is_none());
        assert!(results[1].last_recalled_at.is_none());

        Ok(())
    }

    #[test]
    fn test_upsert() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![0.2; 384];

        store.memorize("key", "original content", &[], &emb)?;
        store.memorize("key", "updated content", &[], &emb2)?;

        let results = store.recall(&emb2, 5, None)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "updated content");

        Ok(())
    }

    #[test]
    fn test_recall_tracking() -> Result<()> {
        let store = MemoryStore::in_memory()?;

        let emb: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        store.memorize("tracked::fact", "some content", &[], &emb)?;

        // First recall — returned snapshot has count=0 (pre-update value)
        let results = store.recall(&emb, 5, None)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].recall_count, 0);
        assert!(results[0].last_recalled_at.is_none());

        // Second recall — DB was updated by the first recall, so now count=1
        let results = store.recall(&emb, 5, None)?;
        assert_eq!(results[0].recall_count, 1);
        assert!(results[0].last_recalled_at.is_some());

        // Third recall — count should be 2
        let results = store.recall(&emb, 5, None)?;
        assert_eq!(results[0].recall_count, 2);
        assert!(results[0].last_recalled_at.is_some());

        Ok(())
    }

    #[test]
    fn test_create_link() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        // Use a very different embedding so auto-link doesn't fire
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize("alpha", "first memory", &[], &emb1)?;
        store.memorize("beta", "second memory", &[], &emb2)?;

        store.link("alpha", "beta", "related")?;

        let links = store.get_links("alpha")?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source_mnemonic, "alpha");
        assert_eq!(links[0].target_mnemonic, "beta");
        assert_eq!(links[0].link_type, "related");

        // Link is also visible from beta's perspective
        let links_beta = store.get_links("beta")?;
        assert_eq!(links_beta.len(), 1);

        Ok(())
    }

    #[test]
    fn test_auto_link_similar_memories() -> Result<()> {
        let store = MemoryStore::in_memory()?;

        // Embeddings in the auto-link zone (distance between 0.15 and 0.3)
        // offset 0.01 → L2 distance ≈ sqrt(384 * 0.01²) ≈ 0.196
        let emb1: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb2: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.01).collect();

        store.memorize("similar::a", "topic A", &[], &emb1)?;
        store.memorize("similar::b", "topic B", &[], &emb2)?;

        let links = store.get_links("similar::b")?;
        assert!(
            !links.is_empty(),
            "auto-link should create a link between similar memories"
        );
        assert_eq!(links[0].link_type, "related");

        Ok(())
    }

    #[test]
    fn test_get_links() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];
        let emb3: Vec<f32> = vec![0.9; 384];

        store.memorize("x", "mem x", &[], &emb1)?;
        store.memorize("y", "mem y", &[], &emb2)?;
        store.memorize("z", "mem z", &[], &emb3)?;

        store.link("x", "y", "supersedes")?;
        store.link("z", "x", "derived_from")?;

        let links = store.get_links("x")?;
        assert_eq!(links.len(), 2);

        Ok(())
    }

    #[test]
    fn test_unlink() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize("a", "mem a", &[], &emb1)?;
        store.memorize("b", "mem b", &[], &emb2)?;

        store.link("a", "b", "related")?;
        assert_eq!(store.get_links("a")?.len(), 1);

        store.unlink("a", "b", "related")?;
        assert_eq!(store.get_links("a")?.len(), 0);

        Ok(())
    }

    #[test]
    fn test_links_survive_upsert() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize("p", "original", &[], &emb1)?;
        store.memorize("q", "other", &[], &emb2)?;
        store.link("p", "q", "related")?;

        // Upsert p with new content
        store.memorize("p", "updated", &[], &emb1)?;

        let links = store.get_links("p")?;
        assert!(
            links.iter().any(|l| l.target_mnemonic == "q"),
            "link should survive upsert"
        );

        Ok(())
    }

    #[test]
    fn test_link_idempotent() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize("m1", "first", &[], &emb1)?;
        store.memorize("m2", "second", &[], &emb2)?;

        store.link("m1", "m2", "related")?;
        store.link("m1", "m2", "related")?; // should not error

        let links = store.get_links("m1")?;
        assert_eq!(links.len(), 1);

        Ok(())
    }

    #[test]
    fn test_link_missing_mnemonic() {
        let store = MemoryStore::in_memory().unwrap();
        let emb: Vec<f32> = vec![0.1; 384];
        store.memorize("exists", "content", &[], &emb).unwrap();

        let result = store.link("exists", "does_not_exist", "related");
        assert!(result.is_err());

        let result = store.link("does_not_exist", "exists", "related");
        assert!(result.is_err());
    }

    #[test]
    fn test_recall_includes_links() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize("r1", "recall target", &[], &emb1)?;
        store.memorize("r2", "other memory", &[], &emb2)?;
        store.link("r1", "r2", "supersedes")?;

        let results = store.recall(&emb1, 5, None)?;
        let r1 = results.iter().find(|m| m.mnemonic == "r1").unwrap();
        assert!(!r1.links.is_empty(), "recalled memory should include links");

        Ok(())
    }

    #[test]
    fn test_score_field_populated() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        store.memorize("scored", "content", &[], &emb)?;

        let results = store.recall(&emb, 5, None)?;
        assert_eq!(results.len(), 1);
        assert!(results[0].score > 0.0, "score should be positive for close match");
        Ok(())
    }

    #[test]
    fn test_frequency_boosts_score() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb2: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.01).collect();

        store.memorize("freq::a", "frequently recalled", &[], &emb1)?;
        store.memorize("freq::b", "rarely recalled", &[], &emb2)?;

        // Recall several times to boost freq::a's recall_count
        for _ in 0..5 {
            store.recall(&emb1, 1, None)?;
        }

        // Query equidistant — freq::a should score higher due to frequency
        let mid: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.005).collect();
        let results = store.recall(&mid, 2, None)?;
        assert_eq!(results.len(), 2);

        let a = results.iter().find(|m| m.mnemonic == "freq::a").unwrap();
        let b = results.iter().find(|m| m.mnemonic == "freq::b").unwrap();
        assert!(a.score > b.score, "higher recall_count should boost score");
        Ok(())
    }

    #[test]
    fn test_recency_boosts_score() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb2: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.01).collect();

        store.memorize("recent::a", "recently recalled", &[], &emb1)?;
        store.memorize("recent::b", "never recalled", &[], &emb2)?;

        // Recall a once to give it a recent last_recalled_at
        store.recall(&emb1, 1, None)?;

        // Query equidistant
        let mid: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.005).collect();
        let results = store.recall(&mid, 2, None)?;
        let a = results.iter().find(|m| m.mnemonic == "recent::a").unwrap();
        let b = results.iter().find(|m| m.mnemonic == "recent::b").unwrap();
        // a has recency + frequency boost, b has neither
        assert!(a.score > b.score, "recently recalled memory should score higher");
        Ok(())
    }

    #[test]
    fn test_link_boost_score() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        // a and c equidistant from query, b nearby; a is linked to b
        let base: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb_a: Vec<f32> = base.iter().map(|x| x - 0.01).collect();
        let emb_b: Vec<f32> = base.clone();
        let emb_c: Vec<f32> = base.iter().map(|x| x + 0.01).collect();

        store.memorize("linked::a", "content a", &[], &emb_a)?;
        store.memorize("linked::b", "content b", &[], &emb_b)?;
        store.memorize("linked::c", "content c", &[], &emb_c)?;

        // Link a and b — both are candidates, so a gets link_boost from b's similarity
        store.link("linked::a", "linked::b", "related")?;

        let results = store.recall(&base, 3, None)?;
        let a = results.iter().find(|m| m.mnemonic == "linked::a").unwrap();
        let c = results.iter().find(|m| m.mnemonic == "linked::c").unwrap();
        // a and c have symmetric distances from query, but a has link boost
        assert!(a.score > c.score, "linked memory should score higher than equidistant unlinked");
        Ok(())
    }

    #[test]
    fn test_auto_merge_very_close_embeddings() -> Result<()> {
        let store = MemoryStore::in_memory()?;

        // Two embeddings within AUTO_MERGE_THRESHOLD (0.15)
        let emb1: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb2: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.0001).collect();

        store.memorize("merge::old", "old content", &["tag_a".into()], &emb1)?;
        store.memorize("merge::new", "new content", &["tag_b".into()], &emb2)?;

        // Old should be merged into new
        let results = store.recall(&emb1, 10, None)?;
        let mnemonics: Vec<&str> = results.iter().map(|m| m.mnemonic.as_str()).collect();
        assert!(
            !mnemonics.contains(&"merge::old"),
            "old memory should be deleted after auto-merge"
        );
        let new = results.iter().find(|m| m.mnemonic == "merge::new").unwrap();
        assert!(
            new.content.contains("new content") && new.content.contains("old content"),
            "merged memory should contain both contents"
        );
        // Tags should be unioned
        assert!(new.tags.contains(&"tag_a".to_string()));
        assert!(new.tags.contains(&"tag_b".to_string()));

        Ok(())
    }

    #[test]
    fn test_manual_merge_preserves_content_and_links() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];
        let emb3: Vec<f32> = vec![0.9; 384];

        store.memorize("keep", "keep content", &["a".into()], &emb1)?;
        store.memorize("discard", "discard content", &["b".into()], &emb2)?;
        store.memorize("other", "other content", &[], &emb3)?;

        // Link discard to other
        store.link("discard", "other", "related")?;

        store.merge("keep", "discard", &emb1)?;

        // Discard should be gone
        let results = store.recall(&emb2, 10, None)?;
        assert!(
            !results.iter().any(|m| m.mnemonic == "discard"),
            "discard memory should be deleted"
        );

        // Keep should have merged content
        let results = store.recall(&emb1, 10, None)?;
        let kept = results.iter().find(|m| m.mnemonic == "keep").unwrap();
        assert!(kept.content.contains("keep content"));
        assert!(kept.content.contains("discard content"));
        assert!(kept.tags.contains(&"a".to_string()));
        assert!(kept.tags.contains(&"b".to_string()));

        // Link from discard should have transferred to keep
        let links = store.get_links("keep")?;
        assert!(
            links.iter().any(|l| l.target_mnemonic == "other" || l.source_mnemonic == "other"),
            "links should transfer from discard to keep"
        );

        Ok(())
    }
}
