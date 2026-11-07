use std::collections::BTreeSet;

use super::memory::{MemoryId, MemoryKind, MemoryRecord, MemoryState};

const VOWELS: &str = "аеиоуыэюяё";
const PERFECTIVE_1: &[&str] = &["вшись", "вши", "в"];
const PERFECTIVE_2: &[&str] = &["ившись", "ывшись", "ивши", "ывши", "ив", "ыв"];
const ADJECTIVE: &[&str] = &[
    "ими", "ыми", "его", "его", "ому", "ему", "ыми", "ой", "ей", "ый", "ий", "ая", "яя", "ую",
    "юю", "ое", "ее", "ым", "им", "ом", "ем", "их", "ых", "ую", "юю", "ое", "ее", "ого",
];
const PARTICIPLE_1: &[&str] = &["ем", "нн", "вш", "ющ", "щ"];
const PARTICIPLE_2: &[&str] = &["ивш", "ывш", "ующ"];
const REFLEXIVE: &[&str] = &["ся", "сь"];
const VERB_1: &[&str] = &[
    "ла", "на", "ете", "йте", "ли", "й", "л", "ем", "н", "ло", "но", "ет", "ют", "ны", "ть", "ешь",
    "нно",
];
const VERB_2: &[&str] = &[
    "ила", "ыла", "ена", "ейте", "уйте", "ите", "или", "ыли", "ей", "уй", "ил", "ыл", "им", "ым",
    "ен", "ило", "ыло", "ено", "ят", "ует", "уют", "ит", "ыт", "ены", "ить", "ыть", "ишь", "ую",
    "ю",
];
const NOUN: &[&str] = &[
    "а", "ев", "ов", "ие", "ье", "е", "иями", "ями", "ами", "еи", "ии", "и", "ией", "ей", "ой",
    "ий", "й", "иям", "ям", "ием", "ем", "ам", "ом", "о", "у", "ах", "иях", "ях", "ы", "ь", "ию",
    "ью", "ю", "ия", "ья", "я",
];
const DERIVATIONAL: &[&str] = &["ост", "ость"];
const SUPERLATIVE: &[&str] = &["ейш", "ейше"];

fn strip_region_suffix(
    word: &str,
    rv: usize,
    endings: &[&str],
    after_a_ya: bool,
) -> Option<String> {
    let mut candidates = endings.to_vec();
    candidates.sort_by_key(|ending| std::cmp::Reverse(ending.chars().count()));
    for ending in candidates {
        let Some(base) = word.strip_suffix(ending) else {
            continue;
        };
        if base.len() < rv {
            continue;
        }
        if after_a_ya && (base.len() <= rv || !matches!(base.chars().next_back(), Some('а' | 'я')))
        {
            continue;
        }
        return Some(base.to_owned());
    }
    None
}

fn stem_russian(token: &str) -> String {
    let mut word = token.replace('ё', "е");
    if word.chars().count() < 3 || !word.chars().all(|ch| ('а'..='я').contains(&ch)) {
        return word;
    }
    let rv = word
        .char_indices()
        .find(|(_, ch)| VOWELS.contains(*ch))
        .map_or(word.len(), |(index, ch)| index + ch.len_utf8());

    if let Some(perfective) = strip_region_suffix(&word, rv, PERFECTIVE_2, false)
        .or_else(|| strip_region_suffix(&word, rv, PERFECTIVE_1, true))
    {
        word = perfective;
    } else {
        if let Some(reflexive) = strip_region_suffix(&word, rv, REFLEXIVE, false) {
            word = reflexive;
        }
        if let Some(participle) = strip_region_suffix(&word, rv, PARTICIPLE_2, false)
            .or_else(|| strip_region_suffix(&word, rv, PARTICIPLE_1, true))
        {
            word = participle;
            if let Some(adjective) = strip_region_suffix(&word, rv, ADJECTIVE, false) {
                word = adjective;
            }
        } else if let Some(adjective) = strip_region_suffix(&word, rv, ADJECTIVE, false) {
            word = adjective;
        } else if let Some(verb) = strip_region_suffix(&word, rv, VERB_2, false)
            .or_else(|| strip_region_suffix(&word, rv, VERB_1, true))
        {
            word = verb;
        } else if let Some(noun) = strip_region_suffix(&word, rv, NOUN, false) {
            word = noun;
        }
    }
    if word.get(rv..).is_some_and(|region| region.ends_with('и')) {
        word.pop();
    }
    if let Some(derived) = strip_region_suffix(&word, rv, DERIVATIONAL, false) {
        word = derived;
    }
    if word.get(rv..).is_some_and(|region| region.ends_with("нн")) {
        word.pop();
    } else if let Some(superlative) = strip_region_suffix(&word, rv, SUPERLATIVE, false) {
        word = superlative;
        if word.get(rv..).is_some_and(|region| region.ends_with("нн")) {
            word.pop();
        }
    }
    if word.get(rv..).is_some_and(|region| region.ends_with('ь')) {
        word.pop();
    }
    word
}

fn is_lexical_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ('а'..='я').contains(&ch) || ch == 'ё'
}

fn tokens(content: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut current = String::new();
    let mut flush = |current: &mut String| {
        if current.chars().count() >= 3 {
            result.insert(stem_russian(current));
        }
        current.clear();
    };
    for ch in content.to_lowercase().chars() {
        if is_lexical_char(ch) {
            current.push(ch);
        } else {
            flush(&mut current);
        }
    }
    flush(&mut current);
    result
}

/// Python-compatible lexical tokens/Jaccard, with a stable MemoryId tie-breaker.
/// The policy is explicit and cannot start embedding inference.
pub(super) fn best_active_fact<'a>(
    records: impl IntoIterator<Item = &'a MemoryRecord>,
    content: &str,
    threshold_per_mille: u16,
) -> Result<Option<MemoryId>, &'static str> {
    if !(500..=1000).contains(&threshold_per_mille) {
        return Err("lexical dedup threshold must be 500..1000 per mille");
    }
    let desired = tokens(content);
    if desired.is_empty() {
        return Ok(None);
    }
    let mut best: Option<(MemoryId, usize, usize)> = None;
    for record in records {
        if record.kind != MemoryKind::Fact || record.state != MemoryState::Active {
            continue;
        }
        let existing = tokens(&record.content);
        let intersection = desired.intersection(&existing).count();
        if intersection == 0 {
            continue;
        }
        let union = desired.len() + existing.len() - intersection;
        if intersection * 1000 < usize::from(threshold_per_mille) * union {
            continue;
        }
        let better = best.is_none_or(|(id, previous_intersection, previous_union)| {
            intersection * previous_union > previous_intersection * union
                || (intersection * previous_union == previous_intersection * union
                    && record.id < id)
        });
        if better {
            best = Some((record.id, intersection, union));
        }
    }
    Ok(best.map(|(id, _, _)| id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_reference_tokenization_and_stemming() {
        for (input, expected) in [
            ("Кошки любят молоко", &["кошк", "люб", "молок"][..]),
            ("Кошка любит молоко", &["кошк", "люб", "молок"]),
            ("играли в шахматы", &["игра", "шахмат"]),
            ("играть в шахматы", &["игра", "шахмат"]),
            ("Мне нравится зелёный чай", &["зелен", "мне", "нрав", "ча"]),
            ("Мне нравится зеленый чай", &["зелен", "мне", "нрав", "ча"]),
            ("a,b hello-world 123", &["123", "hello", "world"]),
            ("и у на", &[]),
        ] {
            let expected = expected.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(tokens(input), expected, "{input}");
        }
    }

    #[test]
    fn python_reference_golden_fixtures_for_stemming_and_tokenization() {
        let fixtures: Vec<(String, Vec<String>)> = serde_json::from_str(include_str!(
            "../../tests/fixtures/memory_lexical_python_20261022.json"
        ))
        .unwrap();
        assert!(fixtures.len() >= 50);
        for (text, expected) in fixtures {
            assert_eq!(tokens(&text), expected.into_iter().collect(), "{text}");
        }
    }

    #[test]
    fn lexical_dedup_updates_same_identity_and_preserves_priority() {
        use super::super::memory::{
            CharacterMemories, MemoryChange, MemoryCommand, MemoryPriority,
        };
        let id = MemoryId(uuid::Uuid::from_u128(42));
        let mut original = CharacterMemories::default();
        original
            .apply_committed(&MemoryChange::Upsert(MemoryRecord::fact(
                id,
                MemoryPriority::High,
                "Кошки любят молоко",
            )))
            .unwrap();
        let commands = [MemoryCommand::AddLexicallyDeduplicated {
            priority: MemoryPriority::Low,
            content: "Кошка любит молоко".into(),
            threshold_per_mille: 800,
        }];
        let changes = original.resolve(&commands).unwrap();
        assert_eq!(changes.len(), 1);
        let MemoryChange::Upsert(updated) = &changes[0] else {
            panic!("expected upsert")
        };
        assert_eq!(updated.id, id);
        assert_eq!(updated.priority, MemoryPriority::High);
        assert_eq!(updated.content, "Кошка любит молоко");
        assert_eq!(original.get(id).unwrap().content, "Кошки любят молоко");
        let repeated = CharacterMemories::default();
        assert!(
            repeated
                .resolve(&[MemoryCommand::AddLexicallyDeduplicated {
                    priority: MemoryPriority::Normal,
                    content: "Новая память".into(),
                    threshold_per_mille: 800,
                }])
                .is_ok()
        );
        let mut merged = original;
        merged.apply_committed(&changes[0]).unwrap();
        assert!(merged.resolve(&commands).unwrap().is_empty());
    }

    #[test]
    fn lexical_dedup_excludes_hidden_and_islands_and_refuses_protected_target() {
        use super::super::memory::{
            CharacterMemories, MemoryChange, MemoryCommand, MemoryIsland, MemoryPriority,
        };
        let hidden = MemoryId(uuid::Uuid::from_u128(41));
        let protected = MemoryId(uuid::Uuid::from_u128(42));
        let mut book = CharacterMemories::default();
        let mut forgotten = MemoryRecord::fact(hidden, MemoryPriority::Low, "играли в шахматы");
        forgotten.state = MemoryState::Forgotten;
        book.apply_committed(&MemoryChange::Upsert(forgotten))
            .unwrap();
        let mut protected_record =
            MemoryRecord::fact(protected, MemoryPriority::Normal, "Кошки любят молоко");
        protected_record.protected = true;
        book.apply_committed(&MemoryChange::Upsert(protected_record))
            .unwrap();
        let no_hidden_match = book
            .resolve(&[MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::Normal,
                content: "играть в шахматы".into(),
                threshold_per_mille: 800,
            }])
            .unwrap();
        let MemoryChange::Upsert(new_fact) = &no_hidden_match[0] else {
            panic!("expected add")
        };
        assert_ne!(new_fact.id, hidden);
        assert_eq!(new_fact.state, MemoryState::Active);
        let rejection = book.resolve(&[
            MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "brand new unrelated".into(),
            },
            MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::High,
                content: "Кошка любит молоко".into(),
                threshold_per_mille: 800,
            },
        ]);
        assert_eq!(
            rejection,
            Err("protected memory cannot be edited before unprotecting")
        );
        assert_eq!(book.all().len(), 2);
        let island = MemoryId(uuid::Uuid::from_u128(43));
        let mut typed = MemoryRecord::fact(island, MemoryPriority::Normal, "люблю зелёный чай");
        typed.kind = MemoryKind::Island(MemoryIsland::Preferences);
        book.apply_committed(&MemoryChange::Upsert(typed)).unwrap();
        let changes = book
            .resolve(&[MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::Normal,
                content: "люблю зеленый чай".into(),
                threshold_per_mille: 800,
            }])
            .unwrap();
        let MemoryChange::Upsert(new_fact) = &changes[0] else {
            panic!("expected add")
        };
        assert_ne!(new_fact.id, island);
    }

    #[test]
    fn lexical_threshold_is_validated_before_domain_write() {
        use super::super::memory::{CharacterMemories, MemoryCommand, MemoryPriority};
        let book = CharacterMemories::default();
        assert_eq!(
            book.resolve(&[MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::Low,
                content: "text content".into(),
                threshold_per_mille: 1001,
            }]),
            Err("lexical dedup threshold must be 500..1000 per mille")
        );
    }

    #[test]
    fn jaccard_boundary_is_inclusive_and_higher_similarity_wins() {
        let low = MemoryId(uuid::Uuid::from_u128(10));
        let high = MemoryId(uuid::Uuid::from_u128(20));
        let records = [
            MemoryRecord::fact(
                low,
                Default::default(),
                "alpha beta gamma delta epsilon zeta",
            ),
            MemoryRecord::fact(high, Default::default(), "alpha beta gamma delta epsilon"),
        ];
        let query = "alpha beta gamma delta epsilon extra";
        assert_eq!(best_active_fact(&records, query, 500), Ok(Some(high)));
        assert_eq!(best_active_fact(&records, query, 834), Ok(None));
        let exact_five = "alpha beta gamma delta epsilon";
        assert_eq!(best_active_fact(&records, exact_five, 1000), Ok(Some(high)));
        assert_eq!(best_active_fact(&records, exact_five, 900), Ok(Some(high)));
        let single = [MemoryRecord::fact(low, Default::default(), exact_five)];
        let four_words = "alpha beta gamma delta";
        assert_eq!(best_active_fact(&single, four_words, 800), Ok(Some(low)));
        assert_eq!(best_active_fact(&single, four_words, 801), Ok(None));
    }

    #[test]
    fn threshold_and_tie_breaking_are_deterministic() {
        let id_a = MemoryId(uuid::Uuid::from_u128(1));
        let id_b = MemoryId(uuid::Uuid::from_u128(2));
        let records = [
            MemoryRecord::fact(id_b, Default::default(), "Кошки любят молоко"),
            MemoryRecord::fact(id_a, Default::default(), "Кошка любит молоко"),
        ];
        assert_eq!(
            best_active_fact(&records, "Кошки любят молоко!", 800),
            Ok(Some(id_a))
        );
        assert_eq!(best_active_fact(&records, "Ничего общего", 800), Ok(None));
        assert_eq!(
            best_active_fact(&records, "Кошки любят молоко", 499),
            Err("lexical dedup threshold must be 500..1000 per mille")
        );
    }
}
