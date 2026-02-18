use anyhow::{Context, Result};
use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::{Deserialize, Serialize};
use sqlite_vec::sqlite3_vec_init;
use std::path::Path;
use std::sync::Once;
use zerocopy::AsBytes;

static VEC_INIT: Once = Once::new();

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
    pub updated_at: String,
}

pub struct MemoryStore {
    conn: Connection,
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
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mnemonic TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL,
                tags TEXT DEFAULT '[]',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memory_vectors USING vec0(
                memory_id INTEGER PRIMARY KEY,
                embedding float[384]
            );",
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
        tx.execute(
            "INSERT INTO memories (mnemonic, content, tags)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(mnemonic) DO UPDATE SET
                content = excluded.content,
                tags = excluded.tags,
                updated_at = datetime('now')",
            params![mnemonic, content, tags_json],
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

        tx.commit()?;
        Ok(())
    }

    pub fn recall(
        &self,
        query_embedding: &[f32],
        limit: usize,
        tags: Option<&[String]>,
    ) -> Result<Vec<Memory>> {
        // sqlite-vec requires k=? in the WHERE clause for KNN queries.
        // When filtering by tags, fetch extra results and filter post-query.
        let fetch_limit = match tags {
            Some(_) => limit * 4,
            None => limit,
        };

        let query = "SELECT m.mnemonic, m.content, m.tags, v.distance, m.updated_at
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
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let memories: Vec<Memory> = rows
            .into_iter()
            .map(|row| {
                let row_tags: Vec<String> =
                    serde_json::from_str(&row.tags_json).unwrap_or_default();
                Memory {
                    mnemonic: row.mnemonic,
                    content: row.content,
                    tags: row_tags,
                    distance: row.distance,
                    updated_at: row.updated_at,
                }
            })
            .filter(|mem| match tags {
                Some(filter_tags) => filter_tags.iter().any(|t| mem.tags.contains(t)),
                None => true,
            })
            .take(limit)
            .collect();

        Ok(memories)
    }
}

struct MemoryRow {
    mnemonic: String,
    content: String,
    tags_json: String,
    distance: f64,
    updated_at: String,
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
}
