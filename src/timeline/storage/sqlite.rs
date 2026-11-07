use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::identity::{CharacterId, EventId};
use crate::timeline::domain::{
    AppendGuard, Branch, BranchId, BranchInfo, EventDraft, EventKind, KnownEvent, Receipt,
    SessionId, TimelineError, TimelineFact, TimelineRepository, TimelineStore, WorldLookup,
};
use crate::timeline::selection::{TimelineSelection, TimelineSelectionRepository};

mod native;
use native::Connection;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS timeline_schema(version INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS timeline_revision(
    slot INTEGER PRIMARY KEY CHECK(slot=1),
    revision INTEGER NOT NULL CHECK(revision>=0)
);
INSERT OR IGNORE INTO timeline_revision(slot,revision) VALUES(1,0);
CREATE TABLE IF NOT EXISTS timeline_branches(
    branch_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    parent_id TEXT,
    fork_sequence INTEGER NOT NULL,
    head_sequence INTEGER NOT NULL,
    checkpoint_id TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS timeline_session_roots
    ON timeline_branches(session_id) WHERE parent_id IS NULL;
CREATE TABLE IF NOT EXISTS timeline_events(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    fact_json TEXT NOT NULL,
    PRIMARY KEY(branch_id, sequence),
    FOREIGN KEY(branch_id) REFERENCES timeline_branches(branch_id)
);
CREATE TABLE IF NOT EXISTS timeline_observers(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    character_id TEXT NOT NULL,
    PRIMARY KEY(branch_id, sequence, character_id),
    FOREIGN KEY(branch_id,sequence) REFERENCES timeline_events(branch_id,sequence) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS timeline_observers_by_character
    ON timeline_observers(character_id,branch_id,sequence);
CREATE TABLE IF NOT EXISTS timeline_receipts(
    branch_id TEXT NOT NULL,
    operation_key TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY(branch_id, operation_key),
    FOREIGN KEY(branch_id) REFERENCES timeline_branches(branch_id)
);
CREATE TABLE IF NOT EXISTS timeline_fact_keys(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    external_id TEXT,
    kind_json TEXT NOT NULL,
    PRIMARY KEY(branch_id,sequence)
);
CREATE INDEX IF NOT EXISTS timeline_keys_event ON timeline_fact_keys(branch_id,event_id,sequence);
CREATE INDEX IF NOT EXISTS timeline_keys_external ON timeline_fact_keys(branch_id,external_id,sequence);
CREATE TABLE IF NOT EXISTS timeline_acquisitions(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    character_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    PRIMARY KEY(branch_id,sequence,character_id,event_id)
);
CREATE INDEX IF NOT EXISTS timeline_acquisitions_by_event
    ON timeline_acquisitions(branch_id,character_id,event_id,sequence);
CREATE TABLE IF NOT EXISTS timeline_resets(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    character_id TEXT NOT NULL,
    PRIMARY KEY(branch_id,sequence,character_id)
);
CREATE INDEX IF NOT EXISTS timeline_resets_by_character
    ON timeline_resets(branch_id,character_id,sequence);
CREATE TABLE IF NOT EXISTS timeline_active_selection(
    slot TEXT PRIMARY KEY CHECK(slot='core'),
    branch_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    FOREIGN KEY(branch_id) REFERENCES timeline_branches(branch_id)
);
";

fn storage(message: impl Into<String>) -> TimelineError {
    TimelineError::Storage(message.into())
}

fn parse_uuid(text: &str) -> Result<Uuid, TimelineError> {
    Uuid::parse_str(text).map_err(|_| storage("invalid persisted UUID"))
}
fn parse_u64(text: &str) -> Result<u64, TimelineError> {
    text.parse()
        .map_err(|_| storage("invalid persisted sequence"))
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, TimelineError> {
    serde_json::from_str(text).map_err(|error| storage(error.to_string()))
}
fn encode<T: serde::Serialize>(value: &T) -> Result<String, TimelineError> {
    serde_json::to_string(value).map_err(|error| storage(error.to_string()))
}

/// SQLite is a replaceable transaction owner, not a second implementation of timeline rules.
/// SQLite sources are bundled through rusqlite when the sqlite-system feature is enabled.
pub struct SqliteTimelineRepository {
    path: PathBuf,
}

impl SqliteTimelineRepository {
    pub fn open_or_create_session(&mut self) -> Result<(SessionId, BranchId), TimelineError> {
        self.with_write(|connection, store| {
            let selected = read_selection(connection)?;
            let rows = if let Some(selected) = selected {
                let session = branch_info(connection, selected.branch_id)?.session_id;
                connection.query("SELECT session_id,branch_id FROM timeline_branches WHERE parent_id IS NULL AND session_id=?", &[&session.0.to_string()])?
            } else {
                connection.query("SELECT session_id,branch_id FROM timeline_branches WHERE parent_id IS NULL", &[])?
            };
            match rows.as_slice() {
                [] => {
                    let session = SessionId::new();
                    let root = BranchId::new();
                    store.create_session(session, root)?;
                    save_branch(connection, store, root)?;
                    Ok((session, root))
                }
                [row] => Ok((SessionId(parse_uuid(&row[0])?), BranchId(parse_uuid(&row[1])?))),
                _ => Err(TimelineError::Conflict),
            }
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, TimelineError> {
        let path = path.as_ref();
        if path.to_string_lossy() == ":memory:" {
            return Err(storage("SQLite timeline requires a file path"));
        }
        let repo = Self {
            path: path.to_path_buf(),
        };
        let conn = repo.connection()?;
        conn.exec("PRAGMA journal_mode = WAL")?;
        conn.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            conn.exec(SCHEMA)?;
            let versions = conn.query("SELECT version FROM timeline_schema", &[])?;
            match versions.as_slice() {
                [] => conn.write("INSERT INTO timeline_schema(version) VALUES('2')", &[]),
                [version] if version[0] == "2" => Ok(()),
                [version] if version[0] == "1" => {
                    for row in conn.query(
                        "SELECT fact_json FROM timeline_events ORDER BY branch_id,sequence",
                        &[],
                    )? {
                        index_fact(&conn, &decode::<TimelineFact>(&row[0])?)?;
                    }
                    conn.write("UPDATE timeline_schema SET version='2'", &[])
                }
                _ => Err(storage("unsupported timeline schema version")),
            }
        })();
        if result.is_ok() {
            conn.exec("COMMIT")?;
        } else {
            conn.exec("ROLLBACK")?;
        }
        result?;
        Ok(repo)
    }

    fn connection(&self) -> Result<Connection, TimelineError> {
        Connection::open(&self.path)
    }

    fn read_snapshot<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, TimelineError>,
    ) -> Result<T, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN")?;
        let result = operation(&connection);
        connection.exec("ROLLBACK")?;
        result
    }

    fn with_write<T>(
        &mut self,
        apply: impl FnOnce(&Connection, &mut TimelineStore) -> Result<T, TimelineError>,
    ) -> Result<T, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let mut store = load(&connection)?;
            let value = apply(&connection, &mut store)?;
            advance_revision(&connection, revision(&connection)?)?;
            Ok(value)
        })();
        if result.is_ok() {
            connection.exec("COMMIT")?;
        } else {
            connection.exec("ROLLBACK")?;
        }
        result
    }
}

fn revision(connection: &Connection) -> Result<u64, TimelineError> {
    let rows = connection.query("SELECT revision FROM timeline_revision WHERE slot=1", &[])?;
    let [row] = rows.as_slice() else {
        return Err(storage("timeline revision metadata missing"));
    };
    parse_u64(&row[0])
}

fn advance_revision(connection: &Connection, current: u64) -> Result<u64, TimelineError> {
    let next = current
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or(TimelineError::Invalid("timeline revision overflow"))?;
    connection.write(
        "UPDATE timeline_revision SET revision=? WHERE slot=1 AND revision=?",
        &[&next.to_string(), &current.to_string()],
    )?;
    if revision(connection)? != next {
        return Err(TimelineError::ConcurrentWrite);
    }
    Ok(next)
}

fn branch_info(connection: &Connection, branch: BranchId) -> Result<BranchInfo, TimelineError> {
    let rows = connection.query(
        "SELECT session_id,ifnull(parent_id,''),fork_sequence,head_sequence,ifnull(checkpoint_id,'') FROM timeline_branches WHERE branch_id=?",
        &[&branch.0.to_string()],
    )?;
    let [row] = rows.as_slice() else {
        return Err(TimelineError::NotFound);
    };
    Ok(BranchInfo {
        id: branch,
        session_id: SessionId(parse_uuid(&row[0])?),
        parent: if row[1].is_empty() {
            None
        } else {
            Some(BranchId(parse_uuid(&row[1])?))
        },
        fork_sequence: parse_u64(&row[2])?,
        head_sequence: parse_u64(&row[3])?,
        checkpoint: if row[4].is_empty() {
            None
        } else {
            Some(EventId(parse_uuid(&row[4])?))
        },
    })
}

fn load(connection: &Connection) -> Result<TimelineStore, TimelineError> {
    let mut branches = Vec::new();
    for row in connection.query("SELECT branch_id, session_id, ifnull(parent_id,''), fork_sequence, head_sequence, ifnull(checkpoint_id,'') FROM timeline_branches ORDER BY branch_id", &[])? {
        let id = BranchId(parse_uuid(&row[0])?);
        let info = BranchInfo {
            id,
            session_id: SessionId(parse_uuid(&row[1])?),
            parent: if row[2].is_empty() { None } else { Some(BranchId(parse_uuid(&row[2])?)) },
            fork_sequence: parse_u64(&row[3])?,
            head_sequence: parse_u64(&row[4])?,
            checkpoint: if row[5].is_empty() { None } else { Some(EventId(parse_uuid(&row[5])?)) },
        };
        let mut events = Vec::new();
        for event in connection.query("SELECT fact_json FROM timeline_events WHERE branch_id=? ORDER BY sequence", &[&row[0]])? {
            events.push(decode(&event[0])?);
        }
        let mut receipts = std::collections::HashMap::new();
        for receipt in connection.query("SELECT operation_key, receipt_json FROM timeline_receipts WHERE branch_id=?", &[&row[0]])? {
            receipts.insert(receipt[0].clone(), decode::<Receipt>(&receipt[1])?);
        }
        branches.push(Branch { info, events, receipts });
    }
    TimelineStore::load_persisted(branches)
}

/// Materialized *facts*, not domain decisions. These indexes are derived from
/// immutable events and may be rebuilt from them during schema migration.
fn index_fact(conn: &Connection, fact: &TimelineFact) -> Result<(), TimelineError> {
    let branch = fact.branch_id.0.to_string();
    let sequence = fact.sequence.to_string();
    conn.write(
        "INSERT INTO timeline_fact_keys(branch_id,sequence,event_id,external_id,kind_json) VALUES(?,?,?,nullif(?,''),?)",
        &[&branch, &sequence, &fact.draft.id.0.to_string(),
          fact.draft.external_message_id.as_deref().unwrap_or(""), &encode(&fact.draft.kind)?],
    )?;
    for observer in &fact.draft.observers {
        let character = observer.to_string();
        let Some(change) = crate::timeline::domain::knowledge_change(fact, *observer) else {
            continue;
        };
        if change.reset {
            conn.write(
                "INSERT INTO timeline_resets(branch_id,sequence,character_id) VALUES(?,?,?)",
                &[&branch, &sequence, &character],
            )?;
        }
        {
            let acquired = change.acquired;
            conn.write(
                "INSERT INTO timeline_acquisitions(branch_id,sequence,character_id,event_id) VALUES(?,?,?,?)",
                &[&branch, &sequence, &character, &acquired.0.to_string()],
            )?;
        }
    }
    Ok(())
}

/// A bounded branch lineage with each ancestor cutoff; siblings never leak
/// into this read-only lookup. The transaction holds a stable branch world.
struct IndexedWorld<'a> {
    connection: &'a Connection,
    lineage: Vec<(BranchId, u64)>,
}

impl<'a> IndexedWorld<'a> {
    fn new(connection: &'a Connection, branch: BranchId) -> Result<Self, TimelineError> {
        Self::through(connection, branch, None)
    }

    fn through(
        connection: &'a Connection,
        branch: BranchId,
        through: Option<u64>,
    ) -> Result<Self, TimelineError> {
        let mut lineage = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut next = Some(branch);
        let mut ceiling = through;
        while let Some(id) = next {
            if !seen.insert(id) {
                return Err(storage("cyclic branch lineage"));
            }
            let info = branch_info(connection, id)?;
            if ceiling.is_some_and(|value| value > info.head_sequence) {
                return Err(TimelineError::Invalid("read position exceeds branch head"));
            }
            lineage.push((id, ceiling.unwrap_or(info.head_sequence)));
            ceiling = Some(info.fork_sequence);
            next = info.parent;
        }
        Ok(Self {
            connection,
            lineage,
        })
    }
    fn fact(&self, event: EventId) -> Result<TimelineFact, TimelineError> {
        for (branch, cutoff) in &self.lineage {
            let rows = self.connection.query(
                "SELECT e.fact_json FROM timeline_fact_keys k JOIN timeline_events e ON e.branch_id=k.branch_id AND e.sequence=k.sequence WHERE k.branch_id=? AND k.event_id=? AND k.sequence<=? ORDER BY k.sequence DESC LIMIT 1",
                &[&branch.0.to_string(), &event.0.to_string(), &cutoff.to_string()],
            )?;
            if let Some(row) = rows.first() {
                return decode(&row[0]);
            }
        }
        Err(TimelineError::NotFound)
    }

    fn latest_reset(&self, character: CharacterId) -> Result<u64, TimelineError> {
        Ok(self
            .max_position("timeline_resets", character, None)?
            .unwrap_or(0))
    }

    fn max_position(
        &self,
        table: &str,
        character: CharacterId,
        event: Option<EventId>,
    ) -> Result<Option<u64>, TimelineError> {
        let mut maximum = None;
        for (branch, cutoff) in &self.lineage {
            let id = branch.0.to_string();
            let ceiling = cutoff.to_string();
            let character = character.to_string();
            let rows = if let Some(event) = event {
                self.connection.query("SELECT max(sequence) FROM timeline_acquisitions WHERE branch_id=? AND character_id=? AND event_id=? AND sequence<=?",
                    &[&id,&character,&event.0.to_string(),&ceiling])?
            } else {
                debug_assert_eq!(table, "timeline_resets");
                self.connection.query("SELECT max(sequence) FROM timeline_resets WHERE branch_id=? AND character_id=? AND sequence<=?",
                    &[&id,&character,&ceiling])?
            };
            if let Some(row) = rows.first()
                && !row[0].is_empty()
            {
                let position = parse_u64(&row[0])?;
                maximum = Some(maximum.map_or(position, |old: u64| old.max(position)));
            }
        }
        Ok(maximum)
    }
}

impl WorldLookup for IndexedWorld<'_> {
    fn memories(
        &self,
        character: CharacterId,
    ) -> Result<crate::memory::CharacterMemories, TimelineError> {
        let mut book = crate::memory::CharacterMemories::default();
        let (branch, through) = self.lineage.first().ok_or(TimelineError::NotFound)?;
        for fact in read_page(
            self.connection,
            *branch,
            0,
            Some(*through),
            i64::MAX as usize,
            None,
        )? {
            if fact.draft.kind == EventKind::CharacterMemoryChanged
                && fact
                    .draft
                    .actor
                    .as_deref()
                    .and_then(crate::identity::CharacterId::parse)
                    == Some(character)
            {
                let change = serde_json::from_value::<crate::memory::MemoryChange>(
                    fact.draft
                        .payload
                        .get("change")
                        .cloned()
                        .ok_or(TimelineError::Invalid("missing memory change"))?,
                )
                .map_err(|_| TimelineError::Invalid("invalid memory change"))?;
                book.apply_committed(&change)
                    .map_err(TimelineError::Invalid)?;
            }
        }
        Ok(book)
    }
    fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError> {
        for (branch, cutoff) in &self.lineage {
            let rows = self.connection.query("SELECT kind_json FROM timeline_fact_keys WHERE branch_id=? AND event_id=? AND sequence<=? ORDER BY sequence DESC LIMIT 1",
                &[&branch.0.to_string(),&event.0.to_string(),&cutoff.to_string()])?;
            if let Some(row) = rows.first() {
                return Ok(Some(decode(&row[0])?));
            }
        }
        Ok(None)
    }
    fn has_external(&self, external: &str) -> Result<bool, TimelineError> {
        for (branch, cutoff) in &self.lineage {
            if !self.connection.query("SELECT 1 FROM timeline_fact_keys WHERE branch_id=? AND external_id=? AND sequence<=? LIMIT 1",
                &[&branch.0.to_string(),external,&cutoff.to_string()])?.is_empty() { return Ok(true); }
        }
        Ok(false)
    }
    fn knows(&self, character: CharacterId, event: EventId) -> Result<bool, TimelineError> {
        let acquired = self.max_position("timeline_acquisitions", character, Some(event))?;
        let reset = self.max_position("timeline_resets", character, None)?;
        Ok(acquired.is_some_and(|position| reset.is_none_or(|reset| position >= reset)))
    }
}

fn save_branch(
    connection: &Connection,
    store: &TimelineStore,
    branch_id: BranchId,
) -> Result<(), TimelineError> {
    let branch = store
        .persisted_branches()
        .into_iter()
        .find(|branch| branch.info.id == branch_id)
        .ok_or(TimelineError::NotFound)?;
    let info = branch.info;
    let branch_id = info.id.0.to_string();
    let session_id = info.session_id.0.to_string();
    let parent = info.parent.map(|id| id.0.to_string()).unwrap_or_default();
    let fork = info.fork_sequence.to_string();
    let head = info.head_sequence.to_string();
    let checkpoint = info
        .checkpoint
        .map(|id| id.0.to_string())
        .unwrap_or_default();
    connection.write("INSERT INTO timeline_branches(branch_id,session_id,parent_id,fork_sequence,head_sequence,checkpoint_id) VALUES (?, ?, nullif(?,''), ?, ?, nullif(?,'')) ON CONFLICT(branch_id) DO UPDATE SET head_sequence=excluded.head_sequence", &[&branch_id, &session_id, &parent, &fork, &head, &checkpoint])?;
    for table in [
        "timeline_fact_keys",
        "timeline_acquisitions",
        "timeline_resets",
    ] {
        connection.write(
            &format!("DELETE FROM {table} WHERE branch_id=?"),
            &[&branch_id],
        )?;
    }
    connection.write(
        "DELETE FROM timeline_events WHERE branch_id=?",
        &[&branch_id],
    )?;
    for fact in &branch.events {
        connection.write(
            "INSERT INTO timeline_events(branch_id,sequence,fact_json) VALUES(?, ?, ?)",
            &[&branch_id, &fact.sequence.to_string(), &encode(fact)?],
        )?;
        index_fact(connection, fact)?;
        for observer in &fact.draft.observers {
            connection.write(
                "INSERT INTO timeline_observers(branch_id,sequence,character_id) VALUES(?, ?, ?)",
                &[
                    &branch_id,
                    &fact.sequence.to_string(),
                    &observer.to_string(),
                ],
            )?;
        }
    }
    connection.write(
        "DELETE FROM timeline_receipts WHERE branch_id=?",
        &[&branch_id],
    )?;
    for (key, receipt) in &branch.receipts {
        connection.write(
            "INSERT INTO timeline_receipts(branch_id,operation_key,receipt_json) VALUES(?, ?, ?)",
            &[&branch_id, key, &encode(receipt)?],
        )?;
    }
    Ok(())
}

fn save_indexed_append(
    connection: &Connection,
    branch: BranchId,
    previous_head: u64,
    key: &str,
    receipt: &Receipt,
) -> Result<(), TimelineError> {
    let id = branch.0.to_string();
    let head = previous_head
        .checked_add(receipt.result().len() as u64)
        .ok_or(TimelineError::Invalid("sequence overflow"))?;
    connection.write(
        "UPDATE timeline_branches SET head_sequence=? WHERE branch_id=? AND head_sequence=?",
        &[&head.to_string(), &id, &previous_head.to_string()],
    )?;
    for fact in receipt.result() {
        connection.write(
            "INSERT INTO timeline_events(branch_id,sequence,fact_json) VALUES(?,?,?)",
            &[&id, &fact.sequence.to_string(), &encode(fact)?],
        )?;
        for observer in &fact.draft.observers {
            connection.write(
                "INSERT INTO timeline_observers(branch_id,sequence,character_id) VALUES(?,?,?)",
                &[&id, &fact.sequence.to_string(), &observer.to_string()],
            )?;
        }
        index_fact(connection, fact)?;
    }
    connection.write(
        "INSERT INTO timeline_receipts(branch_id,operation_key,receipt_json) VALUES(?,?,?)",
        &[&id, key, &encode(receipt)?],
    )?;
    Ok(())
}

fn read_page(
    conn: &Connection,
    branch: BranchId,
    after: u64,
    through: Option<u64>,
    limit: usize,
    observer: Option<CharacterId>,
) -> Result<Vec<TimelineFact>, TimelineError> {
    if limit == 0 {
        return Err(TimelineError::Invalid("read limit must be positive"));
    }
    let mut branches = Vec::new();
    let mut next = Some(branch);
    let mut ceiling = through;
    let mut visited = std::collections::HashSet::new();
    while let Some(id) = next {
        if !visited.insert(id) {
            return Err(storage("cyclic branch lineage"));
        }
        let rows = conn.query(
            "SELECT ifnull(parent_id,''),fork_sequence,head_sequence FROM timeline_branches WHERE branch_id=?",
            &[&id.0.to_string()],
        )?;
        let [row] = rows.as_slice() else {
            return Err(TimelineError::NotFound);
        };
        let fork = parse_u64(&row[1])?;
        let head = parse_u64(&row[2])?;
        let current = ceiling.unwrap_or(head);
        if id == branch && current > head {
            return Err(TimelineError::Invalid("read position exceeds branch head"));
        }
        branches.push((id, fork, current.min(head)));
        ceiling = Some(current.min(fork));
        next = if row[0].is_empty() {
            None
        } else {
            Some(BranchId(parse_uuid(&row[0])?))
        };
    }
    let mut results = Vec::new();
    for (id, fork, ceiling) in branches.into_iter().rev() {
        let remaining = limit - results.len();
        if remaining == 0 {
            break;
        }
        let id = id.0.to_string();
        let lower = fork.max(after).to_string();
        let ceiling = ceiling.to_string();
        let limit = remaining.to_string();
        let rows = if let Some(observer) = observer {
            conn.query(
                "SELECT e.fact_json FROM timeline_events e WHERE e.branch_id=? AND e.sequence>? AND e.sequence<=? AND EXISTS(SELECT 1 FROM timeline_observers o WHERE o.branch_id=e.branch_id AND o.sequence=e.sequence AND o.character_id=?) ORDER BY e.sequence LIMIT ?",
                &[&id, &lower, &ceiling, &observer.to_string(), &limit],
            )?
        } else {
            conn.query(
                "SELECT fact_json FROM timeline_events WHERE branch_id=? AND sequence>? AND sequence<=? ORDER BY sequence LIMIT ?",
                &[&id, &lower, &ceiling, &limit],
            )?
        };
        for row in rows {
            results.push(decode(&row[0])?);
        }
    }
    Ok(results)
}

impl SqliteTimelineRepository {
    fn append_inner(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
        guard: Option<AppendGuard>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let current_revision = revision(&connection)?;
            let info = branch_info(&connection, branch)?;
            if let Some(guard) = guard {
                let current = read_selection(&connection)?;
                if current
                    != Some(TimelineSelection {
                        branch_id: branch,
                        generation: guard.selection_generation,
                    })
                {
                    return Err(TimelineError::ConcurrentWrite);
                }
                if let Some((character, epoch, trigger)) = guard.context {
                    let world = IndexedWorld::new(&connection, branch)?;
                    if world.latest_reset(character)? != epoch {
                        return Err(TimelineError::ConcurrentWrite);
                    }
                    if let Some(event) = trigger
                        && !world.knows(character, event)?
                    {
                        return Err(TimelineError::ConcurrentWrite);
                    }
                }
            }
            if let Some(guard) = guard
                && let Some((character, expected_revision)) = guard.accepted_reply_revision
            {
                let facts = read_page(&connection, branch, 0, None, i64::MAX as usize, None)?;
                if crate::timeline::domain::accepted_reply_count(&facts, character)?
                    != expected_revision
                {
                    return Err(TimelineError::ConcurrentWrite);
                }
            }
            if key.is_empty() || key.trim() != key {
                return Err(TimelineError::Invalid(
                    "operation key must be canonical and nonempty",
                ));
            }
            if !command.is_object() {
                return Err(TimelineError::Invalid(
                    "operation command must be an object",
                ));
            }
            let rows = connection.query(
                "SELECT receipt_json FROM timeline_receipts WHERE branch_id=? AND operation_key=?",
                &[&branch.0.to_string(), key],
            )?;
            if let Some(row) = rows.first() {
                let receipt: Receipt = decode(&row[0])?;
                return if receipt.matches(&command, &drafts) {
                    Ok(receipt.result().to_vec())
                } else {
                    Err(TimelineError::Conflict)
                };
            }
            if info.head_sequence != expected_head {
                return Err(TimelineError::ConcurrentWrite);
            }
            let world = IndexedWorld::new(&connection, branch)?;
            let facts = TimelineStore::validate_batch(info, expected_head, &drafts, &world)?;
            let receipt = Receipt::new(command, drafts, facts.clone());
            save_indexed_append(&connection, branch, expected_head, key, &receipt)?;
            advance_revision(&connection, current_revision)?;
            Ok(facts)
        })();
        match result {
            Ok(facts) => {
                connection.exec("COMMIT")?;
                Ok(facts)
            }
            Err(error) => {
                connection.exec("ROLLBACK")?;
                Err(error)
            }
        }
    }
}

impl TimelineRepository for SqliteTimelineRepository {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        match branch_info(&self.connection()?, root) {
            Ok(info) if info.parent.is_none() && info.session_id == session => return Ok(info),
            Ok(_) => return Err(TimelineError::Conflict),
            Err(TimelineError::NotFound) => {}
            Err(error) => return Err(error),
        }
        self.with_write(|connection, store| {
            let already_exists = store.branch(root).is_ok();
            let info = store.create_session(session, root)?;
            if !already_exists {
                save_branch(connection, store, root)?;
            }
            Ok(info)
        })
    }
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError> {
        branch_info(&self.connection()?, id)
    }
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN")?;
        let result = read_page(&connection, id, 0, None, i64::MAX as usize, None);
        connection.exec("ROLLBACK")?;
        result
    }
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError> {
        self.read_snapshot(|connection| IndexedWorld::new(connection, branch)?.fact(id))
    }
    fn read(
        &self,
        branch: BranchId,
        after: u64,
        through: Option<u64>,
        limit: usize,
        observer: Option<CharacterId>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        let conn = self.connection()?;
        conn.exec("BEGIN")?;
        let result = read_page(&conn, branch, after, through, limit, observer);
        conn.exec("ROLLBACK")?;
        result
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        self.append_inner(branch, expected_head, key, command, drafts, None)
    }
    fn append_guarded(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
        guard: AppendGuard,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        self.append_inner(branch, expected_head, key, command, drafts, Some(guard))
    }
    fn context_epoch_at(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<u64, TimelineError> {
        self.read_snapshot(|connection| {
            IndexedWorld::through(connection, branch, through)?.latest_reset(character)
        })
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        self.with_write(|connection, store| {
            let already_exists = store.branch(child).is_ok();
            let info = store.fork(parent, checkpoint, child)?;
            if !already_exists {
                save_branch(connection, store, child)?;
            }
            Ok(info)
        })
    }
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN")?;
        let result = read_page(&connection, branch, 0, through, i64::MAX as usize, None)
            .map(|facts| crate::timeline::domain::character_view(&facts, character));
        connection.exec("ROLLBACK")?;
        result
    }
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError> {
        self.read_snapshot(|connection| {
            IndexedWorld::through(connection, branch, through)?.knows(character, event)
        })
    }
}

fn read_selection(connection: &Connection) -> Result<Option<TimelineSelection>, TimelineError> {
    let rows = connection.query(
        "SELECT branch_id, generation FROM timeline_active_selection WHERE slot='core'",
        &[],
    )?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => Ok(Some(TimelineSelection {
            branch_id: BranchId(parse_uuid(&row[0])?),
            generation: parse_u64(&row[1])?,
        })),
        _ => Err(storage("duplicate active timeline selection")),
    }
}

impl TimelineSelectionRepository for SqliteTimelineRepository {
    fn selection(&self) -> Result<Option<TimelineSelection>, TimelineError> {
        read_selection(&self.connection()?)
    }

    fn select_branch(
        &mut self,
        branch: BranchId,
        expected: Option<TimelineSelection>,
    ) -> Result<TimelineSelection, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            branch_info(&connection, branch)?;
            let current = read_selection(&connection)?;
            if current != expected {
                return Err(TimelineError::ConcurrentWrite);
            }
            let generation = current
                .map_or(Some(1), |value| value.generation.checked_add(1))
                .filter(|value| *value <= i64::MAX as u64)
                .ok_or(TimelineError::Invalid("selection generation overflow"))?;
            connection.write(
                "INSERT INTO timeline_active_selection(slot,branch_id,generation) VALUES('core', ?, ?) ON CONFLICT(slot) DO UPDATE SET branch_id=excluded.branch_id,generation=excluded.generation",
                &[&branch.0.to_string(), &generation.to_string()],
            )?;
            Ok(TimelineSelection {
                branch_id: branch,
                generation,
            })
        })();
        if result.is_ok() {
            connection.exec("COMMIT")?;
        } else {
            connection.exec("ROLLBACK")?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestDb(PathBuf);
    impl TestDb {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("neuromita-sqlite-failure-{}.db", Uuid::new_v4())),
            )
        }
    }
    impl Drop for TestDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
            }
        }
    }

    #[test]
    fn reopen_existing_session_does_not_change_timeline_revision() {
        let db = TestDb::new();
        let session = SessionId::new();
        let branch = BranchId::new();
        let mut original = SqliteTimelineRepository::open(&db.0).unwrap();
        original.create_session(session, branch).unwrap();
        original
            .append(
                branch,
                0,
                "event",
                json!({}),
                vec![EventDraft::new(
                    crate::timeline::domain::EventKind::UnityEvent,
                )],
            )
            .unwrap();
        let before = revision(&Connection::open(&db.0).unwrap()).unwrap();
        let mut reopened = SqliteTimelineRepository::open(&db.0).unwrap();
        assert_eq!(
            reopened
                .create_session(session, branch)
                .unwrap()
                .head_sequence,
            1
        );
        assert_eq!(revision(&Connection::open(&db.0).unwrap()).unwrap(), before);
        assert_eq!(reopened.world(branch).unwrap().len(), 1);
    }

    #[test]
    fn indexed_append_recovers_from_sqlite_failure() {
        let db = TestDb::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        repo.append(
            branch,
            0,
            "first",
            json!({}),
            vec![EventDraft::new(
                crate::timeline::domain::EventKind::UnityEvent,
            )],
        )
        .unwrap();
        let conn = Connection::open(&db.0).unwrap();
        conn.exec("CREATE TRIGGER timeline_test_warm_fail BEFORE INSERT ON timeline_events BEGIN SELECT RAISE(ABORT, 'injected warmed failure'); END").unwrap();
        assert!(matches!(
            repo.append(
                branch,
                1,
                "retry",
                json!({}),
                vec![EventDraft::new(
                    crate::timeline::domain::EventKind::UnityEvent
                )]
            ),
            Err(TimelineError::Storage(_))
        ));
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);
        conn.exec("DROP TRIGGER timeline_test_warm_fail").unwrap();
        repo.append(
            branch,
            1,
            "retry",
            json!({}),
            vec![EventDraft::new(
                crate::timeline::domain::EventKind::UnityEvent,
            )],
        )
        .unwrap();
        assert_eq!(repo.world(branch).unwrap().len(), 2);
    }

    #[test]
    fn version_one_database_migrates_indices_without_changing_facts() {
        let db = TestDb::new();
        let branch = BranchId::new();
        let session = SessionId::new();
        let mut first = SqliteTimelineRepository::open(&db.0).unwrap();
        first.create_session(session, branch).unwrap();
        let mut secret = EventDraft::new(EventKind::UnityEvent);
        secret.observers.insert(CharacterId::Crazy);
        secret.external_message_id = Some("source:legacy".into());
        let id = secret.id;
        let facts = first
            .append(branch, 0, "legacy", json!({}), vec![secret])
            .unwrap();
        drop(first);

        let conn = Connection::open(&db.0).unwrap();
        conn.exec("UPDATE timeline_schema SET version=1").unwrap();
        for table in [
            "timeline_fact_keys",
            "timeline_acquisitions",
            "timeline_resets",
        ] {
            conn.write(&format!("DELETE FROM {table}"), &[]).unwrap();
        }
        drop(conn);

        let mut reopened = SqliteTimelineRepository::open(&db.0).unwrap();
        assert_eq!(reopened.world(branch).unwrap(), facts);
        assert_eq!(
            reopened.knows(branch, CharacterId::Crazy, id, None),
            Ok(true)
        );
        assert_eq!(
            reopened.knows(branch, CharacterId::Kind, id, None),
            Ok(false)
        );
        let mut duplicate = EventDraft::new(EventKind::UnityEvent);
        duplicate.external_message_id = Some("source:legacy".into());
        assert_eq!(
            reopened.append(branch, 1, "collision", json!({}), vec![duplicate]),
            Err(TimelineError::Conflict)
        );
        assert_eq!(
            SqliteTimelineRepository::open(&db.0)
                .unwrap()
                .world(branch)
                .unwrap(),
            facts
        );
    }

    #[test]
    fn sqlite_failure_after_head_update_rolls_back_everything() {
        let db = TestDb::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        let conn = Connection::open(&db.0).unwrap();
        conn.exec("CREATE TRIGGER timeline_test_fail BEFORE INSERT ON timeline_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END").unwrap();
        let draft = EventDraft::new(crate::timeline::domain::EventKind::UnityEvent);
        assert!(matches!(
            repo.append(branch, 0, "operation", json!({}), vec![draft.clone()]),
            Err(TimelineError::Storage(_))
        ));
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 0);
        assert!(repo.world(branch).unwrap().is_empty());
        conn.exec("DROP TRIGGER timeline_test_fail").unwrap();
        assert!(
            repo.append(branch, 0, "operation", json!({}), vec![draft])
                .is_ok()
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);
    }
    #[test]
    fn guarded_append_is_atomic_with_external_selection_change() {
        let db = TestDb::new();
        let branch = BranchId::new();
        let mut first = SqliteTimelineRepository::open(&db.0).unwrap();
        first.create_session(SessionId::new(), branch).unwrap();
        let selection = first.select_branch(branch, None).unwrap();
        let mut other = SqliteTimelineRepository::open(&db.0).unwrap();
        let changed = other.select_branch(branch, Some(selection)).unwrap();
        let draft = EventDraft::new(EventKind::UnityEvent);
        assert_eq!(
            first.append_guarded(
                branch,
                0,
                "guarded:1",
                json!({}),
                vec![draft.clone()],
                AppendGuard {
                    selection_generation: selection.generation,
                    context: None,
                    accepted_reply_revision: None,
                },
            ),
            Err(TimelineError::ConcurrentWrite),
        );
        assert_eq!(first.branch(branch).unwrap().head_sequence, 0);
        assert!(
            first
                .append_guarded(
                    branch,
                    0,
                    "guarded:1",
                    json!({}),
                    vec![draft],
                    AppendGuard {
                        selection_generation: changed.generation,
                        context: None,
                        accepted_reply_revision: None,
                    },
                )
                .is_ok()
        );
        assert_eq!(first.branch(branch).unwrap().head_sequence, 1);
        assert_eq!(first.selection().unwrap(), Some(changed));
    }

    #[test]
    fn guarded_append_checks_reset_and_trigger_at_commit_time() {
        let db = TestDb::new();
        let branch = BranchId::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        let selected = repo.select_branch(branch, None).unwrap();
        let mut source = EventDraft::new(EventKind::PlayerMessage);
        source.actor = Some("player".into());
        source.observers.insert(CharacterId::Crazy);
        let source_id = source.id;
        repo.append(branch, 0, "source", json!({}), vec![source])
            .unwrap();
        let guard = AppendGuard {
            selection_generation: selected.generation,
            context: Some((CharacterId::Crazy, 0, Some(source_id))),
            accepted_reply_revision: None,
        };
        let reply = || {
            let mut fact = EventDraft::new(EventKind::CharacterMessage);
            fact.actor = Some(CharacterId::Crazy.to_string());
            fact.observers.insert(CharacterId::Crazy);
            fact.links.push(crate::timeline::domain::CausalLink {
                relation: crate::timeline::domain::CausalRelation::ReplyTo,
                event_id: source_id,
            });
            fact
        };
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        repo.append(branch, 1, "reset", json!({}), vec![reset])
            .unwrap();
        assert_eq!(repo.context_epoch(branch, CharacterId::Crazy), Ok(2));
        assert_eq!(
            repo.knows(branch, CharacterId::Crazy, source_id, Some(1)),
            Ok(true)
        );
        assert_eq!(
            repo.knows(branch, CharacterId::Crazy, source_id, None),
            Ok(false)
        );
        assert_eq!(
            repo.append_guarded(branch, 2, "reply", json!({}), vec![reply()], guard),
            Err(TimelineError::ConcurrentWrite),
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 2);
        let still_unknown = AppendGuard {
            context: Some((CharacterId::Crazy, 2, Some(source_id))),
            ..guard
        };
        assert_eq!(
            repo.append_guarded(branch, 2, "reply", json!({}), vec![reply()], still_unknown),
            Err(TimelineError::ConcurrentWrite),
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 2);
    }

    #[test]
    fn guarded_replay_never_bypasses_stale_selection() {
        let db = TestDb::new();
        let branch = BranchId::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        let selection = repo.select_branch(branch, None).unwrap();
        let draft = EventDraft::new(EventKind::UnityEvent);
        let guard = AppendGuard {
            selection_generation: selection.generation,
            context: None,
            accepted_reply_revision: None,
        };
        let stored = repo
            .append_guarded(branch, 0, "receipt", json!({}), vec![draft.clone()], guard)
            .unwrap();
        let mut other = SqliteTimelineRepository::open(&db.0).unwrap();
        other.select_branch(branch, Some(selection)).unwrap();
        assert_eq!(
            repo.append_guarded(branch, 0, "receipt", json!({}), vec![draft], guard),
            Err(TimelineError::ConcurrentWrite),
        );
        assert_eq!(repo.world(branch).unwrap(), stored);
    }
}
