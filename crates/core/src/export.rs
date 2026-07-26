use anyhow::{Result, anyhow};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::embedder::Embedder;
use crate::store::MemoryStore;

fn is_zero(n: &i64) -> bool {
    *n == 0
}

#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    uuid: String,
    mnemonic: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mnemonics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<ExportLink>,
    // Stats — all serde-defaulted so exports written before this field set
    // still import cleanly. Timestamps are the raw SQLite TEXT values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    recall_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_recalled_at: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    useful_count: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    not_useful_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportLink {
    target: String, // UUID of target
    #[serde(rename = "type")]
    link_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
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
    memory_id: i64,
    uuid: String,
    title: String,
    content: String,
    tags_json: String,
    created_at: String,
    updated_at: String,
    recall_count: i64,
    last_recalled_at: Option<String>,
    useful_count: i64,
    not_useful_count: i64,
}

struct ExportLinkRow {
    source_uuid: String,
    target_uuid: String,
    link_type: String,
    created_at: String,
}

/// Columns selected for every export row. Uses the `m` alias so the same
/// list works for both the tag-filtered (`json_each`) and full-table queries.
const EXPORT_COLS: &str = "m.id, m.uuid, m.title, m.content, m.tags, \
    m.created_at, m.updated_at, m.recall_count, m.last_recalled_at, \
    m.useful_count, m.not_useful_count";

fn map_export_row(row: &rusqlite::Row) -> rusqlite::Result<ExportRow> {
    Ok(ExportRow {
        memory_id: row.get(0)?,
        uuid: row.get(1)?,
        title: row.get(2)?,
        content: row.get(3)?,
        tags_json: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        recall_count: row.get(7)?,
        last_recalled_at: row.get(8)?,
        useful_count: row.get(9)?,
        not_useful_count: row.get(10)?,
    })
}

fn map_export_link_row(row: &rusqlite::Row) -> rusqlite::Result<ExportLinkRow> {
    Ok(ExportLinkRow {
        source_uuid: row.get(0)?,
        target_uuid: row.get(1)?,
        link_type: row.get(2)?,
        created_at: row.get(3)?,
    })
}

impl MemoryStore {
    /// Fetch export rows, optionally pre-filtered to memories carrying any of `tags`.
    fn query_export_rows(&self, tags: Option<&[String]>) -> Result<Vec<ExportRow>> {
        let rows = match tags {
            Some(filter_tags) if !filter_tags.is_empty() => {
                let placeholders: Vec<String> =
                    (1..=filter_tags.len()).map(|i| format!("?{i}")).collect();
                let sql = format!(
                    "SELECT DISTINCT {EXPORT_COLS}
                     FROM memories m, json_each(m.tags) je
                     WHERE je.value IN ({})
                     ORDER BY m.title",
                    placeholders.join(", ")
                );
                let mut stmt = self.conn().prepare(&sql)?;
                let params: Vec<&dyn rusqlite::types::ToSql> = filter_tags
                    .iter()
                    .map(|t| t as &dyn rusqlite::types::ToSql)
                    .collect();
                stmt.query_map(params.as_slice(), map_export_row)?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }
            _ => {
                let sql = format!("SELECT {EXPORT_COLS} FROM memories m ORDER BY m.title");
                let mut stmt = self.conn().prepare(&sql)?;
                stmt.query_map([], map_export_row)?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        Ok(rows)
    }

    /// Fetch all links whose source and target are both in the exported set.
    fn query_export_links(
        &self,
        exported_uuids: &std::collections::HashSet<&str>,
    ) -> Result<Vec<ExportLinkRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT s_mem.uuid, t_mem.uuid, ml.link_type, ml.created_at
             FROM memory_links ml
             JOIN memories s_mem ON s_mem.id = ml.source_id
             JOIN memories t_mem ON t_mem.id = ml.target_id",
        )?;
        let rows = stmt
            .query_map([], map_export_link_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|l| {
                exported_uuids.contains(l.source_uuid.as_str())
                    && exported_uuids.contains(l.target_uuid.as_str())
            })
            .collect();
        Ok(rows)
    }

    /// Write one Markdown file per memory row, embedding its links as frontmatter.
    fn write_memory_files(
        &self,
        dir: &Path,
        rows: &[ExportRow],
        link_rows: &[ExportLinkRow],
    ) -> Result<()> {
        for row in rows {
            let tags: Vec<String> = serde_json::from_str(&row.tags_json).unwrap_or_default();

            // Additional mnemonics (excluding the title itself)
            let mnemonics: Vec<String> =
                MemoryStore::get_mnemonics_for_memory(self.conn(), row.memory_id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|m| m != &row.title)
                    .collect();

            // Links where this memory is the source
            let links: Vec<ExportLink> = link_rows
                .iter()
                .filter(|l| l.source_uuid == row.uuid)
                .map(|l| ExportLink {
                    target: l.target_uuid.clone(),
                    link_type: l.link_type.clone(),
                    created_at: Some(l.created_at.clone()),
                })
                .collect();

            let fm = Frontmatter {
                uuid: row.uuid.clone(),
                mnemonic: row.title.clone(),
                mnemonics,
                tags,
                links,
                created_at: Some(row.created_at.clone()),
                updated_at: Some(row.updated_at.clone()),
                recall_count: row.recall_count,
                last_recalled_at: row.last_recalled_at.clone(),
                useful_count: row.useful_count,
                not_useful_count: row.not_useful_count,
            };

            let yaml = serde_norway::to_string(&fm)?;
            let file_content = format!("---\n{yaml}---\n\n{}", row.content);
            let filename = format!("{}.md", slugify(&row.title));
            std::fs::write(dir.join(&filename), file_content)?;
        }
        Ok(())
    }

    pub fn export(&self, dir: &Path, tags: Option<&[String]>) -> Result<()> {
        std::fs::create_dir_all(dir)?;

        let rows = self.query_export_rows(tags)?;
        let exported_uuids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.uuid.as_str()).collect();
        let link_rows = self.query_export_links(&exported_uuids)?;

        self.write_memory_files(dir, &rows, &link_rows)
    }

    /// Like `export`, but applies an additional filter predicate on each memory's tags.
    /// Only memories for which `filter(&tags)` returns true are exported.
    pub fn export_filtered(
        &self,
        dir: &Path,
        tags: Option<&[String]>,
        filter: impl Fn(&[String]) -> bool,
    ) -> Result<()> {
        std::fs::create_dir_all(dir)?;

        let rows: Vec<ExportRow> = self
            .query_export_rows(tags)?
            .into_iter()
            .filter(|row| {
                let mem_tags: Vec<String> =
                    serde_json::from_str(&row.tags_json).unwrap_or_default();
                filter(&mem_tags)
            })
            .collect();

        let exported_uuids: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.uuid.as_str()).collect();
        let link_rows = self.query_export_links(&exported_uuids)?;

        self.write_memory_files(dir, &rows, &link_rows)
    }

    pub fn import(&self, dir: &Path, embedder: &mut Embedder) -> Result<ImportResult> {
        if !dir.is_dir() {
            return Err(anyhow!("not a directory: {}", dir.display()));
        }

        let mut result = ImportResult::default();

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

            let memory_id: i64 = match existing {
                Some((id, old_content)) => {
                    if old_content == content {
                        result.unchanged += 1;
                    } else {
                        let tags_json = serde_json::to_string(&fm.tags)?;
                        let embedding = embedder.embed(&fm.mnemonic)?;
                        self.conn().execute(
                            "UPDATE memories SET content = ?1, tags = ?2, title = ?3, mnemonic = ?3,
                                created_at = COALESCE(?4, created_at),
                                updated_at = COALESCE(?5, datetime('now')),
                                recall_count = ?6, last_recalled_at = ?7,
                                useful_count = ?8, not_useful_count = ?9
                             WHERE id = ?10",
                            params![
                                content, tags_json, fm.mnemonic,
                                fm.created_at, fm.updated_at,
                                fm.recall_count, fm.last_recalled_at,
                                fm.useful_count, fm.not_useful_count,
                                id
                            ],
                        )?;
                        // Update primary mnemonic in mnemonics table
                        self.conn().execute(
                            "INSERT OR IGNORE INTO mnemonics (memory_id, text) VALUES (?1, ?2)",
                            params![id, fm.mnemonic],
                        )?;
                        let mn_id: i64 = self.conn().query_row(
                            "SELECT id FROM mnemonics WHERE text = ?1",
                            params![fm.mnemonic],
                            |row| row.get(0),
                        )?;
                        // Update mnemonic vector
                        self.conn().execute(
                            "DELETE FROM mnemonic_vectors WHERE mnemonic_id = ?1",
                            params![mn_id],
                        )?;
                        self.conn().execute(
                            "INSERT INTO mnemonic_vectors (mnemonic_id, embedding) VALUES (?1, ?2)",
                            params![mn_id, zerocopy::IntoBytes::as_bytes(embedding.as_slice())],
                        )?;
                        // Also update legacy memory_vectors
                        self.conn().execute(
                            "DELETE FROM memory_vectors WHERE memory_id = ?1",
                            params![id],
                        )?;
                        self.conn().execute(
                            "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
                            params![id, zerocopy::IntoBytes::as_bytes(embedding.as_slice())],
                        )?;
                        result.updated += 1;
                    }
                    id
                }
                None => {
                    let tags_json = serde_json::to_string(&fm.tags)?;
                    let embedding = embedder.embed(&fm.mnemonic)?;
                    self.conn().execute(
                        "INSERT INTO memories
                            (uuid, mnemonic, title, content, tags,
                             created_at, updated_at, recall_count, last_recalled_at,
                             useful_count, not_useful_count)
                         VALUES (?1, ?2, ?3, ?4, ?5,
                             COALESCE(?6, datetime('now')), COALESCE(?7, datetime('now')),
                             ?8, ?9, ?10, ?11)",
                        params![
                            fm.uuid, fm.mnemonic, fm.mnemonic, content, tags_json,
                            fm.created_at, fm.updated_at,
                            fm.recall_count, fm.last_recalled_at,
                            fm.useful_count, fm.not_useful_count
                        ],
                    )?;
                    let id: i64 = self.conn().query_row(
                        "SELECT id FROM memories WHERE uuid = ?1",
                        params![fm.uuid],
                        |row| row.get(0),
                    )?;
                    // Insert primary mnemonic
                    self.conn().execute(
                        "INSERT OR IGNORE INTO mnemonics (memory_id, text) VALUES (?1, ?2)",
                        params![id, fm.mnemonic],
                    )?;
                    let mn_id: i64 = self.conn().query_row(
                        "SELECT id FROM mnemonics WHERE text = ?1",
                        params![fm.mnemonic],
                        |row| row.get(0),
                    )?;
                    self.conn().execute(
                        "INSERT INTO mnemonic_vectors (mnemonic_id, embedding) VALUES (?1, ?2)",
                        params![mn_id, zerocopy::IntoBytes::as_bytes(embedding.as_slice())],
                    )?;
                    // Also insert legacy memory_vectors
                    self.conn().execute(
                        "INSERT INTO memory_vectors (memory_id, embedding) VALUES (?1, ?2)",
                        params![id, zerocopy::IntoBytes::as_bytes(embedding.as_slice())],
                    )?;
                    result.created += 1;
                    id
                }
            };

            // Import additional mnemonics
            for mn_text in &fm.mnemonics {
                self.conn().execute(
                    "INSERT OR IGNORE INTO mnemonics (memory_id, text) VALUES (?1, ?2)",
                    params![memory_id, mn_text],
                )?;
                let mn_id: Option<i64> = self
                    .conn()
                    .query_row(
                        "SELECT id FROM mnemonics WHERE text = ?1",
                        params![mn_text],
                        |row| row.get(0),
                    )
                    .ok();
                if let Some(mn_id) = mn_id {
                    // Check if already has vector
                    let has_vec: bool = self
                        .conn()
                        .query_row(
                            "SELECT COUNT(*) FROM mnemonic_vectors WHERE mnemonic_id = ?1",
                            params![mn_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .map(|c| c > 0)?;
                    if !has_vec {
                        let emb = embedder.embed(mn_text)?;
                        self.conn().execute(
                            "INSERT INTO mnemonic_vectors (mnemonic_id, embedding) VALUES (?1, ?2)",
                            params![mn_id, zerocopy::IntoBytes::as_bytes(emb.as_slice())],
                        )?;
                    }
                }
            }
        }

        // Recreate links from UUID references (second pass)
        for entry in &entries {
            let path = entry.path();
            let raw = std::fs::read_to_string(&path)?;
            let (fm, _) = parse_frontmatter(&raw).unwrap();

            for link in &fm.links {
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
                        "INSERT OR IGNORE INTO memory_links (source_id, target_id, link_type, created_at)
                         VALUES (?1, ?2, ?3, COALESCE(?4, datetime('now')))",
                        params![sid, tid, link.link_type, link.created_at],
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

        store.export(dir.path(), None)?;

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
        store.export(dir.path(), None)?;

        // Import into a fresh store
        let store2 = MemoryStore::in_memory()?;
        let mut embedder = Embedder::new()?;
        let result = store2.import(dir.path(), &mut embedder)?;

        assert_eq!(result.created, 2);
        assert_eq!(result.updated, 0);
        assert_eq!(result.unchanged, 0);

        // Verify links were recreated
        let links = store2.get_links("project design")?;
        assert!(!links.is_empty(), "links should be recreated on import");

        Ok(())
    }

    #[test]
    fn test_roundtrip_preserves_all_attributes() -> Result<()> {
        let store = MemoryStore::in_memory()?;
        let emb1: Vec<f32> = vec![0.1; 384];
        let emb2: Vec<f32> = vec![-0.5; 384];

        store.memorize("alpha", "content A", &["t1".into()], &emb1)?;
        store.memorize("beta", "content B", &["t2".into()], &emb2)?;
        store.add_mnemonic("alpha", "alpha-alias", &emb1)?;
        store.rate("alpha", true)?;
        store.rate("alpha", true)?;
        store.rate("alpha", false)?;
        store.link("alpha", "beta", "related")?;
        store.recall(&emb1, 5, None, None, None)?; // bumps recall_count + last_recalled_at

        let before = store.get_memory_by_mnemonic("alpha")?.unwrap();
        assert!(before.recall_count > 0 && before.last_recalled_at.is_some());
        assert_eq!((before.useful_count, before.not_useful_count), (2, 1));

        let dir = TempDir::new()?;
        store.export(dir.path(), None)?;
        let store2 = MemoryStore::in_memory()?;
        let mut embedder = Embedder::new()?;
        store2.import(dir.path(), &mut embedder)?;
        let after = store2.get_memory_by_mnemonic("alpha")?.unwrap();

        // Stats must survive the roundtrip.
        assert_eq!(before.recall_count, after.recall_count, "recall_count");
        assert_eq!(before.useful_count, after.useful_count, "useful_count");
        assert_eq!(
            before.not_useful_count, after.not_useful_count,
            "not_useful_count"
        );
        assert_eq!(
            before.last_recalled_at, after.last_recalled_at,
            "last_recalled_at"
        );
        assert_eq!(before.created_at, after.created_at, "created_at");
        assert_eq!(before.updated_at, after.updated_at, "updated_at");

        // And the content/tags/mnemonics/links we already carried.
        assert_eq!(before.content, after.content, "content");
        assert_eq!(before.tags, after.tags, "tags");
        assert_eq!(before.mnemonics, after.mnemonics, "mnemonics");
        assert_eq!(before.links.len(), after.links.len(), "links");
        // Link created_at preserved too.
        assert_eq!(
            before.links[0].created_at, after.links[0].created_at,
            "link created_at"
        );

        Ok(())
    }

    #[test]
    fn test_import_idempotent() -> Result<()> {
        let store = make_store_with_data()?;
        let dir = TempDir::new()?;
        store.export(dir.path(), None)?;

        // Import twice into same store
        let store2 = MemoryStore::in_memory()?;
        let mut embedder = Embedder::new()?;
        let r1 = store2.import(dir.path(), &mut embedder)?;
        assert_eq!(r1.created, 2);

        let r2 = store2.import(dir.path(), &mut embedder)?;
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
