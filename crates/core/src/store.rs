use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::{Deserialize, Serialize};
use sqlite_vec::sqlite3_vec_init;
use std::path::Path;
use std::sync::Once;
use zerocopy::AsBytes;

static VEC_INIT: Once = Once::new();

const AUTO_LINK_THRESHOLD: f64 = 0.3;
const AUTO_LINK_MAX_NEIGHBORS: usize = 5;

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
            "PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS memories (
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

        for (neighbor_id, distance) in &neighbors {
            if *neighbor_id != memory_id && *distance < AUTO_LINK_THRESHOLD {
                tx.execute(
                    "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type)
                     VALUES (?1, ?2, 'related')",
                    params![memory_id, neighbor_id],
                )?;
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
                    updated_at: row.updated_at,
                    links: Vec::new(),
                }
            })
            .filter(|mem| match tags {
                Some(filter_tags) => filter_tags.iter().any(|t| mem.tags.contains(t)),
                None => true,
            })
            .take(limit)
            .collect();

        // Populate links for each recalled memory
        for mem in &mut memories {
            mem.links = self.get_links(&mem.mnemonic)?;
        }

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

        // Two very similar embeddings — should be auto-linked
        let emb1: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
        let emb2: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0 + 0.001).collect();

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
}
