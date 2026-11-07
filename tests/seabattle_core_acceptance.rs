use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, ChatApi},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    games::{
        GameApi, GamePolicy, UserGameCommand,
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

async fn start(policy: GamePolicy) -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
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
    (ApplicationLifecycle::start(config).await.unwrap(), receive)
}

fn request() -> RequestHandle {
    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("game acceptance".into()).unwrap(),
    )
    .unwrap();
    ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        event,
        RequestSource::Presenter,
    ))
    .unwrap()
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
async fn owner_transactions_reject_partial_stale_and_cancelled_model_moves() {
    let (app, mut calls) = start(GamePolicy {
        enabled: true,
        auto_reactions: false,
    })
    .await;
    let character = CharacterId::Crazy;
    let first = GameApi::start(character, false).await.unwrap();
    assert!(first.reaction.is_none());
    assert!(first.receipt.view.is_some());
    let first_key = first.receipt.key.unwrap();
    let placed = GameApi::place_ship(
        character,
        Coordinate::parse("A1").unwrap(),
        4,
        Orientation::Horizontal,
    )
    .await
    .unwrap();
    assert!(placed.receipt.view.unwrap().own_board[0].starts_with("SSSS"));
    assert!(placed.receipt.key.unwrap().revision > first_key.revision);
    assert!(
        GameApi::view(CharacterId::Kind)
            .await
            .unwrap()
            .view
            .is_none()
    );
    let closed = GameApi::close(character).await.unwrap();
    assert!(closed.receipt.view.is_none());
    let fresh = GameApi::start(character, false).await.unwrap();
    let key = fresh.receipt.key.unwrap();
    assert_ne!(key.session, first_key.session);

    let before_state = CharacterApi::get(character).unwrap().state().await.unwrap();
    let partial = request();
    let call = next(&mut calls).await;
    assert_eq!(
        call.input.context.game.as_ref().unwrap().session,
        key.session
    );
    let mut invalid = response(&["PlaceShip,A1,4,H", "PlaceShip,A2,3,H"]);
    invalid.attitude_change = 5.0;
    call.reply.send(invalid).unwrap();
    assert!(matches!(
        finish(&partial).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    let view = GameApi::view(character).await.unwrap();
    assert_eq!(view.key, Some(key));
    assert!(
        view.view
            .unwrap()
            .own_board
            .iter()
            .all(|row| !row.contains('S'))
    );
    let after_state = CharacterApi::get(character).unwrap().state().await.unwrap();
    assert_eq!(before_state.attitude, after_state.attitude);
    assert_eq!(before_state.accepted_replies, after_state.accepted_replies);

    let stale = request();
    let call = next(&mut calls).await;
    GameApi::close(character).await.unwrap();
    let replacement = GameApi::start(character, false).await.unwrap();
    let replacement_key = replacement.receipt.key;
    call.reply.send(response(&["PlaceShip,A1,4,H"])).unwrap();
    finish(&stale).await;
    let actual = GameApi::view(character).await.unwrap();
    assert_eq!(actual.key, replacement_key);
    assert!(
        actual
            .view
            .unwrap()
            .own_board
            .iter()
            .all(|row| !row.contains('S'))
    );

    let cancelled = request();
    let call = next(&mut calls).await;
    cancelled.cancel();
    assert!(matches!(
        finish(&cancelled).await.state,
        RequestState::Cancelled
    ));
    let _ = call.reply.send(response(&["PlaceShip,A1,4,H"]));
    assert_eq!(GameApi::view(character).await.unwrap().key, replacement_key);
    app.shutdown().await.unwrap();

    let (app, mut calls) = start(GamePolicy {
        enabled: false,
        auto_reactions: false,
    })
    .await;
    assert!(GameApi::start(character, false).await.is_err());
    assert!(
        CharacterApi::get(character)
            .unwrap()
            .game(UserGameCommand::Start {
                auto_reactions: false
            })
            .await
            .is_err()
    );
    let disabled = request();
    let call = next(&mut calls).await;
    let mut start_response = response(&[]);
    start_response.segments[0].start_game = Some("seabattle".into());
    call.reply.send(start_response).unwrap();
    finish(&disabled).await;
    assert!(GameApi::view(character).await.unwrap().view.is_none());
    app.shutdown().await.unwrap();
}
