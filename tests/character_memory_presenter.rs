use std::{num::NonZeroUsize, time::Duration};

use neuromita::{
    api::CharacterApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterError, CharacterId},
    generation::{
        GenerationPreset, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    memory::{MemoryCommand, MemoryIsland, MemoryKind, MemoryPriority, MemoryState},
    presentation::MemoryListQuery,
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(4).unwrap(),
        discovery_buffer: NonZeroUsize::new(16).unwrap(),
        character_commands: NonZeroUsize::new(16).unwrap(),
        presenter_updates: NonZeroUsize::new(16).unwrap(),
        retained_finished: NonZeroUsize::new(4).unwrap(),
    }
}

async fn start() -> ApplicationLifecycle {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(
            "http://127.0.0.1:1/v1beta/",
            "test-model",
            "unused-key",
            GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new(Vec::new()).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap()
}

#[tokio::test]
async fn presenter_memory_projection_filters_pages_and_keeps_edit_token() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let before = presenter.memories(CharacterId::Crazy).await.unwrap();
    let after = presenter
        .edit_memories(
            CharacterId::Crazy,
            before.clone(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "Мита любит чай".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Мита любит кофе".into(),
                },
                MemoryCommand::SetIsland {
                    island: MemoryIsland::Preferences,
                    content: "Предпочитает тихую музыку".into(),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(after.snapshot.records().len(), 3);
    assert_eq!(after.applied.len(), 3);
    assert!(matches!(
        presenter
            .edit_memories(
                CharacterId::Crazy,
                before,
                vec![MemoryCommand::Forget {
                    id: after.snapshot.records()[0].id
                }]
            )
            .await,
        Err(CharacterError::StaleState)
    ));

    let page = presenter
        .list_memories(
            CharacterId::Crazy,
            MemoryListQuery {
                text: Some("МИТА".into()),
                kind: Some(MemoryKind::Fact),
                limit: 1,
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.total_matches(), 2);
    assert_eq!(page.records().len(), 1);
    assert_eq!(page.next_offset(), Some(1));
    assert_eq!(page.summary().active, 3);
    assert_eq!(page.summary().facts, 2);
    assert_eq!(page.summary().islands, 1);
    assert_eq!(page.snapshot().records().len(), 3);

    let second = presenter
        .list_memories(
            CharacterId::Crazy,
            MemoryListQuery {
                text: Some("мита".into()),
                kind: Some(MemoryKind::Fact),
                offset: page.next_offset().unwrap(),
                limit: 1,
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.records().len(), 1);
    assert_ne!(page.records()[0].id, second.records()[0].id);
    assert_eq!(second.next_offset(), None);

    let target = page.records()[0].id;
    let retained_page = page.clone();
    let edited = presenter
        .edit_memories(
            CharacterId::Crazy,
            page.into_snapshot(),
            vec![MemoryCommand::Forget { id: target }],
        )
        .await
        .unwrap();
    assert!(
        edited
            .snapshot
            .records()
            .iter()
            .any(|r| r.id == target && r.state == MemoryState::Forgotten)
    );
    let pinned = neuromita::presentation::MemoryListPage::from_snapshot(
        retained_page.snapshot().clone(),
        MemoryListQuery {
            state: Some(MemoryState::Forgotten),
            ..MemoryListQuery::default()
        },
    )
    .unwrap();
    assert_eq!(pinned.total_matches(), 0);
    assert_eq!(pinned.summary().active, 3);
    assert_eq!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .memories()
            .await
            .unwrap()
            .records(),
        edited.snapshot.records()
    );
    let forgotten = presenter
        .list_memories(
            CharacterId::Crazy,
            MemoryListQuery {
                state: Some(MemoryState::Forgotten),
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(forgotten.total_matches(), 1);
    assert_eq!(forgotten.summary().forgotten, 1);
    assert!(
        presenter
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn memory_presenter_rejects_invalid_queries_without_mutating_character() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    for query in [
        MemoryListQuery {
            limit: 0,
            ..MemoryListQuery::default()
        },
        MemoryListQuery {
            limit: 101,
            ..MemoryListQuery::default()
        },
        MemoryListQuery {
            text: Some("x".repeat(201)),
            ..MemoryListQuery::default()
        },
    ] {
        assert!(matches!(
            presenter.list_memories(CharacterId::Crazy, query).await,
            Err(CharacterError::InvalidMemory(_))
        ));
    }
    let empty = presenter
        .list_memories(
            CharacterId::Crazy,
            MemoryListQuery {
                offset: usize::MAX,
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(empty.records().is_empty());
    assert_eq!(empty.next_offset(), None);
    assert_eq!(empty.total_matches(), 0);
    assert!(
        presenter
            .memories(CharacterId::Crazy)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn memory_observer_refreshes_only_on_committed_character_memory_changes() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let mut mita = presenter
        .observe_memories(CharacterId::Crazy, MemoryListQuery::default())
        .unwrap();
    let mut other = presenter
        .observe_memories(CharacterId::Kind, MemoryListQuery::default())
        .unwrap();
    assert_eq!(mita.next().await.unwrap().total_matches(), 0);
    assert_eq!(other.next().await.unwrap().total_matches(), 0);

    let before = presenter.memories(CharacterId::Crazy).await.unwrap();
    let committed = presenter
        .edit_memories(
            CharacterId::Crazy,
            before.clone(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "Remember the winter festival".into(),
            }],
        )
        .await
        .unwrap();
    let refreshed = tokio::time::timeout(Duration::from_secs(2), mita.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.total_matches(), 1);
    assert_eq!(
        refreshed.records()[0].content,
        "Remember the winter festival"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), other.next())
            .await
            .is_err()
    );

    let stale = presenter
        .edit_memories(
            CharacterId::Crazy,
            before,
            vec![MemoryCommand::Forget {
                id: committed.snapshot.records()[0].id,
            }],
        )
        .await;
    assert!(matches!(stale, Err(CharacterError::StaleState)));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), mita.next())
            .await
            .is_err()
    );

    let changed_other = presenter
        .edit_memories(
            CharacterId::Kind,
            presenter.memories(CharacterId::Kind).await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Only another character knows".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(changed_other.snapshot.records().len(), 1);
    assert_eq!(other.next().await.unwrap().total_matches(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), mita.next())
            .await
            .is_err()
    );

    app.shutdown().await.unwrap();
    assert!(matches!(
        mita.next().await,
        Err(CharacterError::Unavailable)
    ));
}

#[tokio::test]
async fn capacity_preview_requires_confirmation_and_rejects_stale_or_cross_character_apply() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let initial = presenter.memories(CharacterId::Crazy).await.unwrap();
    let added = presenter
        .edit_memories(
            CharacterId::Crazy,
            initial,
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::Low,
                    content: "least important".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "normal fact".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Critical,
                    content: "critical fact".into(),
                },
                MemoryCommand::SetIsland {
                    island: MemoryIsland::Relationship,
                    content: "a durable relationship".into(),
                },
            ],
        )
        .await
        .unwrap();
    let low = added
        .snapshot
        .records()
        .iter()
        .find(|record| record.priority == MemoryPriority::Low)
        .unwrap()
        .id;
    let preview = presenter
        .preview_memory_capacity(CharacterId::Crazy, 2)
        .await
        .unwrap();
    assert_eq!(preview.plan().active_facts(), 3);
    assert_eq!(preview.plan().victims(), &[low]);
    assert_eq!(
        presenter
            .memories(CharacterId::Crazy)
            .await
            .unwrap()
            .records()
            .len(),
        4
    );
    assert!(matches!(
        presenter
            .apply_memory_capacity(CharacterId::Kind, preview.clone())
            .await,
        Err(CharacterError::StaleState)
    ));

    // A different character's edit does not invalidate Mita's preview.
    let other = presenter.memories(CharacterId::Kind).await.unwrap();
    presenter
        .edit_memories(
            CharacterId::Kind,
            other,
            vec![MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "other character's fact".into(),
            }],
        )
        .await
        .unwrap();
    let committed = presenter
        .apply_memory_capacity(CharacterId::Crazy, preview)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(committed.applied.len(), 1);
    assert_eq!(
        committed
            .snapshot
            .records()
            .iter()
            .find(|record| record.id == low)
            .unwrap()
            .state,
        MemoryState::Forgotten
    );
    assert_eq!(
        presenter
            .preview_memory_capacity(CharacterId::Crazy, 2)
            .await
            .unwrap()
            .plan()
            .victims()
            .len(),
        0
    );
    let noop = presenter
        .preview_memory_capacity(CharacterId::Crazy, 2)
        .await
        .unwrap();
    assert!(
        presenter
            .apply_memory_capacity(CharacterId::Crazy, noop)
            .await
            .unwrap()
            .is_none()
    );

    let next = presenter
        .preview_memory_capacity(CharacterId::Crazy, 1)
        .await
        .unwrap();
    assert_eq!(next.plan().victims().len(), 1);
    assert_ne!(next.plan().victims(), &[low]);
    assert_eq!(
        presenter
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .len(),
        1
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn capacity_plan_stale_write_is_atomic_and_cannot_touch_protected_memory() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let added = presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::Low,
                    content: "low fact".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "normal fact".into(),
                },
            ],
        )
        .await
        .unwrap();
    let preview = presenter
        .preview_memory_capacity(CharacterId::Crazy, 1)
        .await
        .unwrap();
    assert_eq!(preview.plan().victims().len(), 1);
    let low = added
        .snapshot
        .records()
        .iter()
        .find(|record| record.priority == MemoryPriority::Low)
        .unwrap()
        .id;
    presenter
        .edit_memories(
            CharacterId::Crazy,
            added.snapshot,
            vec![MemoryCommand::SetProtection {
                id: low,
                protected: true,
            }],
        )
        .await
        .unwrap();
    assert!(matches!(
        presenter
            .apply_memory_capacity(CharacterId::Crazy, preview)
            .await,
        Err(CharacterError::StaleState)
    ));
    assert_eq!(
        presenter
            .memories(CharacterId::Crazy)
            .await
            .unwrap()
            .records()
            .iter()
            .filter(|record| record.state == MemoryState::Active)
            .count(),
        2
    );
    let new_plan = presenter
        .preview_memory_capacity(CharacterId::Crazy, 1)
        .await
        .unwrap();
    assert_ne!(new_plan.plan().victims(), &[low]);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn capacity_preview_can_commit_a_full_255_fact_reduction_atomically() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    for batch in 0..2 {
        let commands = (0..128)
            .map(|n| MemoryCommand::Add {
                priority: MemoryPriority::Low,
                content: format!("capacity fact {}", batch * 128 + n),
            })
            .collect();
        presenter
            .edit_memories(
                CharacterId::Crazy,
                presenter.memories(CharacterId::Crazy).await.unwrap(),
                commands,
            )
            .await
            .unwrap();
    }
    let preview = presenter
        .preview_memory_capacity(CharacterId::Crazy, 1)
        .await
        .unwrap();
    assert_eq!(preview.plan().active_facts(), 256);
    assert_eq!(preview.plan().victims().len(), 255);
    let result = presenter
        .apply_memory_capacity(CharacterId::Crazy, preview)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.applied.len(), 255);
    assert_eq!(
        result
            .snapshot
            .records()
            .iter()
            .filter(|r| r.state == MemoryState::Active)
            .count(),
        1
    );
    assert_eq!(
        result
            .snapshot
            .records()
            .iter()
            .filter(|r| r.state == MemoryState::Forgotten)
            .count(),
        255
    );
    assert_eq!(result.snapshot.records().len(), 256);
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn capacity_forgetting_is_durable_after_sqlite_reopen() {
    let _serial = SERIAL.lock().await;
    let path = std::env::temp_dir().join(format!("neuromita-capacity-{}", uuid::Uuid::new_v4()));
    let launch = || {
        let preset = GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::new(
                "http://127.0.0.1:1/v1beta/",
                "test-model",
                "unused-key",
                GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
            )
            .unwrap(),
        ));
        ApplicationConfig::persistent(
            preset,
            ResponseGenerator::new(Vec::new()).unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &path,
        )
        .unwrap()
    };
    let app = ApplicationLifecycle::start(launch()).await.unwrap();
    let presenter = &app.presenter().characters;
    presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::Low,
                    content: "durable low fact".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "durable high fact".into(),
                },
            ],
        )
        .await
        .unwrap();
    let preview = presenter
        .preview_memory_capacity(CharacterId::Crazy, 1)
        .await
        .unwrap();
    let forgotten_id = preview.plan().victims()[0];
    presenter
        .apply_memory_capacity(CharacterId::Crazy, preview)
        .await
        .unwrap();
    app.shutdown().await.unwrap();

    let reopened = ApplicationLifecycle::start(launch()).await.unwrap();
    let records = reopened
        .presenter()
        .characters
        .memories(CharacterId::Crazy)
        .await
        .unwrap();
    assert_eq!(records.records().len(), 2);
    assert_eq!(
        records
            .records()
            .iter()
            .find(|record| record.id == forgotten_id)
            .unwrap()
            .state,
        MemoryState::Forgotten
    );
    assert_eq!(
        records
            .records()
            .iter()
            .filter(|r| r.state == MemoryState::Active)
            .count(),
        1
    );
    assert!(
        reopened
            .presenter()
            .characters
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    reopened.shutdown().await.unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn ensure_fact_is_explicit_idempotent_presenter_edit_with_character_isolation() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let initial = presenter.memories(CharacterId::Crazy).await.unwrap();
    let first = presenter
        .edit_memories(
            CharacterId::Crazy,
            initial.clone(),
            vec![MemoryCommand::EnsureFact {
                priority: MemoryPriority::Low,
                content: "persistent unique wording".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(first.applied.len(), 1);
    let id = first.snapshot.records()[0].id;
    assert!(matches!(
        presenter
            .edit_memories(
                CharacterId::Crazy,
                initial,
                vec![MemoryCommand::EnsureFact {
                    priority: MemoryPriority::Low,
                    content: "persistent unique wording".into(),
                }],
            )
            .await,
        Err(CharacterError::StaleState)
    ));
    let repeated = presenter
        .edit_memories(
            CharacterId::Crazy,
            first.snapshot,
            vec![MemoryCommand::EnsureFact {
                priority: MemoryPriority::Critical,
                content: "persistent unique wording".into(),
            }],
        )
        .await
        .unwrap();
    assert!(repeated.applied.is_empty());
    assert_eq!(repeated.snapshot.records()[0].priority, MemoryPriority::Low);
    let forgotten = presenter
        .edit_memories(
            CharacterId::Crazy,
            repeated.snapshot,
            vec![MemoryCommand::Forget { id }],
        )
        .await
        .unwrap();
    let reactivated = presenter
        .edit_memories(
            CharacterId::Crazy,
            forgotten.snapshot,
            vec![MemoryCommand::EnsureFact {
                priority: MemoryPriority::High,
                content: "persistent unique wording".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(reactivated.applied.len(), 1);
    assert_eq!(reactivated.snapshot.records()[0].id, id);
    assert_eq!(reactivated.snapshot.records()[0].state, MemoryState::Active);
    assert_eq!(
        reactivated.snapshot.records()[0].priority,
        MemoryPriority::High
    );
    assert!(
        presenter
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn ensure_fact_reactivates_deleted_identity_across_sqlite_reopen() {
    let _serial = SERIAL.lock().await;
    let path = std::env::temp_dir().join(format!("neuromita-ensure-fact-{}", uuid::Uuid::new_v4()));
    let launch = || {
        let preset = GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::new(
                "http://127.0.0.1:1/v1beta/",
                "test-model",
                "unused-key",
                GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
            )
            .unwrap(),
        ));
        ApplicationConfig::persistent(
            preset,
            ResponseGenerator::new(Vec::new()).unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &path,
        )
        .unwrap()
    };
    let app = ApplicationLifecycle::start(launch()).await.unwrap();
    let presenter = &app.presenter().characters;
    let first = presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![MemoryCommand::EnsureFact {
                priority: MemoryPriority::Low,
                content: "survives restarts".into(),
            }],
        )
        .await
        .unwrap();
    let id = first.snapshot.records()[0].id;
    presenter
        .edit_memories(
            CharacterId::Crazy,
            first.snapshot,
            vec![MemoryCommand::SoftDelete { id }],
        )
        .await
        .unwrap();
    app.shutdown().await.unwrap();

    let reopened = ApplicationLifecycle::start(launch()).await.unwrap();
    let presenter = &reopened.presenter().characters;
    let restored = presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![MemoryCommand::EnsureFact {
                priority: MemoryPriority::High,
                content: "survives restarts".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(restored.snapshot.records().len(), 1);
    assert_eq!(restored.snapshot.records()[0].id, id);
    assert_eq!(restored.snapshot.records()[0].state, MemoryState::Active);
    assert_eq!(
        restored.snapshot.records()[0].priority,
        MemoryPriority::High
    );
    reopened.shutdown().await.unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn seed_island_is_one_time_per_character_via_presenter() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let mut observer = presenter
        .observe_memories(CharacterId::Crazy, MemoryListQuery::default())
        .unwrap();
    assert_eq!(observer.next().await.unwrap().total_matches(), 0);
    let first = presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![MemoryCommand::SeedIsland {
                island: MemoryIsland::Relationship,
                content: "first story".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(first.applied.len(), 1);
    let id = first.snapshot.records()[0].id;
    assert_eq!(first.snapshot.records()[0].priority, MemoryPriority::High);
    assert_eq!(observer.next().await.unwrap().total_matches(), 1);
    let duplicate = presenter
        .edit_memories(
            CharacterId::Crazy,
            first.snapshot,
            vec![MemoryCommand::SeedIsland {
                island: MemoryIsland::Relationship,
                content: "would overwrite".into(),
            }],
        )
        .await
        .unwrap();
    assert!(duplicate.applied.is_empty());
    assert_eq!(duplicate.snapshot.records()[0].content, "first story");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observer.next())
            .await
            .is_err()
    );
    let forgotten = presenter
        .edit_memories(
            CharacterId::Crazy,
            duplicate.snapshot,
            vec![MemoryCommand::Forget { id }],
        )
        .await
        .unwrap();
    assert_eq!(observer.next().await.unwrap().summary().forgotten, 1);
    let repeated = presenter
        .edit_memories(
            CharacterId::Crazy,
            forgotten.snapshot,
            vec![MemoryCommand::SeedIsland {
                island: MemoryIsland::Relationship,
                content: "do not restore".into(),
            }],
        )
        .await
        .unwrap();
    assert!(repeated.applied.is_empty());
    assert_eq!(repeated.snapshot.records()[0].state, MemoryState::Forgotten);
    assert_eq!(repeated.snapshot.records()[0].id, id);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observer.next())
            .await
            .is_err()
    );
    let other = presenter
        .edit_memories(
            CharacterId::Kind,
            presenter.memories(CharacterId::Kind).await.unwrap(),
            vec![MemoryCommand::SeedIsland {
                island: MemoryIsland::Relationship,
                content: "other character's story".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(other.snapshot.records().len(), 1);
    assert_ne!(other.snapshot.records()[0].id, id);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observer.next())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn lexical_dedup_via_presenter_keeps_character_identity_and_observer_semantics() {
    let _serial = SERIAL.lock().await;
    let app = start().await;
    let presenter = &app.presenter().characters;
    let mut observer = presenter
        .observe_memories(CharacterId::Crazy, MemoryListQuery::default())
        .unwrap();
    assert_eq!(observer.next().await.unwrap().total_matches(), 0);

    let empty = presenter.memories(CharacterId::Crazy).await.unwrap();
    let first = presenter
        .edit_memories(
            CharacterId::Crazy,
            empty.clone(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Кошки любят молоко".into(),
            }],
        )
        .await
        .unwrap();
    let id = first.snapshot.records()[0].id;
    assert_eq!(observer.next().await.unwrap().total_matches(), 1);

    let updated = presenter
        .edit_memories(
            CharacterId::Crazy,
            first.snapshot,
            vec![MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::Low,
                content: "Кошка любит молоко".into(),
                threshold_per_mille: 800,
            }],
        )
        .await
        .unwrap();
    assert_eq!(updated.applied.len(), 1);
    assert_eq!(updated.snapshot.records().len(), 1);
    assert_eq!(updated.snapshot.records()[0].id, id);
    assert_eq!(updated.snapshot.records()[0].priority, MemoryPriority::High);
    assert_eq!(
        observer.next().await.unwrap().records()[0].content,
        "Кошка любит молоко"
    );
    assert!(matches!(
        presenter
            .edit_memories(
                CharacterId::Crazy,
                empty,
                vec![MemoryCommand::AddLexicallyDeduplicated {
                    priority: MemoryPriority::Normal,
                    content: "Кошки любят молоко".into(),
                    threshold_per_mille: 800,
                }],
            )
            .await,
        Err(CharacterError::StaleState)
    ));
    let repeated = presenter
        .edit_memories(
            CharacterId::Crazy,
            updated.snapshot,
            vec![MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::Low,
                content: "Кошка любит молоко".into(),
                threshold_per_mille: 800,
            }],
        )
        .await
        .unwrap();
    assert!(repeated.applied.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observer.next())
            .await
            .is_err()
    );
    assert!(
        presenter
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn lexical_dedup_preserves_identity_across_persistent_timeline_reopen() {
    let _serial = SERIAL.lock().await;
    let path =
        std::env::temp_dir().join(format!("neuromita-lexical-dedup-{}", uuid::Uuid::new_v4()));
    let launch = || {
        let preset = GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::new(
                "http://127.0.0.1:1/v1beta/",
                "test-model",
                "unused-key",
                GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
            )
            .unwrap(),
        ));
        ApplicationConfig::persistent(
            preset,
            ResponseGenerator::new(Vec::new()).unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &path,
        )
        .unwrap()
    };
    let app = ApplicationLifecycle::start(launch()).await.unwrap();
    let presenter = &app.presenter().characters;
    let first = presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "играли в шахматы".into(),
            }],
        )
        .await
        .unwrap();
    let id = first.snapshot.records()[0].id;
    let updated = presenter
        .edit_memories(
            CharacterId::Crazy,
            first.snapshot,
            vec![MemoryCommand::AddLexicallyDeduplicated {
                priority: MemoryPriority::High,
                content: "играть в шахматы".into(),
                threshold_per_mille: 800,
            }],
        )
        .await
        .unwrap();
    assert_eq!(updated.snapshot.records().len(), 1);
    assert_eq!(updated.snapshot.records()[0].id, id);
    app.shutdown().await.unwrap();

    let reopened = ApplicationLifecycle::start(launch()).await.unwrap();
    let stored = reopened
        .presenter()
        .characters
        .memories(CharacterId::Crazy)
        .await
        .unwrap();
    assert_eq!(stored.records().len(), 1);
    assert_eq!(stored.records()[0].id, id);
    assert_eq!(stored.records()[0].content, "играть в шахматы");
    assert_eq!(stored.records()[0].priority, MemoryPriority::High);
    assert!(
        reopened
            .presenter()
            .characters
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    reopened.shutdown().await.unwrap();
    std::fs::remove_dir_all(path).unwrap();
}
