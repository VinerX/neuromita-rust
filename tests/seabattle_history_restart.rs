#![cfg(feature = "sqlite-system")]

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, ChatApi},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    games::{
        GameApi, GamePolicy,
        seabattle::{Coordinate, Orientation},
    },
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{
        ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState, UserInput,
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

struct Call {
    input: GenerationInput,
    reply: oneshot::Sender<StructuredResponse>,
}

struct ControlledProvider(mpsc::UnboundedSender<Call>);

impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(Call { input, reply })
                .map_err(|_| GenerationError::Transport("fixture closed".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture response dropped".into()))
        })
    }
}

async fn start(
    policy: GamePolicy,
    timeline: neuromita::timeline::TimelineConfig,
) -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
    let (send, receive) = mpsc::unbounded_channel();
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledProvider(send));
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "acceptance",
            "fixture",
            GenerationSettings::new(0.0, 1024, Duration::from_secs(10)).unwrap(),
        )
        .unwrap(),
    ));
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(32).unwrap(),
            character_commands: NonZeroUsize::new(16).unwrap(),
            presenter_updates: NonZeroUsize::new(32).unwrap(),
            retained_finished: NonZeroUsize::new(16).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.games = policy;
    config.timeline = timeline;
    (ApplicationLifecycle::start(config).await.unwrap(), receive)
}

fn response(commands: &[&str]) -> StructuredResponse {
    let mut response = StructuredResponse::spoken("Проверенный игровой ответ");
    response.segments[0].commands = commands.iter().map(|s| (*s).to_owned()).collect();
    response
}

async fn next(calls: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    timeout(Duration::from_secs(10), calls.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn finish(handle: &RequestHandle) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(10), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn canonical_game_survives_historical_reply_fork_and_core_restart_without_leaks() {
    use neuromita::timeline::{
        ContextPosition, TimelineConfig,
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        storage::SqliteTimelineRepository,
    };
    let database =
        std::env::temp_dir().join(format!("seabattle-core-{}.sqlite", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let config = || TimelineConfig::Sqlite {
        database: database.clone(),
        session,
        root,
    };
    let policy = GamePolicy {
        enabled: true,
        auto_reactions: false,
    };
    let (app, mut calls) = start(policy, config()).await;
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let before_game = app.at();
    GameApi::start(CharacterId::Crazy, false).await.unwrap();
    GameApi::place_ship(
        CharacterId::Crazy,
        Coordinate::parse("A1").unwrap(),
        4,
        Orientation::Horizontal,
    )
    .await
    .unwrap();
    let input = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("saved question".into()).unwrap(),
    )
    .unwrap();
    let saved = app.at();
    let saved_key = GameApi::view(CharacterId::Crazy).await.unwrap().key;
    // Fork from an explicit Timeline checkpoint after the private placement snapshot.
    let child = BranchId::new();
    {
        let mut repo = SqliteTimelineRepository::open(&database).unwrap();
        let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
        checkpoint
            .payload
            .insert("snapshot".into(), serde_json::json!({}));
        let checkpoint_id = checkpoint.id;
        repo.append(
            root,
            repo.branch(root).unwrap().head_sequence,
            "game-test-checkpoint",
            serde_json::json!({}),
            vec![checkpoint],
        )
        .unwrap();
        repo.fork(root, checkpoint_id, child).unwrap();
        let facts = repo.world(root).unwrap();
        let private: Vec<_> = facts
            .iter()
            .filter(|fact| fact.draft.kind == EventKind::GameStateCaptured)
            .collect();
        assert_eq!(private.len(), 2);
        for fact in private {
            assert_eq!(fact.draft.actor.as_deref(), Some("Crazy"));
            assert_eq!(
                fact.draft.observers.iter().copied().collect::<Vec<_>>(),
                vec![CharacterId::Crazy]
            );
            assert!(fact.draft.payload.contains_key("game_state"));
            for character in CharacterId::ALL {
                assert!(!repo.knows(root, character, fact.draft.id, None).unwrap());
                assert!(
                    repo.view(root, character, None)
                        .unwrap()
                        .iter()
                        .all(|known| known.fact.draft.kind != EventKind::GameStateCaptured)
                );
                let rejected = ChatApi::react(neuromita::request::ReactRequest::new(
                    character,
                    fact.draft.id,
                    "must not react to storage",
                    RequestSource::Presenter,
                ));
                assert!(rejected.is_err());
            }
        }
    }
    assert!(owner.get_context(before_game).await.unwrap().game.is_none());
    let old = owner.get_context(saved).await.unwrap();
    assert!(
        old.game
            .as_ref()
            .unwrap()
            .own_board
            .iter()
            .all(|row| !row.contains('S'))
    );
    assert!(
        old.game
            .as_ref()
            .unwrap()
            .opponent_board
            .iter()
            .all(|row| !row.contains('S'))
    );
    assert!(
        CharacterApi::get(CharacterId::Kind)
            .unwrap()
            .get_context(saved)
            .await
            .unwrap()
            .game
            .is_none()
    );
    // A later message makes the anchor historical without changing game or character revision.
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("later message".into()).unwrap(),
    )
    .unwrap();
    let historical = ChatApi::reply(
        ReplyRequest::new(CharacterId::Crazy, input, RequestSource::Presenter)
            .at(ContextPosition::At(saved)),
    )
    .unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.input.context.anchor, saved);
    let prompt = format!("{:?}", call.input.context);
    assert!(!prompt.contains("game_state"));
    assert!(!prompt.contains("RecordedAction"));
    assert!(
        call.input
            .context
            .game
            .as_ref()
            .unwrap()
            .opponent_board
            .iter()
            .all(|row| !row.contains('S'))
    );
    call.reply.send(response(&["PlaceShip,A1,4,H"])).unwrap();
    assert!(matches!(
        finish(&historical).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    if let RequestState::Finished(RequestOutcome::Success(accepted)) =
        &finish(&historical).await.state
    {
        assert!(
            accepted
                .effects
                .iter()
                .all(|effect| !format!("{effect:?}").contains("GameSlot"))
        );
    }
    assert_eq!(owner.get_context(saved).await.unwrap().game, old.game);
    assert!(
        owner
            .get_context(app.at())
            .await
            .unwrap()
            .game
            .unwrap()
            .own_board[0]
            .starts_with("SSSS")
    );
    let mixed_input = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("mixed transaction".into()).unwrap(),
    )
    .unwrap();
    let mixed = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        mixed_input,
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next(&mut calls).await;
    let before_mixed = app.at();
    let mut invalid = response(&["PlaceShip,A3,3,H"]);
    invalid.attitude_change = 5.0;
    invalid.memory_add = Some(vec!["must not persist".into()]);
    invalid.timer_add = Some(vec!["600|must not schedule".into()]);
    invalid.reminder_delete = Some(vec!["999999".into()]);
    call.reply.send(invalid).unwrap();
    assert!(matches!(
        finish(&mixed).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert_eq!(app.at(), before_mixed);
    assert!(
        neuromita::reminders::RemindersApi::list(CharacterId::Crazy)
            .await
            .unwrap()
            .is_empty()
    );
    let root_view = GameApi::view(CharacterId::Crazy).await.unwrap().view;
    // Fork inherits the prefix, with an independent game state thereafter.
    app.select_timeline_branch(child, app.timeline_selection())
        .unwrap();
    assert_eq!(
        GameApi::view(CharacterId::Crazy)
            .await
            .unwrap()
            .key
            .unwrap()
            .session,
        saved_key.unwrap().session
    );
    assert_eq!(owner.get_context(app.at()).await.unwrap().game, old.game);
    GameApi::place_ship(
        CharacterId::Crazy,
        Coordinate::parse("A3").unwrap(),
        3,
        Orientation::Horizontal,
    )
    .await
    .unwrap();
    let child_view = GameApi::view(CharacterId::Crazy).await.unwrap().view;
    app.select_timeline_branch(root, app.timeline_selection())
        .unwrap();
    assert_eq!(
        GameApi::view(CharacterId::Crazy).await.unwrap().view,
        root_view
    );
    app.shutdown().await.unwrap();
    let (app, mut calls) = start(policy, config()).await;
    assert_eq!(
        GameApi::view(CharacterId::Crazy).await.unwrap().view,
        root_view
    );
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    assert_eq!(owner.get_context(saved).await.unwrap().game, old.game);
    // Historical generation still runs, but a changed actual game/revision rejects the whole proposal.
    let stale = ChatApi::reply(
        ReplyRequest::new(CharacterId::Crazy, input, RequestSource::Presenter)
            .at(ContextPosition::At(saved)),
    )
    .unwrap();
    let call = next(&mut calls).await;
    assert_eq!(call.input.context.game, old.game);
    let before = app.at();
    let mut answer = response(&["PlaceShip,A3,3,H"]);
    answer.attitude_change = 7.0;
    call.reply.send(answer).unwrap();
    assert!(matches!(
        finish(&stale).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert_eq!(app.at(), before);
    assert_eq!(
        GameApi::view(CharacterId::Crazy).await.unwrap().view,
        root_view
    );
    app.select_timeline_branch(child, app.timeline_selection())
        .unwrap();
    assert_eq!(
        GameApi::view(CharacterId::Crazy).await.unwrap().view,
        child_view
    );
    assert!(
        GameApi::view(CharacterId::Kind)
            .await
            .unwrap()
            .view
            .is_none()
    );
    GameApi::close(CharacterId::Crazy).await.unwrap();
    app.shutdown().await.unwrap();
    let (app, _) = start(policy, config()).await;
    assert!(
        GameApi::view(CharacterId::Crazy)
            .await
            .unwrap()
            .view
            .is_none()
    );
    assert_eq!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .get_context(saved)
            .await
            .unwrap()
            .game,
        old.game
    );
    app.shutdown().await.unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", database.display()));
    }
}
