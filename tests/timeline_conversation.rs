use neuromita::{
    character::CharacterId,
    timeline::{
        conversation::{ConversationMessage, ConversationSpeaker, append_conversation_message},
        domain::{
            AppendGuard, BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId,
            TimelineError, TimelineFact, TimelineRepository, TimelineStore,
        },
        view::TimelineMessageQueries,
    },
};
use serde_json::json;

fn publish(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    msg: ConversationMessage,
) -> Result<TimelineFact, TimelineError> {
    let head = repo.branch(branch)?.head_sequence;
    append_conversation_message(
        repo,
        branch,
        head,
        AppendGuard {
            selection_generation: 1,
            context: None,
            accepted_reply_revision: None,
        },
        msg,
    )
}

fn contract(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
) -> (ConversationMessage, TimelineFact) {
    let mut incoming = ConversationMessage::new(
        "dialogue:player:1",
        ConversationSpeaker::Player,
        json!([{"type":"text", "text":"Private question"}]),
        [CharacterId::Crazy],
    );
    incoming
        .audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Crazy));
    incoming.audience.participants.extend([
        ConversationSpeaker::Player,
        ConversationSpeaker::Character(CharacterId::Crazy),
        ConversationSpeaker::Character(CharacterId::Kind),
    ]);
    let first = publish(repo, branch, incoming.clone()).unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(first.draft.observers.len(), 1);

    let before = TimelineMessageQueries::capture(repo, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(before.count(), 1);
    let initial = before.provider_messages();
    assert_eq!(initial[0]["role"], "user");
    assert_eq!(initial[0]["speaker"], "player");
    assert_eq!(initial[0]["message_id"], "dialogue:player:1");
    assert_eq!(initial[0]["targets"], json!(["Crazy"]));
    assert_eq!(
        initial[0]["participants"],
        json!(["player", "Crazy", "Kind"])
    );
    assert_eq!(initial[0]["content"], incoming.content);
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Kind, None)
            .unwrap()
            .count(),
        0,
    );
    assert_eq!(publish(repo, branch, incoming.clone()).unwrap(), first);
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);

    let mut different = incoming.clone();
    different.audience.targets.clear();
    different
        .audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Kind));
    assert_eq!(
        publish(repo, branch, different),
        Err(TimelineError::Conflict)
    );
    let mut different_time = incoming.clone();
    different_time.occurred_at_unix_ms += 1;
    assert_eq!(
        publish(repo, branch, different_time),
        Err(TimelineError::Conflict)
    );
    let mut duplicate_id = incoming.clone();
    duplicate_id.id = neuromita::timeline::EventId::new();
    assert_eq!(
        publish(repo, branch, duplicate_id),
        Err(TimelineError::Conflict)
    );
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);

    let mut assistant = ConversationMessage::new(
        "dialogue:Crazy:1",
        ConversationSpeaker::Character(CharacterId::Crazy),
        json!("Answer"),
        [CharacterId::Kind],
    );
    assistant
        .audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Kind));
    assistant.audience.participants.extend([
        ConversationSpeaker::Character(CharacterId::Crazy),
        ConversationSpeaker::Character(CharacterId::Kind),
    ]);
    assistant.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: incoming.id,
    });
    let answer = publish(repo, branch, assistant.clone()).unwrap();
    assert_eq!(answer.sequence, 2);
    assert_eq!(answer.draft.observers.len(), 2);
    assert_eq!(repo.world(branch).unwrap().len(), 2);

    let mita = TimelineMessageQueries::capture(repo, branch, CharacterId::Crazy, None).unwrap();
    let kind = TimelineMessageQueries::capture(repo, branch, CharacterId::Kind, None).unwrap();
    assert_eq!(mita.count(), 2);
    assert_eq!(kind.count(), 1);
    assert_eq!(mita.provider_messages()[1]["role"], "assistant");
    assert_eq!(kind.provider_messages()[0]["role"], "user");
    assert_eq!(kind.provider_messages()[0]["speaker"], "Crazy");
    assert_eq!(mita.messages()[1].event_id, kind.messages()[0].event_id);
    assert_eq!(kind.messages()[0].learned_at, 2);

    let mut retelling = ConversationMessage::new(
        "dialogue:kind:retelling",
        ConversationSpeaker::Character(CharacterId::Kind),
        json!("Mita answered a question before you arrived"),
        [CharacterId::Crazy],
    );
    retelling.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: incoming.id,
    });
    let retold = publish(repo, branch, retelling.clone()).unwrap();
    assert_eq!(retold.sequence, 3);
    assert_eq!(publish(repo, branch, retelling).unwrap(), retold);
    let kind = TimelineMessageQueries::capture(repo, branch, CharacterId::Kind, None).unwrap();
    assert_eq!(kind.count(), 2);
    assert!(
        !repo
            .knows(branch, CharacterId::Kind, incoming.id, None)
            .unwrap()
    );
    assert!(
        repo.knows(branch, CharacterId::Kind, retold.draft.id, None)
            .unwrap()
    );
    assert_eq!(before.count(), 1);

    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Crazy);
    repo.append(branch, 3, "reset:mita", json!({}), vec![reset])
        .unwrap();
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Crazy, None)
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Kind, None)
            .unwrap()
            .count(),
        2
    );
    (incoming, first)
}

#[test]
fn one_canonical_conversation_message_for_multiple_listeners_in_memory() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    contract(&mut repo, branch);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn one_canonical_conversation_message_for_multiple_listeners_sqlite_reopen() {
    use neuromita::timeline::{
        selection::TimelineSelectionRepository, storage::SqliteTimelineRepository,
    };
    let path = std::env::temp_dir().join(format!("conversation-{}.sqlite", uuid::Uuid::new_v4()));
    let branch = BranchId::new();
    let (input, original);
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        repo.select_branch(branch, None).unwrap();
        (input, original) = contract(&mut repo, branch);
    }
    {
        let mut reopened = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(publish(&mut reopened, branch, input).unwrap(), original);
        assert_eq!(reopened.branch(branch).unwrap().head_sequence, 4);
        let kind =
            TimelineMessageQueries::capture(&reopened, branch, CharacterId::Kind, None).unwrap();
        assert_eq!(kind.count(), 2);
        assert_eq!(kind.provider_messages()[0]["role"], "user");
        assert_eq!(
            kind.provider_messages()[1]["content"],
            json!("Mita answered a question before you arrived")
        );
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn structured_segments_survive_sqlite_reopen_as_one_message() {
    use neuromita::timeline::{
        selection::TimelineSelectionRepository, storage::SqliteTimelineRepository,
    };
    let path = std::env::temp_dir().join(format!(
        "structured-message-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let branch = BranchId::new();
    let segments = json!([
        {"text":"Hello", "emotions":["happy"]},
        {"text":"How are you?", "emotions":["curious"]},
    ]);
    let original;
    {
        let mut repository = SqliteTimelineRepository::open(&path).unwrap();
        repository.create_session(SessionId::new(), branch).unwrap();
        repository.select_branch(branch, None).unwrap();
        original = ConversationMessage::new(
            "structured:Kind:1",
            ConversationSpeaker::Character(CharacterId::Kind),
            segments.clone(),
            [CharacterId::Crazy],
        );
        publish(&mut repository, branch, original.clone()).unwrap();
    }
    {
        let mut reopened = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(
            publish(&mut reopened, branch, original).unwrap().sequence,
            1
        );
        assert_eq!(reopened.branch(branch).unwrap().head_sequence, 1);
        for listener in [CharacterId::Crazy, CharacterId::Kind] {
            let view = TimelineMessageQueries::capture(&reopened, branch, listener, None).unwrap();
            assert_eq!(view.provider_messages()[0]["content"], segments);
            assert_eq!(view.messages()[0].content, "Hello How are you?");
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn invalid_empty_audience_and_unknown_reply_do_not_write() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let message = ConversationMessage::new("empty", ConversationSpeaker::Player, json!("test"), []);
    assert!(matches!(
        publish(&mut repo, branch, message),
        Err(TimelineError::Invalid(_))
    ));
    let mut message = ConversationMessage::new(
        "character",
        ConversationSpeaker::Character(CharacterId::Crazy),
        json!("Hello"),
        [],
    );
    message.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: neuromita::timeline::EventId::new(),
    });
    assert_eq!(
        publish(&mut repo, branch, message),
        Err(TimelineError::NotFound)
    );
    assert_eq!(repo.branch(branch).unwrap().head_sequence, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_publishes_shared_conversation_without_generating() {
    use neuromita::{
        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        generation::{
            GenerationPreset, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
        },
    };
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "unused",
            "unused",
            GenerationSettings::new(0.0, 64, Duration::from_secs(2)).unwrap(),
        )
        .unwrap(),
    ));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
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
    ))
    .await
    .unwrap();
    let selection = app.timeline_selection();
    let original = ConversationMessage::new(
        "shared-player-message",
        ConversationSpeaker::Player,
        json!("Hi both"),
        CharacterId::ALL,
    );
    let event = app
        .publish_conversation_message(selection, original.clone())
        .unwrap();
    assert_eq!(app.active_requests(), 0);
    assert_eq!(app.at().through, 1);
    let at = app.at();
    for character in CharacterId::ALL {
        let view = app
            .character(character)
            .unwrap()
            .get_context(at)
            .await
            .unwrap();
        assert_eq!(view.view.messages().len(), 1);
        assert_eq!(view.view.messages()[0].event_id, event.draft.id);
    }
    assert_eq!(
        app.publish_conversation_message(selection, original)
            .unwrap(),
        event
    );
    assert_eq!(app.at().through, 1);
    let private = ConversationMessage::new(
        "private:1",
        ConversationSpeaker::Player,
        json!("only Mita"),
        [CharacterId::Crazy],
    );
    let private_fact = app
        .publish_conversation_message(selection, private)
        .unwrap();
    let old_at = app.at();
    let before_disclosure = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(old_at)
        .await
        .unwrap();
    assert_eq!(before_disclosure.view.count(), 1);
    let mut report = ConversationMessage::new(
        "shared:retelling",
        ConversationSpeaker::Character(CharacterId::Crazy),
        json!("A private question was asked"),
        [CharacterId::Kind],
    );
    report.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: private_fact.draft.id,
    });
    let shared = app.publish_conversation_message(selection, report).unwrap();
    assert_eq!(shared.draft.kind, EventKind::CharacterMessage);
    let after = app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(after.view.count(), 2);
    assert_eq!(after.view.messages()[1].event_id, shared.draft.id);
    assert_ne!(after.view.messages()[1].event_id, private_fact.draft.id);
    assert_eq!(app.active_requests(), 0);
    app.shutdown().await.unwrap();
}
