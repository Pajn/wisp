#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchItem {
    pub id: String,
    pub primary_text: String,
    pub secondary_text: Option<String>,
    pub search_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    pub id: String,
    pub score: i64,
}

pub trait Matcher {
    fn set_items(&mut self, items: Vec<MatchItem>);
    fn query(&self, input: &str) -> Vec<MatchResult>;
}

#[derive(Debug, Clone, Default)]
pub struct SimpleMatcher {
    items: Vec<MatchItem>,
}

impl Matcher for SimpleMatcher {
    fn set_items(&mut self, items: Vec<MatchItem>) {
        self.items = items;
    }

    fn query(&self, input: &str) -> Vec<MatchResult> {
        let normalized_query = normalize_query(input);
        if normalized_query.is_empty() {
            return self
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| MatchResult {
                    id: item.id.clone(),
                    score: -(index as i64),
                })
                .collect();
        }

        let mut matches = self
            .items
            .iter()
            .filter_map(|item| {
                score_match(&item.search_text, &normalized_query).map(|score| MatchResult {
                    id: item.id.clone(),
                    score,
                })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });
        matches
    }
}

#[must_use]
pub fn normalize_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn score_match(haystack: &str, query: &str) -> Option<i64> {
    let haystack = normalize_query(haystack);
    if haystack.contains(query) {
        let density_bonus = query.len() as i64 * 10;
        let position_bonus = -(haystack.find(query).unwrap_or_default() as i64);
        return Some(1_000 + density_bonus + position_bonus);
    }

    let mut score = 0_i64;
    let mut haystack_index = 0_usize;
    for needle in query.chars() {
        let remaining = &haystack[haystack_index..];
        let offset = remaining.find(needle)?;
        haystack_index += offset + needle.len_utf8();
        score += 10 - offset as i64;
    }

    Some(score)
}

#[cfg(test)]
mod tests {
    use crate::{MatchItem, Matcher, SimpleMatcher, normalize_query};

    fn item(id: &str, search_text: &str) -> MatchItem {
        MatchItem {
            id: id.to_string(),
            primary_text: id.to_string(),
            secondary_text: None,
            search_text: search_text.to_string(),
        }
    }

    #[test]
    fn normalizes_queries_for_matching() {
        assert_eq!(normalize_query("  Foo   Bar  "), "foo bar");
    }

    #[test]
    fn preserves_input_order_for_empty_queries() {
        let mut matcher = SimpleMatcher::default();
        matcher.set_items(vec![item("a", "alpha"), item("b", "beta")]);

        let matches = matcher.query("");

        assert_eq!(matches[0].id, "a");
        assert_eq!(matches[1].id, "b");
    }

    #[test]
    fn ranks_better_matches_higher() {
        let mut matcher = SimpleMatcher::default();
        matcher.set_items(vec![item("alpha", "alpha"), item("beta", "alphabet")]);

        let matches = matcher.query("alph");

        assert_eq!(matches[0].id, "alpha");
    }

    #[test]
    fn handles_duplicate_labels_with_stable_ids() {
        let mut matcher = SimpleMatcher::default();
        matcher.set_items(vec![item("1", "workspace"), item("2", "workspace")]);

        let matches = matcher.query("work");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].id, "1");
    }
}
