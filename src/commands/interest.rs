//! Shared engagement counts for issue and PR interest snapshots.
use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Engagement {
    pub(super) reactions: u64,
    pub(super) unique_commenters: usize,
    pub(super) unattributed_comments: usize,
}

/// Keep identities only while collecting; snapshots contain aggregate counts.
#[derive(Default)]
pub(super) struct Commenters {
    authors: HashSet<String>,
    unattributed: usize,
}

impl Commenters {
    pub(super) fn add(&mut self, login: Option<&str>) {
        match login {
            Some(login) if !login.is_empty() => {
                self.authors.insert(login.to_lowercase());
            }
            _ => self.unattributed += 1,
        }
    }

    pub(super) fn finish(self, reactions: u64) -> Engagement {
        Engagement {
            reactions,
            unique_commenters: self.authors.len(),
            unattributed_comments: self.unattributed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_accounts_case_insensitively_without_persisting_identities() {
        let mut commenters = Commenters::default();
        for login in [
            Some("Author"),
            Some("AUTHOR"),
            Some("other[bot]"),
            None,
            Some(""),
        ] {
            commenters.add(login);
        }
        assert_eq!(
            serde_json::to_value(commenters.finish(7)).unwrap(),
            json!({"reactions": 7, "uniqueCommenters": 2, "unattributedComments": 2})
        );
    }
}
