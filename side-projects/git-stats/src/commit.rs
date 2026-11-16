use std::fmt;
use std::sync::OnceLock;

fn commit_type_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^(\w+)(\([^)]*\))?!?:\s")
            .expect("hardcoded commit parser regular expression must compile")
    })
}

fn co_author_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)^Co-Authored-By:\s*(.+?)\s*<([^>]+)>")
            .expect("hardcoded commit parser regular expression must compile")
    })
}

/// Parsed conventional commit type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommitType {
    Feat,
    Fix,
    Refactor,
    Chore,
    Docs,
    Test,
    Other,
}

impl CommitType {
    #[must_use]
    pub fn from_subject(subject: &str) -> Self {
        // Match "type(scope): ..." or "type: ..."
        if let Some(caps) = commit_type_re().captures(subject) {
            match caps
                .get(1)
                .expect("the matched commit expression requires this capturing group")
                .as_str()
            {
                "feat" => CommitType::Feat,
                "fix" => CommitType::Fix,
                "refactor" => CommitType::Refactor,
                "chore" => CommitType::Chore,
                "docs" => CommitType::Docs,
                "test" => CommitType::Test,
                _ => CommitType::Other,
            }
        } else {
            CommitType::Other
        }
    }
}

impl fmt::Display for CommitType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitType::Feat => write!(f, "feat"),
            CommitType::Fix => write!(f, "fix"),
            CommitType::Refactor => write!(f, "refactor"),
            CommitType::Chore => write!(f, "chore"),
            CommitType::Docs => write!(f, "docs"),
            CommitType::Test => write!(f, "test"),
            CommitType::Other => write!(f, "other"),
        }
    }
}

/// A single file change in a commit
#[derive(Debug, Clone)]
pub struct FileChange {
    pub added: u64,
    pub deleted: u64,
}

/// Co-author extracted from commit message
#[derive(Debug, Clone)]
pub struct CoAuthor {
    pub name: String,
    pub email: String,
}

/// A parsed commit with all relevant fields
#[derive(Debug, Clone)]
pub struct ParsedCommit {
    #[allow(dead_code)]
    pub hash: String,
    pub author_name: String,
    pub author_email: String,
    #[allow(dead_code)]
    pub subject: String,
    pub commit_type: CommitType,
    pub co_authors: Vec<CoAuthor>,
    pub files: Vec<FileChange>,
}

/// Extract all Co-Authored-By lines from commit body.
/// Matches: "Co-Authored-By: Name <email>" (case-insensitive)
#[must_use]
pub fn parse_co_authors(body: &str) -> Vec<CoAuthor> {
    let re = co_author_re();
    body.lines()
        .filter_map(|line| {
            re.captures(line).map(|caps| CoAuthor {
                name: caps
                    .get(1)
                    .expect("the matched commit expression requires this capturing group")
                    .as_str()
                    .trim()
                    .to_string(),
                email: caps
                    .get(2)
                    .expect("the matched commit expression requires this capturing group")
                    .as_str()
                    .trim()
                    .to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commit_type_feat() {
        assert_eq!(
            CommitType::from_subject("feat(auth): add login"),
            CommitType::Feat
        );
        assert_eq!(
            CommitType::from_subject("feat: add login"),
            CommitType::Feat
        );
    }

    #[test]
    fn test_commit_type_fix() {
        assert_eq!(
            CommitType::from_subject("fix: crash on null"),
            CommitType::Fix
        );
    }

    #[test]
    fn test_commit_type_other() {
        assert_eq!(
            CommitType::from_subject("random message without type"),
            CommitType::Other
        );
        assert_eq!(CommitType::from_subject("WIP: stuff"), CommitType::Other);
    }

    #[test]
    fn test_parse_co_authors_single() {
        let body = "Some description\n\nCo-Authored-By: Alice <alice@corp.com>";
        let result = parse_co_authors(body);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "Alice");
        assert_eq!(result[0].email, "alice@corp.com");
    }

    #[test]
    fn test_parse_co_authors_multiple() {
        let body = "Co-Authored-By: Alice <alice@corp.com>\nCo-authored-by: Bob <bob@corp.com>";
        let result = parse_co_authors(body);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_co_authors_empty() {
        let body = "Just a normal commit\nNo co-authors here";
        let result = parse_co_authors(body);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_commit_type_refactor() {
        assert_eq!(
            CommitType::from_subject("refactor(db): simplify"),
            CommitType::Refactor
        );
    }

    #[test]
    fn test_commit_type_chore() {
        assert_eq!(
            CommitType::from_subject("chore: update deps"),
            CommitType::Chore
        );
    }

    #[test]
    fn test_commit_type_docs() {
        assert_eq!(
            CommitType::from_subject("docs(readme): clarify"),
            CommitType::Docs
        );
    }

    #[test]
    fn test_commit_type_test() {
        assert_eq!(
            CommitType::from_subject("test(auth): add coverage"),
            CommitType::Test
        );
    }

    #[test]
    fn test_breaking_change() {
        assert_eq!(
            CommitType::from_subject("feat!: breaking API"),
            CommitType::Feat
        );
    }

    #[test]
    fn test_co_author_spaces_in_name() {
        let body = "Co-Authored-By: Mary Jane <mj@test.com>";
        let result = parse_co_authors(body);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "Mary Jane");
    }
}
