use std::sync::Arc;

use neuromita::{
    api::ChatApi,
    character::{AcceptedResponse as CharacterResponse, CharacterId},
    request::{
        AcceptedResponse as RequestResponse, InputError, ReplyRequest, RequestHandle, SubmitError,
        TurnIntent, UserInput,
    },
    settings::{SettingsApi, SettingsError, SettingsSnapshot, VoiceSettings},
    timeline::EventId,
};

#[test]
fn public_contract_paths_remain_stable_after_dependency_inversion() {
    let _: fn(ReplyRequest) -> Result<RequestHandle, SubmitError> = ChatApi::reply;
    let _: fn() -> Result<Arc<SettingsSnapshot>, SettingsError> = SettingsApi::snapshot_all;
    let _: fn() -> Result<neuromita::settings::SectionSnapshot<VoiceSettings>, SettingsError> =
        SettingsApi::snapshot::<VoiceSettings>;
    let _: fn(String) -> Result<UserInput, InputError> = UserInput::new;
    let _: fn() -> EventId = EventId::new;
    let _: fn(&str) -> Option<CharacterId> = CharacterId::parse;

    fn from_request(response: RequestResponse) -> CharacterResponse {
        response
    }
    let _: fn(RequestResponse) -> CharacterResponse = from_request;

    let input = UserInput::new("hello".to_owned()).expect("valid input");
    assert_eq!(input.text().as_ref(), "hello");
    assert!(matches!(
        TurnIntent::React(input.event_id()),
        TurnIntent::React(_)
    ));
    assert_eq!(
        serde_json::to_string(&CharacterId::Crazy).unwrap(),
        "\"Crazy\""
    );
}
