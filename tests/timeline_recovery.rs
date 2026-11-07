use neuromita::{
    character::CharacterId,
    request::RequestId,
    timeline::domain::{
        BranchId, EventDraft, EventKind, SessionId, TimelineError, TimelineFact,
        TimelineRepository, TimelineStore,
    },
};
use serde_json::json;
use uuid::Uuid;

fn input(request: RequestId, character: CharacterId) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft.observers.insert(character);
    draft.payload.insert("content".into(), json!("input"));
    draft
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    draft
}

fn reply(request: RequestId, character: CharacterId) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::CharacterMessage);
    draft.actor = Some(character.to_string());
    draft.observers.insert(character);
    draft
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    draft.payload.insert("content".into(), json!("accepted"));
    draft
}

fn append(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    operation: &str,
    draft: EventDraft,
) -> TimelineFact {
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(branch, head, operation, json!({}), vec![draft])
        .unwrap()
        .pop()
        .unwrap()
}

#[test]
fn recovery_uses_only_accepted_replies_not_admission_or_reset() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let request = RequestId(Uuid::new_v4());
    let incoming = append(&mut repo, branch, "in", input(request, CharacterId::Crazy));
    let accepted = |repo: &dyn TimelineRepository, at| {
        repo.accepted_reply_at(branch, CharacterId::Crazy, request.0, at)
            .unwrap()
    };
    assert!(accepted(&repo, None).is_none());
    assert!(accepted(&repo, Some(incoming.sequence)).is_none());

    let mut proactive = EventDraft::new(EventKind::ProactiveOpportunity);
    proactive.actor = Some(CharacterId::Crazy.to_string());
    proactive.observers.insert(CharacterId::Crazy);
    proactive
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    append(&mut repo, branch, "proactive", proactive);
    assert!(accepted(&repo, None).is_none());

    let committed = append(
        &mut repo,
        branch,
        "accepted",
        reply(request, CharacterId::Crazy),
    );
    assert_eq!(accepted(&repo, None), Some(committed.clone()));
    assert!(accepted(&repo, Some(committed.sequence - 1)).is_none());
    assert_eq!(
        repo.accepted_reply_at(branch, CharacterId::Kind, request.0, None)
            .unwrap(),
        None
    );
    assert_eq!(
        repo.accepted_reply_at(branch, CharacterId::Crazy, Uuid::new_v4(), None)
            .unwrap(),
        None
    );

    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Crazy);
    append(&mut repo, branch, "reset", reset);
    assert_eq!(accepted(&repo, None), Some(committed));
    assert!(matches!(
        repo.accepted_reply_at(branch, CharacterId::Crazy, request.0, Some(1_000)),
        Err(TimelineError::Invalid(_))
    ));
}

#[test]
fn recovery_follows_fork_cutoff_and_detects_duplicate_commit() {
    let mut repo = TimelineStore::new();
    let root = BranchId::new();
    let child = BranchId::new();
    repo.create_session(SessionId::new(), root).unwrap();
    let prior = RequestId(Uuid::new_v4());
    let future = RequestId(Uuid::new_v4());
    let committed = append(&mut repo, root, "prior", reply(prior, CharacterId::Crazy));
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(&mut repo, root, "checkpoint", checkpoint);
    repo.fork(root, checkpoint_id, child).unwrap();
    append(&mut repo, root, "future", reply(future, CharacterId::Crazy));
    assert_eq!(
        repo.accepted_reply_at(child, CharacterId::Crazy, prior.0, None)
            .unwrap()
            .unwrap()
            .draft
            .id,
        committed.draft.id
    );
    assert!(
        repo.accepted_reply_at(child, CharacterId::Crazy, future.0, None)
            .unwrap()
            .is_none()
    );
    append(
        &mut repo,
        child,
        "branch-reply",
        reply(future, CharacterId::Crazy),
    );
    assert!(
        repo.accepted_reply_at(child, CharacterId::Crazy, future.0, None)
            .unwrap()
            .is_some()
    );
    append(
        &mut repo,
        child,
        "duplicate",
        reply(future, CharacterId::Crazy),
    );
    assert_eq!(
        repo.accepted_reply_at(child, CharacterId::Crazy, future.0, None),
        Err(TimelineError::Conflict)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_reopen_reconciles_uncertain_commits_without_replaying_them() {
    use neuromita::timeline::storage::SqliteTimelineRepository;

    let path = std::env::temp_dir().join(format!("neuromita-recovery-{}.db", Uuid::new_v4()));
    let branch = BranchId::new();
    let session = SessionId::new();
    let accepted = RequestId(Uuid::new_v4());
    let unaccepted = RequestId(Uuid::new_v4());
    let committed = {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(session, branch).unwrap();
        append(
            &mut repo,
            branch,
            "admitted",
            input(unaccepted, CharacterId::Crazy),
        );
        append(
            &mut repo,
            branch,
            "input",
            input(accepted, CharacterId::Crazy),
        );
        append(
            &mut repo,
            branch,
            "accepted",
            reply(accepted, CharacterId::Crazy),
        )
    };
    for _ in 0..2 {
        let repo = SqliteTimelineRepository::open(&path).unwrap();
        let before = repo.branch(branch).unwrap().head_sequence;
        assert_eq!(
            repo.accepted_reply_at(branch, CharacterId::Crazy, accepted.0, None)
                .unwrap(),
            Some(committed.clone())
        );
        assert!(
            repo.accepted_reply_at(branch, CharacterId::Crazy, unaccepted.0, None)
                .unwrap()
                .is_none()
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, before);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn request_evidence_distinguishes_ingress_dispatch_and_commit() {
    use neuromita::timeline::domain::RequestEvidence;

    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let request = RequestId(Uuid::new_v4());
    let proactive_request = RequestId(Uuid::new_v4());
    let unknown = RequestId(Uuid::new_v4());

    let evidence = |store: &TimelineStore, id: RequestId, at: Option<u64>| {
        store
            .request_evidence_at(branch, CharacterId::Crazy, id.0, at)
            .unwrap()
    };
    assert_eq!(evidence(&repo, unknown, None), RequestEvidence::NoRecord);
    let incoming = append(
        &mut repo,
        branch,
        "input",
        input(request, CharacterId::Crazy),
    );
    assert_eq!(
        evidence(&repo, request, None),
        RequestEvidence::PlayerInput(incoming.clone())
    );
    assert_eq!(
        repo.request_evidence_at(branch, CharacterId::Kind, request.0, None),
        Ok(RequestEvidence::NoRecord)
    );

    let mut opportunity = EventDraft::new(EventKind::ProactiveOpportunity);
    opportunity.actor = Some(CharacterId::Crazy.to_string());
    opportunity.observers.insert(CharacterId::Crazy);
    opportunity
        .payload
        .insert("request_id".into(), json!(proactive_request.to_string()));
    let started = append(&mut repo, branch, "proactive", opportunity);
    assert_eq!(
        evidence(&repo, proactive_request, None),
        RequestEvidence::ProactiveTrigger(started)
    );
    assert_eq!(
        evidence(&repo, request, Some(incoming.sequence)),
        RequestEvidence::PlayerInput(incoming.clone())
    );

    let accepted = append(
        &mut repo,
        branch,
        "reply",
        reply(request, CharacterId::Crazy),
    );
    assert_eq!(
        evidence(&repo, request, None),
        RequestEvidence::AcceptedReply(accepted.clone())
    );
    assert_eq!(
        evidence(&repo, request, Some(accepted.sequence - 1)),
        RequestEvidence::PlayerInput(incoming.clone())
    );
    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Crazy);
    append(&mut repo, branch, "reset", reset);
    assert_eq!(
        evidence(&repo, request, None),
        RequestEvidence::AcceptedReply(accepted)
    );
    assert!(matches!(
        repo.request_evidence_at(branch, CharacterId::Crazy, request.0, Some(10_000)),
        Err(TimelineError::Invalid(_))
    ));

    let mut no_request_id = EventDraft::new(EventKind::PlayerMessage);
    no_request_id.actor = Some("player".into());
    no_request_id.observers.insert(CharacterId::Crazy);
    append(&mut repo, branch, "message-only", no_request_id);
    assert_eq!(evidence(&repo, unknown, None), RequestEvidence::NoRecord);
}

#[test]
fn request_evidence_respects_fork_privacy_and_detects_ambiguous_facts() {
    use neuromita::timeline::domain::RequestEvidence;

    let mut repo = TimelineStore::new();
    let root = BranchId::new();
    let fork = BranchId::new();
    repo.create_session(SessionId::new(), root).unwrap();
    let original = RequestId(Uuid::new_v4());
    let after_fork = RequestId(Uuid::new_v4());
    let seen = append(
        &mut repo,
        root,
        "input",
        input(original, CharacterId::Crazy),
    );
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(&mut repo, root, "checkpoint", checkpoint);
    repo.fork(root, checkpoint_id, fork).unwrap();
    append(
        &mut repo,
        root,
        "parent-future",
        reply(after_fork, CharacterId::Crazy),
    );

    assert_eq!(
        repo.request_evidence_at(fork, CharacterId::Crazy, original.0, None),
        Ok(RequestEvidence::PlayerInput(seen))
    );
    assert_eq!(
        repo.request_evidence_at(fork, CharacterId::Crazy, after_fork.0, None),
        Ok(RequestEvidence::NoRecord)
    );
    assert_eq!(
        repo.request_evidence_at(fork, CharacterId::Kind, original.0, None),
        Ok(RequestEvidence::NoRecord)
    );
    let accepted = append(
        &mut repo,
        fork,
        "fork-reply",
        reply(after_fork, CharacterId::Crazy),
    );
    assert_eq!(
        repo.request_evidence_at(fork, CharacterId::Crazy, after_fork.0, None),
        Ok(RequestEvidence::AcceptedReply(accepted))
    );

    append(
        &mut repo,
        fork,
        "duplicate",
        reply(after_fork, CharacterId::Crazy),
    );
    assert_eq!(
        repo.request_evidence_at(fork, CharacterId::Crazy, after_fork.0, None),
        Err(TimelineError::Conflict)
    );
    assert!(
        repo.accepted_reply_at(fork, CharacterId::Crazy, after_fork.0, None)
            .is_err()
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn request_evidence_survives_sqlite_reopen_without_a_request_registry() {
    use neuromita::timeline::{domain::RequestEvidence, storage::SqliteTimelineRepository};

    let path =
        std::env::temp_dir().join(format!("neuromita-request-evidence-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let branch = BranchId::new();
    let pending = RequestId(Uuid::new_v4());
    let accepted = RequestId(Uuid::new_v4());
    let proactive = RequestId(Uuid::new_v4());
    let (pending_fact, accepted_fact, trigger_fact) = {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(session, branch).unwrap();
        let pending_fact = append(
            &mut repo,
            branch,
            "pending",
            input(pending, CharacterId::Crazy),
        );
        append(
            &mut repo,
            branch,
            "accepted-input",
            input(accepted, CharacterId::Crazy),
        );
        let accepted_fact = append(
            &mut repo,
            branch,
            "reply",
            reply(accepted, CharacterId::Crazy),
        );
        let mut trigger = EventDraft::new(EventKind::ProactiveOpportunity);
        trigger.actor = Some(CharacterId::Kind.to_string());
        trigger.observers.insert(CharacterId::Kind);
        trigger
            .payload
            .insert("request_id".into(), json!(proactive.to_string()));
        let trigger_fact = append(&mut repo, branch, "trigger", trigger);
        (pending_fact, accepted_fact, trigger_fact)
    };
    for _ in 0..2 {
        let repo = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(
            repo.request_evidence_at(branch, CharacterId::Crazy, pending.0, None),
            Ok(RequestEvidence::PlayerInput(pending_fact.clone()))
        );
        assert_eq!(
            repo.request_evidence_at(branch, CharacterId::Crazy, accepted.0, None),
            Ok(RequestEvidence::AcceptedReply(accepted_fact.clone()))
        );
        assert_eq!(
            repo.request_evidence_at(branch, CharacterId::Kind, proactive.0, None),
            Ok(RequestEvidence::ProactiveTrigger(trigger_fact.clone()))
        );
        assert_eq!(
            repo.request_evidence_at(branch, CharacterId::Crazy, proactive.0, None),
            Ok(RequestEvidence::NoRecord)
        );
    }
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn preissued_input_identity_recovers_request_without_handle() {
    use neuromita::request::UserInput;
    use neuromita::timeline::domain::RequestEvidence;

    let input_id = neuromita::timeline::EventId::new();
    let supplied_input = UserInput::with_event_id(input_id, "hello".into()).unwrap();
    assert_eq!(supplied_input.event_id(), input_id);
    assert!(UserInput::with_event_id(input_id, "   ".into()).is_err());

    let mut repo = TimelineStore::new();
    let root = BranchId::new();
    let child = BranchId::new();
    repo.create_session(SessionId::new(), root).unwrap();
    let req = RequestId(Uuid::new_v4());
    let mut draft = input(req, CharacterId::Crazy);
    draft.id = input_id;
    let admitted = append(&mut repo, root, "first", draft);
    let lookup = |repo: &TimelineStore, branch, character, through| {
        repo.request_evidence_for_input_at(branch, character, input_id, through)
            .unwrap()
    };
    assert_eq!(lookup(&repo, root, CharacterId::Crazy, Some(0)), None);
    assert_eq!(lookup(&repo, root, CharacterId::Kind, None), None);
    assert_eq!(
        lookup(&repo, root, CharacterId::Crazy, None),
        Some((req.0, RequestEvidence::PlayerInput(admitted.clone())))
    );

    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(&mut repo, root, "checkpoint", checkpoint);
    repo.fork(root, checkpoint_id, child).unwrap();
    let accepted = append(&mut repo, root, "reply", reply(req, CharacterId::Crazy));
    assert_eq!(
        lookup(&repo, root, CharacterId::Crazy, None),
        Some((req.0, RequestEvidence::AcceptedReply(accepted)))
    );
    assert_eq!(
        lookup(&repo, child, CharacterId::Crazy, None),
        Some((req.0, RequestEvidence::PlayerInput(admitted)))
    );
    assert!(matches!(
        repo.request_evidence_for_input_at(root, CharacterId::Crazy, input_id, Some(10_000)),
        Err(TimelineError::Invalid(_))
    ));
}

#[test]
fn preissued_identity_matches_proactive_trigger_not_message_only() {
    use neuromita::timeline::domain::RequestEvidence;
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let request = RequestId(Uuid::new_v4());
    let mut proactive = EventDraft::new(EventKind::ProactiveOpportunity);
    proactive.actor = Some(CharacterId::Crazy.to_string());
    proactive.observers.insert(CharacterId::Crazy);
    proactive
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    let recorded = append(&mut repo, branch, "trigger", proactive);
    assert_eq!(
        repo.request_evidence_for_input_at(branch, CharacterId::Crazy, recorded.draft.id, None),
        Ok(Some((
            request.0,
            RequestEvidence::ProactiveTrigger(recorded.clone())
        )))
    );
    assert_eq!(
        repo.request_evidence_for_input_at(branch, CharacterId::Kind, recorded.draft.id, None),
        Ok(None)
    );
    let mut plain = EventDraft::new(EventKind::PlayerMessage);
    plain.actor = Some("player".into());
    plain.observers.insert(CharacterId::Crazy);
    let plain = append(&mut repo, branch, "plain", plain);
    assert_eq!(
        repo.request_evidence_for_input_at(branch, CharacterId::Crazy, plain.draft.id, None),
        Ok(None)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn preissued_identity_survives_sqlite_restart_and_resists_duplicate() {
    use neuromita::timeline::{domain::RequestEvidence, storage::SqliteTimelineRepository};
    let path = std::env::temp_dir().join(format!("neuromita-by-input-{}.db", Uuid::new_v4()));
    let branch = BranchId::new();
    let request = RequestId(Uuid::new_v4());
    let input_id = neuromita::timeline::EventId::new();
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut first = input(request, CharacterId::Crazy);
        first.id = input_id;
        append(&mut repo, branch, "admit", first);
    }
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        let evidence = repo
            .request_evidence_for_input_at(branch, CharacterId::Crazy, input_id, None)
            .unwrap()
            .unwrap();
        assert_eq!(evidence.0, request.0);
        assert!(matches!(evidence.1, RequestEvidence::PlayerInput(_)));
        let mut duplicate = input(RequestId(Uuid::new_v4()), CharacterId::Crazy);
        duplicate.id = input_id;
        assert_eq!(
            repo.append(branch, 1, "duplicate", json!({}), vec![duplicate]),
            Err(TimelineError::Conflict)
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
