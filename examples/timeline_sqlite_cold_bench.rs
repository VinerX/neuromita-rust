#[cfg(feature = "sqlite-system")]
fn main() {
    use neuromita::timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        storage::SqliteTimelineRepository,
    };
    use serde_json::json;
    use std::time::Instant;

    let sizes = std::env::args()
        .skip(1)
        .map(|arg| arg.parse::<u32>().expect("expected a positive integer"))
        .collect::<Vec<_>>();
    let sizes = if sizes.is_empty() {
        vec![200, 1000, 3000]
    } else {
        sizes
    };
    println!("events,first_reopen_append_ms,second_reopen_append_ms,head");
    for size in sizes {
        let path = std::env::temp_dir().join(format!("neuromita-cold-{}.db", uuid::Uuid::new_v4()));
        let branch = BranchId::new();
        let mut repo = SqliteTimelineRepository::open(&path).expect("open");
        repo.create_session(SessionId::new(), branch)
            .expect("session");
        let mut drafts = Vec::new();
        for _ in 0..size {
            drafts.push(EventDraft::new(EventKind::UnityEvent));
        }
        repo.append(branch, 0, "initial-batch", json!({}), drafts)
            .expect("initial");
        drop(repo);
        let mut values = Vec::new();
        for index in 0..2 {
            let mut repo = SqliteTimelineRepository::open(&path).expect("reopen");
            let head = repo.branch(branch).expect("branch").head_sequence;
            let start = Instant::now();
            repo.append(
                branch,
                head,
                &format!("cold:{index}"),
                json!({}),
                vec![EventDraft::new(EventKind::UnityEvent)],
            )
            .expect("cold append");
            values.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!("{size},{:.3},{:.3},{}", values[0], values[1], size + 2);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}

#[cfg(not(feature = "sqlite-system"))]
fn main() {
    eprintln!("Run with --features sqlite-system");
}
