use crate::candidate::Candidate;

/// Stable sort by score descending, then title.
pub fn rank_candidates(mut items: Vec<Candidate>) -> Vec<Candidate> {
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Action, Source};

    fn item(title: &str, score: f32) -> Candidate {
        Candidate {
            id: title.into(),
            title: title.into(),
            subtitle: None,
            score,
            source: Source::App,
            actions: vec![Action::open_default()],
            plugin_id: None,
        }
    }

    #[test]
    fn ranks_by_score_desc() {
        let ranked = rank_candidates(vec![item("b", 0.5), item("a", 0.9)]);
        assert_eq!(ranked[0].title, "a");
        assert_eq!(ranked[1].title, "b");
    }
}
