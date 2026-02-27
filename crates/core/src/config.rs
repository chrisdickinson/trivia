use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Deserialize)]
pub struct TriviaConfig {
    #[serde(default)]
    pub memorize: MemorizeConfig,
    #[serde(default)]
    pub recall: RecallConfig,
    #[serde(default)]
    pub export: ExportConfig,
    pub database: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct MemorizeConfig {
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct RecallConfig {
    #[serde(default)]
    pub tags: Vec<String>,
    pub min_score: Option<f64>,
    pub body_max_chars: Option<usize>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ExportConfig {
    #[serde(default)]
    pub tags: Vec<String>,
}

impl TriviaConfig {
    /// Walk up from `start_dir` looking for `trivia.toml`.
    /// Returns default config if not found.
    pub fn discover(start_dir: &Path) -> Result<(Self, Option<PathBuf>)> {
        let mut dir = start_dir.to_path_buf();
        loop {
            let candidate = dir.join("trivia.toml");
            if candidate.is_file() {
                let contents = std::fs::read_to_string(&candidate)?;
                let config: TriviaConfig = toml::from_str(&contents)?;
                return Ok((config, Some(candidate)));
            }
            if !dir.pop() {
                break;
            }
        }
        Ok((TriviaConfig::default(), None))
    }

    /// Load from a specific path. Returns default if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if path.is_file() {
            let contents = std::fs::read_to_string(path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(TriviaConfig::default())
        }
    }

    /// Merge explicit CLI tags with config tags (union, config first).
    pub fn merge_tags(config_tags: &[String], explicit_tags: &[String]) -> Vec<String> {
        let mut merged = config_tags.to_vec();
        for t in explicit_tags {
            if !merged.contains(t) {
                merged.push(t.clone());
            }
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_discover_finds_toml() -> Result<()> {
        let dir = TempDir::new()?;
        let toml_path = dir.path().join("trivia.toml");
        fs::write(
            &toml_path,
            r#"
[memorize]
tags = ["project-x"]

[recall]
tags = ["project-x", "backend"]
"#,
        )?;

        let sub = dir.path().join("deep").join("nested");
        fs::create_dir_all(&sub)?;

        let (config, found) = TriviaConfig::discover(&sub)?;
        assert!(found.is_some());
        assert_eq!(config.memorize.tags, vec!["project-x"]);
        assert_eq!(config.recall.tags, vec!["project-x", "backend"]);
        Ok(())
    }

    #[test]
    fn test_discover_returns_default_when_missing() -> Result<()> {
        let dir = TempDir::new()?;
        let (config, found) = TriviaConfig::discover(dir.path())?;
        assert!(found.is_none());
        assert!(config.memorize.tags.is_empty());
        Ok(())
    }

    #[test]
    fn test_merge_tags() {
        let config = vec!["a".into(), "b".into()];
        let explicit = vec!["b".into(), "c".into()];
        let merged = TriviaConfig::merge_tags(&config, &explicit);
        assert_eq!(merged, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_database_field() -> Result<()> {
        let dir = TempDir::new()?;
        let toml_path = dir.path().join("trivia.toml");
        fs::write(&toml_path, "database = \"/tmp/my.db\"\n")?;

        let config = TriviaConfig::load(&toml_path)?;
        assert_eq!(config.database.as_deref(), Some("/tmp/my.db"));
        Ok(())
    }
}
