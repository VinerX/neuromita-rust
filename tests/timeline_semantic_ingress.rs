use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{
            AppendGuard, BranchId, EventKind, SessionId, TimelineError, TimelineRepository,
            TimelineStore, present_characters,
        },
        ingress::{SemanticBatch, SemanticEvent, append_semantic_batch},
        view::TimelineMessageQueries,
    },
};
use serde_json::json;

fn event(kind: EventKind, observers: &[CharacterId], name: &str) -> SemanticEvent {
    let mut event = SemanticEvent::new(kind, observers.iter().copied());
    event.payload.insert("name".into(), json!(name));
    event
}

fn append(
    repository: &mut dyn TimelineRepository,
    branch: BranchId,
    batch: SemanticBatch,
) -> Result<Vec<neuromita::timeline::domain::TimelineFact>, TimelineError> {
    let head = repository.branch(branch)?.head_sequence;
    append_semantic_batch(
        repository,
        branch,
        head,
        AppendGuard {
            selection_generation: 1,
            context: None,
            accepted_reply_revision: None,
        },
        batch,
    )
}

fn contract(
    repository: &mut dyn TimelineRepository,
    branch: BranchId,
) -> (
    SemanticBatch,
    Vec<neuromita::timeline::domain::TimelineFact>,
) {
    let mut joined = event(
        EventKind::CharacterJoined,
        &[CharacterId::Crazy, CharacterId::Kind],
        "joined",
    );
    joined.actor = Some(CharacterId::Kind.to_string());
    let door = event(EventKind::UnityEvent, &[CharacterId::Crazy], "DoorOpened");
    let game = event(
        EventKind::GameAction,
        &[CharacterId::Crazy, CharacterId::Kind],
        "ChessMove",
    );
    let batch = SemanticBatch::new("unity:turn-1", vec![joined, door, game]);
    let first = append(repository, branch, batch.clone()).unwrap();
    assert_eq!(first.len(), 3);
    assert_eq!(
        first.iter().map(|f| f.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        present_characters(&repository.world(branch).unwrap()),
        [CharacterId::Kind].into_iter().collect()
    );

    let mita = TimelineMessageQueries::capture(repository, branch, CharacterId::Crazy, None)
        .unwrap()
        .world_events();
    assert_eq!(mita.len(), 3);
    assert_eq!(mita[1].payload["name"], "DoorOpened");
    let kind = TimelineMessageQueries::capture(repository, branch, CharacterId::Kind, None)
        .unwrap()
        .world_events();
    assert_eq!(kind.len(), 2);
    assert_eq!(kind[1].payload["name"], "ChessMove");

    let replay = append(repository, branch, batch.clone()).unwrap();
    assert_eq!(replay, first);
    assert_eq!(repository.branch(branch).unwrap().head_sequence, 3);

    let mut conflicting = batch.clone();
    conflicting.events[1]
        .payload
        .insert("name".into(), json!("DifferentDoor"));
    assert_eq!(
        append(repository, branch, conflicting),
        Err(TimelineError::Conflict)
    );
    assert_eq!(repository.branch(branch).unwrap().head_sequence, 3);

    let mut different_time = batch.clone();
    different_time.events[0].occurred_at_unix_ms += 1;
    assert_eq!(
        append(repository, branch, different_time),
        Err(TimelineError::Conflict)
    );
    assert_eq!(repository.branch(branch).unwrap().head_sequence, 3);

    let valid = event(EventKind::GameEnded, &[CharacterId::Crazy], "ChessWin");
    let invalid = event(
        EventKind::CharacterContextReset,
        &[CharacterId::Crazy],
        "reset",
    );
    assert!(matches!(
        append(
            repository,
            branch,
            SemanticBatch::new("bad-batch", vec![valid, invalid])
        ),
        Err(TimelineError::Invalid(_))
    ));
    assert_eq!(repository.branch(branch).unwrap().head_sequence, 3);
    assert!(matches!(
        append(repository, branch, SemanticBatch::new("empty", vec![])),
        Err(TimelineError::Invalid(_))
    ));

    let invisible = event(EventKind::UnityEvent, &[], "NotObserved");
    append(
        repository,
        branch,
        SemanticBatch::new("hidden", vec![invisible]),
    )
    .unwrap();
    assert_eq!(repository.branch(branch).unwrap().head_sequence, 4);
    assert_eq!(
        TimelineMessageQueries::capture(repository, branch, CharacterId::Crazy, None)
            .unwrap()
            .world_events()
            .len(),
        3
    );
    (batch, first)
}

#[test]
fn semantic_ingress_is_atomic_and_idempotent_in_memory() {
    let mut repository = TimelineStore::new();
    let branch = BranchId::new();
    repository.create_session(SessionId::new(), branch).unwrap();
    contract(&mut repository, branch);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn semantic_ingress_is_atomic_and_idempotent_in_sqlite() {
    use neuromita::timeline::{
        selection::TimelineSelectionRepository, storage::SqliteTimelineRepository,
    };
    let path = std::env::temp_dir().join(format!("semantic-ingress-{}.db", uuid::Uuid::new_v4()));
    let branch = BranchId::new();
    let (batch, first);
    {
        let mut repository = SqliteTimelineRepository::open(&path).unwrap();
        repository.create_session(SessionId::new(), branch).unwrap();
        repository.select_branch(branch, None).unwrap();
        (batch, first) = contract(&mut repository, branch);
    }
    {
        let mut reopened = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(append(&mut reopened, branch, batch).unwrap(), first);
        assert_eq!(reopened.branch(branch).unwrap().head_sequence, 4);
        assert_eq!(
            TimelineMessageQueries::capture(&reopened, branch, CharacterId::Kind, None)
                .unwrap()
                .world_events()
                .len(),
            2
        );
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_publishes_world_facts_without_creating_generation_requests() {
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    use neuromita::{
        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        generation::{
            GenerationPreset, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
        },
    };

    let template = GeminiTemplate::google(
        "unused-model",
        "unused-key",
        GenerationSettings::new(0.0, 64, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(template));
    let config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([Arc::new(GeminiProvider::new().unwrap()) as Arc<_>]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(4).unwrap(),
            character_commands: NonZeroUsize::new(4).unwrap(),
            presenter_updates: NonZeroUsize::new(4).unwrap(),
            retained_finished: NonZeroUsize::new(4).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let selection = app.timeline_selection();
    let door = event(EventKind::UnityEvent, &[CharacterId::Crazy], "DoorOpened");
    let first = app
        .publish_semantic_events(
            selection,
            SemanticBatch::new("unity-door", vec![door.clone()]),
        )
        .unwrap();
    assert_eq!(app.active_requests(), 0);
    assert_eq!(first.len(), 1);
    assert_eq!(app.at().through, 1);
    let mita = app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(mita.view.world_events()[0].payload["name"], "DoorOpened");
    let kind = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert!(kind.view.world_events().is_empty());
    assert_eq!(
        app.publish_semantic_events(selection, SemanticBatch::new("unity-door", vec![door]))
            .unwrap(),
        first
    );
    assert_eq!(app.at().through, 1);
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_semantic_ingress_rejects_a_reselected_branch_before_commit() {
    use neuromita::timeline::{
        selection::TimelineSelectionRepository, storage::SqliteTimelineRepository,
    };

    let path = std::env::temp_dir().join(format!("semantic-selection-{}.db", uuid::Uuid::new_v4()));
    let branch = BranchId::new();
    let mut first = SqliteTimelineRepository::open(&path).unwrap();
    first.create_session(SessionId::new(), branch).unwrap();
    let selected = first.select_branch(branch, None).unwrap();
    let mut external = SqliteTimelineRepository::open(&path).unwrap();
    let newer = external.select_branch(branch, Some(selected)).unwrap();
    let batch = SemanticBatch::new(
        "unity-old-turn",
        vec![event(
            EventKind::UnityEvent,
            &[CharacterId::Crazy],
            "OpenDoor",
        )],
    );
    assert_eq!(
        append_semantic_batch(
            &mut first,
            branch,
            0,
            AppendGuard {
                selection_generation: selected.generation,
                context: None,
                accepted_reply_revision: None,
            },
            batch.clone(),
        ),
        Err(TimelineError::ConcurrentWrite)
    );
    assert_eq!(first.branch(branch).unwrap().head_sequence, 0);
    let accepted = append_semantic_batch(
        &mut first,
        branch,
        0,
        AppendGuard {
            selection_generation: newer.generation,
            context: None,
            accepted_reply_revision: None,
        },
        batch,
    )
    .unwrap();
    assert_eq!(accepted.len(), 1);
    drop(first);
    drop(external);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
