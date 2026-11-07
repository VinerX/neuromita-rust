//! Explicit, snapshot-bound inspection of forgotten facts. Never a prompt input.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::{
    identity::CharacterId,
    memory::{CharacterMemories, MemoryId, MemoryKind, MemoryRecord, MemoryState},
    timeline::ContextAnchor,
};

use super::memory_context::{priority_rank, tokens};

pub const MAX_RECALL_RESULTS: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallSelector {
    /// Only exact forgotten-fact IDs; unknown, duplicate or ineligible IDs fail closed.
    Ids(Vec<MemoryId>),
    /// Exact lexical token intersection with retained forgotten facts, never embeddings.
    Text { query: String, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalledFact {
    pub record: MemoryRecord,
    pub matching_tokens: usize,
}

/// Provenance is tied to the original character and immutable Timeline prefix.
/// Returned records remain Forgotten and are never implicitly restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgottenRecall {
    pub character: CharacterId,
    pub anchor: ContextAnchor,
    pub facts: Vec<RecalledFact>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecallError {
    #[error("recall must request between 1 and 12 results")]
    InvalidLimit,
    #[error("recall query has no searchable tokens")]
    InvalidQuery,
    #[error("recall contains duplicate memory identities")]
    DuplicateId,
    #[error("memory is missing or is not a forgotten fact")]
    IneligibleId,
}

/// Explicit caller action, strictly separate from KnowledgeRetrieval::select.
/// `memories` must come from a captured Timeline snapshot for `character` at `anchor`.
pub fn recall_forgotten(
    character: CharacterId,
    anchor: ContextAnchor,
    memories: &CharacterMemories,
    selector: RecallSelector,
) -> Result<ForgottenRecall, RecallError> {
    let facts = match selector {
        RecallSelector::Ids(ids) => {
            if ids.is_empty() || ids.len() > MAX_RECALL_RESULTS {
                return Err(RecallError::InvalidLimit);
            }
            let mut seen = BTreeSet::new();
            ids.into_iter()
                .map(|id| {
                    if !seen.insert(id) {
                        return Err(RecallError::DuplicateId);
                    }
                    let record = memories.get(id).ok_or(RecallError::IneligibleId)?;
                    if record.kind != MemoryKind::Fact || record.state != MemoryState::Forgotten {
                        return Err(RecallError::IneligibleId);
                    }
                    Ok(RecalledFact {
                        record: record.clone(),
                        matching_tokens: 0,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        RecallSelector::Text { query, limit } => {
            if limit == 0 || limit > MAX_RECALL_RESULTS {
                return Err(RecallError::InvalidLimit);
            }
            let query_tokens = tokens(&query);
            if query_tokens.is_empty() {
                return Err(RecallError::InvalidQuery);
            }
            let mut ranked = memories
                .forgotten()
                .into_iter()
                .filter_map(|record| {
                    let matching_tokens =
                        tokens(&record.content).intersection(&query_tokens).count();
                    (matching_tokens > 0).then_some(RecalledFact {
                        record,
                        matching_tokens,
                    })
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|a, b| {
                b.matching_tokens
                    .cmp(&a.matching_tokens)
                    .then_with(|| {
                        priority_rank(b.record.priority).cmp(&priority_rank(a.record.priority))
                    })
                    .then_with(|| a.record.id.cmp(&b.record.id))
            });
            ranked.truncate(limit);
            ranked
        }
    };
    Ok(ForgottenRecall {
        character,
        anchor,
        facts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryChange, MemoryPriority};
    use uuid::Uuid;

    fn sample() -> CharacterMemories {
        let mut book = CharacterMemories::default();
        for (i, state, priority, text) in [
            (
                1,
                MemoryState::Forgotten,
                MemoryPriority::Normal,
                "Player likes green tea",
            ),
            (
                2,
                MemoryState::Forgotten,
                MemoryPriority::High,
                "Green tea is calming",
            ),
            (
                3,
                MemoryState::Active,
                MemoryPriority::Critical,
                "Green tea is secret",
            ),
            (
                4,
                MemoryState::Deleted,
                MemoryPriority::Critical,
                "Green tea is deleted",
            ),
            (
                5,
                MemoryState::Forgotten,
                MemoryPriority::Low,
                "A red scarf",
            ),
        ] {
            let mut record = MemoryRecord::fact(MemoryId(Uuid::from_u128(i)), priority, text);
            record.state = state;
            book.apply_committed(&MemoryChange::Upsert(record)).unwrap();
        }
        book
    }

    fn anchor() -> ContextAnchor {
        ContextAnchor {
            branch_id: Uuid::from_u128(99),
            through: 51,
        }
    }

    #[test]
    fn explicit_recall_is_ranked_bounded_and_does_not_restore() {
        let book = sample();
        let selection = RecallSelector::Text {
            query: "green tea".into(),
            limit: 2,
        };
        let recalled = recall_forgotten(CharacterId::Crazy, anchor(), &book, selection).unwrap();
        assert_eq!(recalled.character, CharacterId::Crazy);
        assert_eq!(recalled.anchor, anchor());
        assert_eq!(
            recalled
                .facts
                .iter()
                .map(|x| x.record.id.0.as_u128())
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(
            recalled
                .facts
                .iter()
                .all(|x| x.record.state == MemoryState::Forgotten && x.matching_tokens == 2)
        );
        assert_eq!(
            book.get(MemoryId(Uuid::from_u128(1))).unwrap().state,
            MemoryState::Forgotten
        );
    }

    #[test]
    fn explicit_ids_fail_closed_without_partial_disclosure() {
        let book = sample();
        let id = |n| MemoryId(Uuid::from_u128(n));
        let valid = recall_forgotten(
            CharacterId::Crazy,
            anchor(),
            &book,
            RecallSelector::Ids(vec![id(5), id(1)]),
        )
        .unwrap();
        assert_eq!(
            valid.facts.iter().map(|x| x.record.id).collect::<Vec<_>>(),
            vec![id(5), id(1)]
        );
        for bad in [3, 4, 99] {
            assert_eq!(
                recall_forgotten(
                    CharacterId::Crazy,
                    anchor(),
                    &book,
                    RecallSelector::Ids(vec![id(1), id(bad)])
                ),
                Err(RecallError::IneligibleId)
            );
        }
        assert_eq!(
            recall_forgotten(
                CharacterId::Crazy,
                anchor(),
                &book,
                RecallSelector::Ids(vec![id(1), id(1)])
            ),
            Err(RecallError::DuplicateId)
        );
        assert_eq!(
            recall_forgotten(
                CharacterId::Crazy,
                anchor(),
                &book,
                RecallSelector::Ids(vec![])
            ),
            Err(RecallError::InvalidLimit)
        );
    }

    #[test]
    fn invalid_and_irrelevant_queries_never_dump_forgotten_corpus() {
        let book = sample();
        let query = |text: &str, limit| RecallSelector::Text {
            query: text.into(),
            limit,
        };
        assert_eq!(
            recall_forgotten(CharacterId::Crazy, anchor(), &book, query("!", 2)),
            Err(RecallError::InvalidQuery)
        );
        assert_eq!(
            recall_forgotten(CharacterId::Crazy, anchor(), &book, query("tea", 0)),
            Err(RecallError::InvalidLimit)
        );
        assert_eq!(
            recall_forgotten(CharacterId::Crazy, anchor(), &book, query("tea", 13)),
            Err(RecallError::InvalidLimit)
        );
        assert!(
            recall_forgotten(CharacterId::Crazy, anchor(), &book, query("aardvark", 12))
                .unwrap()
                .facts
                .is_empty()
        );
    }
}
