use std::collections::BTreeMap;

use neuromita::{
    character::CharacterId,
    knowledge::retrieval_fusion::{
        FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit, RetrievalKey,
        RetrievalMethod, RetrievalSource, fuse,
    },
};

fn hit(source: RetrievalSource, id: &str, content: &str) -> RetrievalHit {
    RetrievalHit {
        key: RetrievalKey {
            character: CharacterId::Crazy,
            source,
            id: id.into(),
        },
        content: content.into(),
        raw_score: None,
        metadata: BTreeMap::new(),
    }
}

fn bucket(method: RetrievalMethod, hits: Vec<RetrievalHit>) -> RetrievalBucket {
    RetrievalBucket { method, hits }
}

fn config(mode: FusionMode, limit: usize) -> FusionConfig {
    FusionConfig {
        mode,
        limit,
        minimum_graph: 0,
    }
}

fn ids(hits: &[neuromita::knowledge::retrieval_fusion::FusedHit]) -> Vec<String> {
    hits.iter().map(|h| h.key.id.clone()).collect()
}

#[test]
fn source_identity_prevents_cross_source_collisions() {
    let buckets = [
        bucket(
            RetrievalMethod::Vector,
            vec![hit(RetrievalSource::Memory, "7", "memory record")],
        ),
        bucket(
            RetrievalMethod::Keyword,
            vec![hit(RetrievalSource::History, "7", "history event")],
        ),
    ];
    let hits = fuse(CharacterId::Crazy, &buckets, config(FusionMode::Union, 5)).unwrap();
    assert_eq!(hits.len(), 2);
    assert_ne!(hits[0].key.source, hits[1].key.source);
    assert_eq!(hits[0].key.id, hits[1].key.id);
}

#[test]
fn union_merges_features_without_overwriting_content_or_metadata() {
    let mut vector = hit(RetrievalSource::Memory, "1", "canonical fact");
    vector.raw_score = Some(0.9);
    vector.metadata.insert("revision".into(), "v1".into());
    let mut lexical = vector.clone();
    lexical.raw_score = Some(2.0);
    lexical.metadata.insert("source".into(), "timeline".into());
    let buckets = [
        bucket(RetrievalMethod::Vector, vec![vector]),
        bucket(RetrievalMethod::Keyword, vec![lexical]),
    ];
    let hits = fuse(CharacterId::Crazy, &buckets, config(FusionMode::Union, 4)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].score, 2.0);
    assert_eq!(hits[0].evidence.len(), 2);
    assert_eq!(hits[0].evidence[0].raw_score, Some(0.9));
    assert_eq!(hits[0].metadata["source"], "timeline");
    assert_eq!(hits[0].metadata["revision"], "v1");
}

#[test]
fn intersect_requires_distinct_methods_and_optionally_vector() {
    let buckets = [
        bucket(
            RetrievalMethod::Vector,
            vec![hit(RetrievalSource::Memory, "1", "one")],
        ),
        bucket(
            RetrievalMethod::Keyword,
            vec![
                hit(RetrievalSource::Memory, "1", "one"),
                hit(RetrievalSource::History, "2", "two"),
            ],
        ),
        bucket(
            RetrievalMethod::Fts,
            vec![hit(RetrievalSource::History, "2", "two")],
        ),
    ];
    let mode = FusionMode::Intersect {
        min_methods: 2,
        require_vector: true,
    };
    assert_eq!(
        ids(&fuse(CharacterId::Crazy, &buckets, config(mode, 5)).unwrap()),
        ["1"]
    );
    let mode = FusionMode::Intersect {
        min_methods: 2,
        require_vector: false,
    };
    assert_eq!(
        fuse(CharacterId::Crazy, &buckets, config(mode, 5))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn two_stage_never_admits_lexical_only_hits_or_silently_falls_back() {
    let buckets = [
        bucket(
            RetrievalMethod::Vector,
            vec![hit(RetrievalSource::Memory, "1", "one")],
        ),
        bucket(
            RetrievalMethod::Keyword,
            vec![
                hit(RetrievalSource::Memory, "1", "one"),
                hit(RetrievalSource::Memory, "2", "two"),
            ],
        ),
    ];
    let hits = fuse(
        CharacterId::Crazy,
        &buckets,
        config(FusionMode::TwoStage, 5),
    )
    .unwrap();
    assert_eq!(ids(&hits), ["1"]);
    assert_eq!(hits[0].evidence.len(), 2);
    let empty_vector = [bucket(RetrievalMethod::Vector, vec![]), buckets[1].clone()];
    assert!(
        fuse(
            CharacterId::Crazy,
            &empty_vector,
            config(FusionMode::TwoStage, 5)
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        fuse(
            CharacterId::Crazy,
            &empty_vector,
            config(FusionMode::Union, 5)
        )
        .unwrap()
        .len()
            == 2
    );
}

#[test]
fn rrf_deterministically_reranks_with_method_evidence() {
    let buckets = [
        bucket(
            RetrievalMethod::Vector,
            vec![
                hit(RetrievalSource::Memory, "1", "one"),
                hit(RetrievalSource::Memory, "2", "two"),
                hit(RetrievalSource::Memory, "3", "three"),
            ],
        ),
        bucket(
            RetrievalMethod::Keyword,
            vec![
                hit(RetrievalSource::Memory, "2", "two"),
                hit(RetrievalSource::Memory, "3", "three"),
            ],
        ),
        bucket(
            RetrievalMethod::Fts,
            vec![hit(RetrievalSource::Memory, "2", "two")],
        ),
    ];
    let expected = ["2", "3", "1"];
    let hits = fuse(
        CharacterId::Crazy,
        &buckets,
        config(FusionMode::Rrf { k: 60 }, 3),
    )
    .unwrap();
    assert_eq!(ids(&hits), expected);
    let reversed = buckets.into_iter().rev().collect::<Vec<_>>();
    let repeated = fuse(
        CharacterId::Crazy,
        &reversed,
        config(FusionMode::Rrf { k: 60 }, 3),
    )
    .unwrap();
    assert_eq!(hits, repeated);
}

#[test]
fn available_graph_quota_keeps_existing_graph_hit_only() {
    let buckets = [bucket(
        RetrievalMethod::Vector,
        vec![
            hit(RetrievalSource::Memory, "1", "one"),
            hit(RetrievalSource::Memory, "2", "two"),
            hit(RetrievalSource::Graph, "3", "three"),
        ],
    )];
    let mut cfg = config(FusionMode::Union, 2);
    cfg.minimum_graph = 1;
    let result = fuse(CharacterId::Crazy, &buckets, cfg).unwrap();
    assert_eq!(ids(&result), ["1", "3"]);
    assert_eq!(result.len(), 2);
    cfg.minimum_graph = 2;
    assert_eq!(fuse(CharacterId::Crazy, &buckets, cfg).unwrap().len(), 2);
}

#[test]
fn rejects_cross_character_duplicates_invalid_scores_and_conflicting_snapshots() {
    let mut bad = hit(RetrievalSource::Memory, "1", "one");
    bad.key.character = CharacterId::Kind;
    assert!(matches!(
        fuse(
            CharacterId::Crazy,
            &[bucket(RetrievalMethod::Vector, vec![bad])],
            config(FusionMode::Union, 2)
        ),
        Err(FusionError::Evidence(_))
    ));
    let mut bad = hit(RetrievalSource::Memory, "1", "one");
    bad.raw_score = Some(f64::NAN);
    assert!(
        fuse(
            CharacterId::Crazy,
            &[bucket(RetrievalMethod::Vector, vec![bad])],
            config(FusionMode::Union, 2)
        )
        .is_err()
    );
    let buckets = [
        bucket(
            RetrievalMethod::Vector,
            vec![hit(RetrievalSource::Memory, "1", "old")],
        ),
        bucket(
            RetrievalMethod::Keyword,
            vec![hit(RetrievalSource::Memory, "1", "new")],
        ),
    ];
    assert!(fuse(CharacterId::Crazy, &buckets, config(FusionMode::Union, 2)).is_err());
    let duplicate = bucket(
        RetrievalMethod::Vector,
        vec![
            hit(RetrievalSource::Memory, "1", "one"),
            hit(RetrievalSource::Memory, "1", "one"),
        ],
    );
    assert!(
        fuse(
            CharacterId::Crazy,
            &[duplicate],
            config(FusionMode::Union, 2)
        )
        .is_err()
    );
}

#[test]
fn validates_limits_and_rrf_parameters_without_side_effects() {
    assert!(fuse(CharacterId::Crazy, &[], config(FusionMode::Union, 0)).is_err());
    assert!(fuse(CharacterId::Crazy, &[], config(FusionMode::Rrf { k: 0 }, 1)).is_err());
    let mut cfg = config(FusionMode::Union, 1);
    cfg.minimum_graph = 2;
    assert!(fuse(CharacterId::Crazy, &[], cfg).is_err());
    cfg = config(
        FusionMode::Intersect {
            min_methods: 0,
            require_vector: false,
        },
        2,
    );
    assert!(fuse(CharacterId::Crazy, &[], cfg).is_err());
}
