use std::borrow::Cow;

/// Accept historical separator spaces without changing spaces inside label names.
pub(crate) fn normalize(name: &str) -> Cow<'_, str> {
    match name.split_once(':') {
        Some((prefix, suffix)) if suffix != suffix.trim_start() => {
            Cow::Owned(format!("{prefix}:{}", suffix.trim_start()))
        }
        _ => Cow::Borrowed(name),
    }
}

pub(crate) fn is_awaiting_author(name: &str) -> bool {
    normalize(name) == "meta:awaiting author"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_separator_spellings_without_collapsing_word_spaces() {
        for name in ["meta: awaiting author", "meta:awaiting author"] {
            assert!(is_awaiting_author(name));
        }
        for name in [
            "meta:awaiting author soon",
            "awaiting author",
            "meta:awaitingauthor",
        ] {
            assert!(!is_awaiting_author(name));
        }
        assert_eq!(normalize("domain: external docs"), "domain:external docs");
        assert_eq!(normalize("good first issue"), "good first issue");
    }
}
