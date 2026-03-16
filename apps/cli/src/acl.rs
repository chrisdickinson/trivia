use std::fmt;
use std::str::FromStr;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessLevel {
    None,
    Read,
    Update,
}

impl fmt::Display for AccessLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccessLevel::None => write!(f, "none"),
            AccessLevel::Read => write!(f, "read"),
            AccessLevel::Update => write!(f, "update"),
        }
    }
}

impl FromStr for AccessLevel {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(AccessLevel::None),
            "read" => Ok(AccessLevel::Read),
            "update" => Ok(AccessLevel::Update),
            _ => Err(anyhow!("invalid access level: {s} (expected none, read, or update)")),
        }
    }
}

#[derive(Debug, Clone)]
struct AclRule {
    pattern: String,
    level: AccessLevel,
}

#[derive(Debug, Clone)]
pub struct Acl {
    rules: Vec<AclRule>,
}

impl fmt::Display for Acl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .rules
            .iter()
            .map(|r| format!("{}:{}", r.pattern, r.level))
            .collect();
        write!(f, "{}", parts.join(","))
    }
}

impl Acl {
    /// Everything allowed — for stdio MCP (local, trusted).
    pub fn open() -> Self {
        Acl {
            rules: vec![AclRule {
                pattern: "*".to_string(),
                level: AccessLevel::Update,
            }],
        }
    }

    /// Everything denied — default when `--share` is not provided.
    pub fn closed() -> Self {
        Acl {
            rules: vec![AclRule {
                pattern: "*".to_string(),
                level: AccessLevel::None,
            }],
        }
    }

    pub fn is_open(&self) -> bool {
        self.rules.len() == 1
            && self.rules[0].pattern == "*"
            && self.rules[0].level == AccessLevel::Update
    }

    /// Parse a spec like "project:read,notes:update,*:none".
    pub fn parse(spec: &str) -> Result<Self> {
        let mut rules = Vec::new();
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (pattern, level_str) = part
                .rsplit_once(':')
                .ok_or_else(|| anyhow!("invalid ACL rule (expected pattern:level): {part}"))?;
            let level = level_str.trim().parse::<AccessLevel>()?;
            rules.push(AclRule {
                pattern: pattern.trim().to_string(),
                level,
            });
        }
        if rules.is_empty() {
            return Err(anyhow!("empty ACL spec"));
        }
        Ok(Acl { rules })
    }

    /// Access level for a single tag. First-match-wins.
    pub fn tag_level(&self, tag: &str) -> AccessLevel {
        for rule in &self.rules {
            if rule.pattern == "*" || rule.pattern == tag {
                return rule.level;
            }
        }
        AccessLevel::None
    }

    /// Effective access level for a memory. Max across all its tags.
    /// Untagged memories match against `*`.
    pub fn memory_level(&self, tags: &[String]) -> AccessLevel {
        if tags.is_empty() {
            return self.tag_level("*");
        }
        tags.iter()
            .map(|t| self.tag_level(t))
            .max()
            .unwrap_or(AccessLevel::None)
    }

    pub fn check_read(&self, tags: &[String]) -> bool {
        self.memory_level(tags) >= AccessLevel::Read
    }

    pub fn check_update(&self, tags: &[String]) -> bool {
        self.memory_level(tags) >= AccessLevel::Update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_rule() {
        let acl = Acl::parse("*:read").unwrap();
        assert_eq!(acl.tag_level("anything"), AccessLevel::Read);
        assert_eq!(acl.tag_level("*"), AccessLevel::Read);
    }

    #[test]
    fn parse_multiple_rules() {
        let acl = Acl::parse("project:update,notes:read,*:none").unwrap();
        assert_eq!(acl.tag_level("project"), AccessLevel::Update);
        assert_eq!(acl.tag_level("notes"), AccessLevel::Read);
        assert_eq!(acl.tag_level("other"), AccessLevel::None);
    }

    #[test]
    fn first_match_wins() {
        let acl = Acl::parse("*:read,project:update").unwrap();
        // * matches first, so project gets read not update
        assert_eq!(acl.tag_level("project"), AccessLevel::Read);
    }

    #[test]
    fn memory_level_max_across_tags() {
        let acl = Acl::parse("project:update,notes:read,*:none").unwrap();
        assert_eq!(
            acl.memory_level(&["notes".into(), "project".into()]),
            AccessLevel::Update
        );
        assert_eq!(
            acl.memory_level(&["notes".into()]),
            AccessLevel::Read
        );
        assert_eq!(
            acl.memory_level(&["other".into()]),
            AccessLevel::None
        );
    }

    #[test]
    fn untagged_memory_uses_wildcard() {
        let acl = Acl::parse("project:update,*:read").unwrap();
        assert_eq!(acl.memory_level(&[]), AccessLevel::Read);

        let acl2 = Acl::parse("project:update,*:none").unwrap();
        assert_eq!(acl2.memory_level(&[]), AccessLevel::None);
    }

    #[test]
    fn check_read_update() {
        let acl = Acl::parse("project:update,notes:read,*:none").unwrap();
        assert!(acl.check_read(&["project".into()]));
        assert!(acl.check_update(&["project".into()]));
        assert!(acl.check_read(&["notes".into()]));
        assert!(!acl.check_update(&["notes".into()]));
        assert!(!acl.check_read(&["other".into()]));
        assert!(!acl.check_update(&["other".into()]));
    }

    #[test]
    fn open_and_closed() {
        let open = Acl::open();
        assert!(open.is_open());
        assert!(open.check_update(&[]));
        assert!(open.check_update(&["anything".into()]));

        let closed = Acl::closed();
        assert!(!closed.is_open());
        assert!(!closed.check_read(&[]));
        assert!(!closed.check_read(&["anything".into()]));
    }

    #[test]
    fn parse_error_cases() {
        assert!(Acl::parse("").is_err());
        assert!(Acl::parse("nocolon").is_err());
        assert!(Acl::parse("tag:invalid_level").is_err());
    }

    #[test]
    fn whitespace_trimmed() {
        let acl = Acl::parse(" project : update , * : none ").unwrap();
        assert_eq!(acl.tag_level("project"), AccessLevel::Update);
        assert_eq!(acl.tag_level("other"), AccessLevel::None);
    }
}
