use crate::{
    character::{CharacterError, CharacterId},
    games::{
        GameError, GamePolicy, GameReceipt, ReactionInvite, UserGameCommand,
        seabattle::{Coordinate, Orientation},
    },
    request::{ReactRequest, RequestHandle, RequestSource, SubmitError},
};

/// Result of one owner command. `reaction` reports whether the ordinary
/// reaction admission accepted (or rejected) the requested follow-up; the
/// committed player action is never rolled back by a full chat queue.
pub struct GameOutcome {
    pub receipt: GameReceipt,
    pub reaction: Option<Result<RequestHandle, SubmitError>>,
}

impl std::fmt::Debug for GameOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameOutcome")
            .field("receipt", &self.receipt)
            .field("reaction", &self.reaction.as_ref().map(|_| "submitted"))
            .finish()
    }
}

/// Static entry point for the human side of the owned mini-game. It routes
/// typed commands to the character's mailbox and then submits an ordinary
/// reaction request for the committed match fact.
pub struct GameApi;

impl GameApi {
    pub async fn command(
        character: CharacterId,
        command: UserGameCommand,
    ) -> Result<GameOutcome, GameError> {
        let bindings = crate::runtime::current().map_err(|_| GameError::Unavailable)?;
        let policy = &bindings.games;
        if !policy.enabled && matches!(command, UserGameCommand::Start { .. }) {
            return Err(GameError::Disabled);
        }
        let handle = bindings
            .characters
            .get(character)
            .ok_or(GameError::Unavailable)?;
        let reply = handle.game(command).await.map_err(map_character)?;
        let reaction = Self::submit(&bindings.chat, policy, character, reply.invite);
        Ok(GameOutcome {
            receipt: reply.receipt,
            reaction,
        })
    }

    pub async fn start(
        character: CharacterId,
        auto_reactions: bool,
    ) -> Result<GameOutcome, GameError> {
        Self::command(character, UserGameCommand::Start { auto_reactions }).await
    }

    pub async fn place_ship(
        character: CharacterId,
        coord: Coordinate,
        length: u8,
        orientation: Orientation,
    ) -> Result<GameOutcome, GameError> {
        Self::command(
            character,
            UserGameCommand::PlaceShip {
                coord,
                length,
                orientation,
            },
        )
        .await
    }

    pub async fn place_fleet_randomly(
        character: CharacterId,
        seed: u64,
    ) -> Result<GameOutcome, GameError> {
        Self::command(character, UserGameCommand::PlaceFleetRandomly { seed }).await
    }

    pub async fn fire(character: CharacterId, coord: Coordinate) -> Result<GameOutcome, GameError> {
        Self::command(character, UserGameCommand::Fire { coord }).await
    }

    pub async fn request_mita_turn(character: CharacterId) -> Result<GameOutcome, GameError> {
        Self::command(character, UserGameCommand::RequestMitaTurn).await
    }

    pub async fn set_auto_reactions(
        character: CharacterId,
        enabled: bool,
    ) -> Result<GameOutcome, GameError> {
        Self::command(character, UserGameCommand::SetAutoReactions(enabled)).await
    }

    pub async fn close(character: CharacterId) -> Result<GameOutcome, GameError> {
        Self::command(character, UserGameCommand::Close).await
    }

    pub async fn view(character: CharacterId) -> Result<GameReceipt, GameError> {
        let bindings = crate::runtime::current().map_err(|_| GameError::Unavailable)?;
        let handle = bindings
            .characters
            .get(character)
            .ok_or(GameError::Unavailable)?;
        handle.game_view().await.map_err(map_character)
    }

    fn submit(
        chat: &crate::request::ChatApiState,
        policy: &GamePolicy,
        character: CharacterId,
        invite: Option<ReactionInvite>,
    ) -> Option<Result<RequestHandle, SubmitError>> {
        let invite = invite?;
        if invite.auto_gated && !policy.auto_reactions {
            return None;
        }
        Some(chat.send(ReactRequest::new(
            character,
            invite.event_id,
            invite.instruction,
            RequestSource::Presenter,
        )))
    }
}

fn map_character(error: CharacterError) -> GameError {
    match error {
        CharacterError::InvalidGame(error) => error,
        CharacterError::Unavailable => GameError::Unavailable,
        CharacterError::StaleState => GameError::StaleState,
        other => GameError::Internal(other.to_string()),
    }
}
