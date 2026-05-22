//! Analysis-cache key composition — mirrors TS `analyzer/cacheKey.ts`.
//! The on-disk layout (one file per entry under `.codeup/cache/entries/`)
//! lives in the CLI binary; this module only owns the pure key shape.

pub fn analysis_cache_key(
    content_hash: &str,
    catalogue_hash: &str,
    model: &str,
    neighbors_key: &str,
) -> String {
    format!("{content_hash}:{catalogue_hash}:{model}:{neighbors_key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(
            analysis_cache_key("h1", "cat1", "sonnet", "n1"),
            analysis_cache_key("h1", "cat1", "sonnet", "n1")
        );
    }

    #[test]
    fn sensitive_to_each_component() {
        let base = analysis_cache_key("h1", "cat1", "sonnet", "");
        assert_ne!(analysis_cache_key("h2", "cat1", "sonnet", ""), base);
        assert_ne!(analysis_cache_key("h1", "cat2", "sonnet", ""), base);
        assert_ne!(analysis_cache_key("h1", "cat1", "opus", ""), base);
        assert_ne!(analysis_cache_key("h1", "cat1", "sonnet", "n2"), base);
    }

    #[test]
    fn empty_neighbors_yields_trailing_colon() {
        assert!(analysis_cache_key("h1", "cat1", "sonnet", "").ends_with("sonnet:"));
    }
}
