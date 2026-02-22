use anyhow::{Result, anyhow};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::embedder::Embedder;
use crate::store::MemoryStore;

#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    uuid: String,
    mnemonic: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<ExportLink>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportLink {
    target: String, // UUID of target
    #[serde(rename = "type")]
    link_type: String,
}

#[derive(Debug, Default)]
pub struct ImportResult {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
}

fn slugify(s: &str) -> String {
    let slug: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive hyphens and trim
    let mut result = String::new();
    let mut prev_hyphen = true; // trim leading
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    // Trim trailing
    while result.ends_with('-') {
        result.pop();
    }
    result
}

struct ExportRow {
    uuid: String,
    mnemonic: String,
    content: String,
    tags_json: String,
}

struct ExportLinkRow {
    source_uuid: String,
    target_uuid: String,
    link_type: String,
}

impl MemoryStore {
    pub fn export(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;

        // Query all memories
        let mut stmt = self
            .conn()
            .prepare("SELECT uuid, mnemonic, content, tags FROM memories ORDER BY mnemonic")?;
        let rows: Vec<ExportRow> = stmt
            .query_map([], |row| {
                Ok(ExportRow {
                    uuid: row.get(0)?,
                    mnemonic: row.get(1)?,
                    content: row.get(2)?,
                    tags_json: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Query all links with UUIDs
        let mut link_stmt = self.conn().prepare(
            "SELECT s_mem.uuid, t_mem.uuid, ml.link_type
             FROM memory_links ml
             JOIN memories s_mem ON s_mem.id = ml.source_id
             JOIN memories t_mem ON t_mem.id = ml.target_id",
        )?;
        let link_rows: Vec<ExportLinkRow> = link_stmt
            .query_map([], |row| {
                Ok(ExportLinkRow {
                    source_uuid: row.get(0)?,
                    target_uuid: row.get(1)?,
                    link_type: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for row in &rows {
            let tags: Vec<String> = serde_json::from_str(&row.tags_json).unwrap_or_default();

            // Collect links where this memory is the source
            let links: Vec<ExportLink> = link_rows
                .iter()
                .filter(|l| l.source_uuid == row.uuid)
                .map(|l| ExportLink {
                    target: l.target_uuid.clone(),
                    link_type: l.link_type.clone(),
                })
                .collect();

            let fm = Frontmatter {
                uuid: row.uuid.clone(),
                mnemonic: row.mnemonic.clone(),
                tags,
                links,
            };

            let yaml = serde_norway::to_string(&fm)?;
            let file_content = format!("---\n{yaml}---\n\n{}", row.content);

            let filename = format!("{}.md", slugify(&row.mnemonic));
            let path = dir.join(&filename);
            std::fs::write(&path, file_content)?;
        }

        Ok(())
    }

    pub fn import(&self, dir: &Path, embedder: &Embedder) -> Result<ImportResult> {
        if !dir.is_dir() {
            return Err(anyhow!("not a directory: {}", dir.display()));
        }

        let mut result = ImportResult::default();
        let mut imported: Vec<(String, String)> = Vec::new(); // (uuid, mnemonic) for link resolution

        // Read all .md files
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        entries.sort_by_key(|e| e.path());

        for entry in &entries {
            let path = entry.path();
            let raw = std::fs::read_to_string(&path)?;

            let (fm, content) = parse_frontmatter(&raw)
                .ok_or_else(|| anyhow!("invalid frontmatter in {}", path.display()))?;

            // Check if this UUID already exists
            let existing: Option<(i64, String)> = self
                .conn()
                .query_row(
                    "SELECT id, content FROM memories WHERE uuid = ?1",
                    params![fm.uuid],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok();

            match existing {
                Some((id, old_content)) => {
                    if old_content == content {
                        result.unchanged += 1;
                    } else {
                        let tags_json = serde_json::to_string(&fm.tags)?;
                        let embedding = embedder.embed(&fm.mnemonic)?;
                        self.conn().execute(
                            "UPDATE memories SET content = ?1, tags = ?2, mnemonic = ?3, updated_at = datetime('now') WHERE id = ?4",
                            params![content, tags_json, fm.mnemonic, id],
                        )?;
                        // Update vector
                        self.conn().execute(
                            "DELETE FROM memory_vectors WHERE memory_id = ?1",
                            params![id],
                        )?;
                        self.conn().execute(
                            "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
                            params![id, zerocopy::AsBytes::as_bytes(embedding.as_slice())],
                        )?;
                        result.updated += 1;
                    }
                }
                None => {
                    let tags_json = serde_json::to_string(&fm.tags)?;
                    let embedding = embedder.embed(&fm.mnemonic)?;
                    self.conn().execute(
                        "INSERT INTO memories (uuid, mnemonic, content, tags) VALUES (?1, ?2, ?3, ?4)",
                        params![fm.uuid, fm.mnemonic, content, tags_json],
                    )?;
                    let id: i64 = self.conn().query_row(
                        "SELECT id FROM memories WHERE uuid = ?1",
                        params![fm.uuid],
                        |row| row.get(0),
                    )?;
                    self.conn().execute(
                        "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
                        params![id, zerocopy::AsBytes::as_bytes(embedding.as_slice())],
                    )?;
                    result.created += 1;
                }
            }

            imported.push((fm.uuid, fm.mnemonic));
        }

        // Recreate links from UUID references (second pass)
        for entry in &entries {
            let path = entry.path();
            let raw = std::fs::read_to_string(&path)?;
            let (fm, _) = parse_frontmatter(&raw).unwrap();

            for link in &fm.links {
                // Resolve source and target UUIDs to IDs
                let source_id: Option<i64> = self
                    .conn()
                    .query_row(
                        "SELECT id FROM memories WHERE uuid = ?1",
                        params![fm.uuid],
                        |row| row.get(0),
                    )
                    .ok();
                let target_id: Option<i64> = self
                    .conn()
                    .query_row(
                        "SELECT id FROM memories WHERE uuid = ?1",
                        params![link.target],
                        |row| row.get(0),
                    )
                    .ok();

                if let (Some(sid), Some(tid)) = (source_id, target_id) {
                    self.conn().execute(
                        "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type) VALUES (?1, ?2, ?3)",
                        params![sid, tid, link.link_type],
                    )?;
                }
            }
        }

        Ok(result)
    }
}

fn parse_frontmatter(raw: &str) -> Option<(Frontmatter, String)> {
    let trimmed = raw.strip_prefix("---\n")?;
    let end = trimmed.find("---\n")?;
    let yaml_part = &trimmed[..end];
    let body = trimmed[end + 4..].trim_start_matches('\n').to_string();
    let fm: Frontmatter = serde_norway::from_str(yaml_part).ok()?;
    Some((fm, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use tempfile::TempDir;

    fn make_store_with_data() -> Result<MemoryStore> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize(
            "project design",
            "layered architecture",
            &["arch".into()],
            &emb1,
        )?;
        store.memorize(
            "api endpoints",
            "REST API at /api/v1",
            &["api".into()],
            &emb2,
        )?;
        store.link("project design", "api endpoints", "related")?;

        Ok(store)
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("project design"), "project-design");
        assert_eq!(slugify("src/foo/bar.rs"), "src-foo-bar-rs");
        assert_eq!(slugify("Hello World!!"), "hello-world");
        assert_eq!(slugify("--leading--trailing--"), "leading-trailing");
    }

    #[test]
    fn test_export_creates_files() -> Result<()> {
        let store = make_store_with_data()?;
        let dir = TempDir::new()?;

        store.export(dir.path())?;

        let files: Vec<_> = std::fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 2);

        // Check one file has frontmatter
        let content = std::fs::read_to_string(dir.path().join("project-design.md"))?;
        assert!(content.starts_with("---\n"));
        assert!(content.contains("mnemonic: project design"));
        assert!(content.contains("layered architecture"));

        Ok(())
    }

    #[test]
    fn test_export_import_roundtrip() -> Result<()> {
        let store = make_store_with_data()?;
        let dir = TempDir::new()?;
        store.export(dir.path())?;

        // Import into a fresh store
        let store2 = MemoryStore::in_memory()?;
        let embedder = Embedder::new()?;
        let result = store2.import(dir.path(), &embedder)?;

        assert_eq!(result.created, 2);
        assert_eq!(result.updated, 0);
        assert_eq!(result.unchanged, 0);

        // Verify links were recreated
        let links = store2.get_links("project design")?;
        assert!(!links.is_empty(), "links should be recreated on import");

        Ok(())
    }

    #[test]
    fn test_import_idempotent() -> Result<()> {
        let store = make_store_with_data()?;
        let dir = TempDir::new()?;
        store.export(dir.path())?;

        // Import twice into same store
        let store2 = MemoryStore::in_memory()?;
        let embedder = Embedder::new()?;
        let r1 = store2.import(dir.path(), &embedder)?;
        assert_eq!(r1.created, 2);

        let r2 = store2.import(dir.path(), &embedder)?;
        assert_eq!(r2.unchanged, 2);
        assert_eq!(r2.created, 0);

        Ok(())
    }

    #[test]
    fn test_uuid_stability() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb: Vec<f32> = vec![0.1; 384];
        store.memorize("stable", "content", &[], &emb)?;

        let uuid1: String = store.conn().query_row(
            "SELECT uuid FROM memories WHERE mnemonic = 'stable'",
            [],
            |row| row.get(0),
        )?;

        // Upsert should not change UUID
        store.memorize("stable", "updated content", &[], &emb)?;

        let uuid2: String = store.conn().query_row(
            "SELECT uuid FROM memories WHERE mnemonic = 'stable'",
            [],
            |row| row.get(0),
        )?;

        assert_eq!(uuid1, uuid2, "UUID should be stable across upserts");
        Ok(())
    }

    #[test]
    fn test_parse_frontmatter() {
        let raw = "---\nuuid: abc-123\nmnemonic: test\n---\n\nHello world";
        let (fm, body) = parse_frontmatter(raw).unwrap();
        assert_eq!(fm.uuid, "abc-123");
        assert_eq!(fm.mnemonic, "test");
        assert_eq!(body, "Hello world");
    }
}
