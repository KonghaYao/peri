use std::collections::HashMap;

use crate::commit::{CommitType, ParsedCommit};

/// Aggregated statistics for one person (keyed by email)
#[derive(Debug, Clone)]
pub struct PersonStats {
    pub name: String,
    pub email: String,
    pub commits: u64,
    pub added_lines: u64,
    pub deleted_lines: u64,
    pub files_touched: u64,
    pub feat: u64,
    pub fix: u64,
    pub refactor_: u64,
    pub chore: u64,
    pub docs: u64,
    pub test: u64,
    pub other: u64,
    pub co_authored_with: Vec<String>,
}

impl PersonStats {
    fn new(name: String, email: String) -> Self {
        PersonStats {
            name,
            email,
            commits: 0,
            added_lines: 0,
            deleted_lines: 0,
            files_touched: 0,
            feat: 0,
            fix: 0,
            refactor_: 0,
            chore: 0,
            docs: 0,
            test: 0,
            other: 0,
            co_authored_with: Vec::new(),
        }
    }

    fn add_commit(&mut self, commit: &ParsedCommit) {
        self.commits += 1;
        for f in &commit.files {
            self.added_lines += f.added;
            self.deleted_lines += f.deleted;
        }
        self.files_touched += commit.files.len() as u64;
        match commit.commit_type {
            CommitType::Feat => self.feat += 1,
            CommitType::Fix => self.fix += 1,
            CommitType::Refactor => self.refactor_ += 1,
            CommitType::Chore => self.chore += 1,
            CommitType::Docs => self.docs += 1,
            CommitType::Test => self.test += 1,
            CommitType::Other => self.other += 1,
        }
    }
}

fn is_ai_email(email: &str) -> bool {
    email.contains("claude-code") || email.contains("@anthropic.com")
}

/// Aggregate commits into per-person stats.
///
/// Attribution logic:
/// - If any co-author has an AI-pattern email (claude-code-best.win / @anthropic.com),
///   the commit is attributed ONLY to those AI co-authors (human author excluded).
/// - Otherwise, author and all co-authors each get full credit as usual.
/// - Input follows git log newest-first order; the first name seen for an email wins.
pub fn aggregate(commits: &[ParsedCommit]) -> Vec<PersonStats> {
    let mut map: HashMap<String, PersonStats> = HashMap::new();

    for commit in commits {
        // All people involved: author + co-authors
        let mut people: Vec<(String, String)> = Vec::new();
        people.push((commit.author_name.clone(), commit.author_email.clone()));
        for ca in &commit.co_authors {
            people.push((ca.name.clone(), ca.email.clone()));
        }

        // Deduplicate by email (same email can appear as both author and co-author)
        people.sort_by(|a, b| a.1.cmp(&b.1));
        people.dedup_by(|a, b| a.1 == b.1);

        // Check if any co-author is an AI model — if so, attribute only to AI
        let has_ai_co_author = commit.co_authors.iter().any(|ca| is_ai_email(&ca.email));
        let attributed: Vec<&(String, String)> = if has_ai_co_author {
            people
                .iter()
                .filter(|(_, email)| is_ai_email(email))
                .collect()
        } else {
            people.iter().collect()
        };

        // Each attributed person gets full credit
        for (name, email) in &attributed {
            let entry = map
                .entry((*email).clone())
                .or_insert_with(|| PersonStats::new((*name).clone(), (*email).clone()));
            entry.add_commit(commit);
        }

        // Record co-author relationships (only when no AI attribution — human context)
        if !has_ai_co_author {
            for ca in &commit.co_authors {
                if let Some(author_stats) = map.get_mut(&commit.author_email) {
                    if !author_stats.co_authored_with.contains(&ca.name) {
                        author_stats.co_authored_with.push(ca.name.clone());
                    }
                }
            }
        }
    }

    let mut stats: Vec<PersonStats> = map.into_values().collect();
    // Sort by commits descending, then by email ascending for deterministic ties
    stats.sort_by(|a, b| {
        b.commits
            .cmp(&a.commits)
            .then_with(|| a.email.cmp(&b.email))
    });
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{CoAuthor, FileChange};

    fn make_commit(
        author: &str,
        email: &str,
        subject: &str,
        co_authors: Vec<CoAuthor>,
    ) -> ParsedCommit {
        ParsedCommit {
            hash: "abc".into(),
            author_name: author.into(),
            author_email: email.into(),
            subject: subject.into(),
            commit_type: CommitType::from_subject(subject),
            co_authors,
            files: vec![FileChange {
                added: 5,
                deleted: 2,
            }],
        }
    }

    #[test]
    fn test_aggregate_single_author() {
        let commits = vec![
            make_commit("Alice", "alice@x.com", "feat: login", vec![]),
            make_commit("Alice", "alice@x.com", "fix: bug", vec![]),
        ];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].commits, 2);
        assert_eq!(stats[0].feat, 1);
        assert_eq!(stats[0].fix, 1);
        assert_eq!(stats[0].added_lines, 10);
        assert_eq!(stats[0].deleted_lines, 4);
    }

    #[test]
    fn test_aggregate_with_co_authors() {
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "feat: a",
            vec![CoAuthor {
                name: "Bob".into(),
                email: "bob@x.com".into(),
            }],
        )];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 2);
        for s in &stats {
            assert_eq!(s.commits, 1);
            assert_eq!(s.added_lines, 5);
        }
    }

    #[test]
    fn test_aggregate_merge_by_email() {
        let commits = vec![
            make_commit("Alice Wang", "alice@x.com", "feat: a", vec![]),
            make_commit("A. Wang", "alice@x.com", "fix: b", vec![]),
        ];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].commits, 2);
    }

    #[test]
    fn test_sort_by_commits_desc() {
        let commits = vec![
            make_commit("Alice", "alice@x.com", "feat: a", vec![]),
            make_commit("Bob", "bob@x.com", "feat: b", vec![]),
            make_commit("Bob", "bob@x.com", "fix: c", vec![]),
        ];
        let stats = aggregate(&commits);
        assert_eq!(stats[0].email, "bob@x.com");
        assert_eq!(stats[1].email, "alice@x.com");
    }

    #[test]
    fn test_co_author_same_email_as_author_not_double_counted() {
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "feat: a",
            vec![CoAuthor {
                name: "Alice".into(),
                email: "alice@x.com".into(),
            }],
        )];
        let stats = aggregate(&commits);
        assert_eq!(
            stats.len(),
            1,
            "Same email should not create duplicate entry"
        );
        assert_eq!(stats[0].commits, 1, "Should only count once");
        assert_eq!(stats[0].added_lines, 5, "Lines should only count once");
    }

    #[test]
    fn test_ai_co_author_takes_over_attribution() {
        // Human author + AI co-author → only AI gets credit
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "feat: login",
            vec![CoAuthor {
                name: "deepseek-v4-pro".into(),
                email: "deepseek-ai@claude-code-best.win".into(),
            }],
        )];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 1, "Only AI should be counted");
        assert_eq!(stats[0].name, "deepseek-v4-pro");
        assert_eq!(stats[0].email, "deepseek-ai@claude-code-best.win");
        assert_eq!(stats[0].commits, 1);
    }

    #[test]
    fn test_anthropic_co_author_takes_over_attribution() {
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "fix: bug",
            vec![CoAuthor {
                name: "Claude Opus 4.7".into(),
                email: "claude@anthropic.com".into(),
            }],
        )];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].email, "claude@anthropic.com");
        assert_eq!(stats[0].commits, 1);
    }

    #[test]
    fn test_human_co_author_still_shared_attribution() {
        // No AI co-author → both human author and human co-author get credit
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "feat: a",
            vec![CoAuthor {
                name: "Bob".into(),
                email: "bob@corp.com".into(),
            }],
        )];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 2, "Both humans should be counted");
        let alice = stats.iter().find(|s| s.email == "alice@x.com").unwrap();
        let bob = stats.iter().find(|s| s.email == "bob@corp.com").unwrap();
        assert_eq!(alice.commits, 1);
        assert_eq!(bob.commits, 1);
    }

    #[test]
    fn test_multiple_ai_co_authors_all_get_credit() {
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "feat: a",
            vec![
                CoAuthor {
                    name: "deepseek-v4-pro".into(),
                    email: "deepseek-ai@claude-code-best.win".into(),
                },
                CoAuthor {
                    name: "Claude Opus 4.7".into(),
                    email: "claude@anthropic.com".into(),
                },
            ],
        )];
        let stats = aggregate(&commits);
        assert_eq!(stats.len(), 2, "Both AI models should be counted");
        assert!(stats.iter().all(|s| s.commits == 1));
        // Human author (alice@x.com) should NOT appear
        assert!(stats.iter().all(|s| s.email != "alice@x.com"));
    }

    #[test]
    fn test_co_author_relationship_recorded_on_first_commit() {
        let commits = vec![make_commit(
            "Alice",
            "alice@x.com",
            "feat: a",
            vec![CoAuthor {
                name: "Bob".into(),
                email: "bob@x.com".into(),
            }],
        )];
        let stats = aggregate(&commits);
        let alice = stats.iter().find(|s| s.email == "alice@x.com").unwrap();
        assert!(
            alice.co_authored_with.contains(&"Bob".to_string()),
            "Alice's co_authored_with should contain Bob even on first commit"
        );
    }
}
