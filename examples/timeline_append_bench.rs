use std::time::Instant;

use neuromita::timeline::domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore};
use serde_json::json;

fn main() {
    let counts: Vec<usize> = std::env::args()
        .skip(1)
        .map(|arg| {
            arg.parse::<usize>()
                .expect("event count must be an integer")
        })
        .collect();
    let counts = if counts.is_empty() {
        vec![1_000, 4_000, 8_000]
    } else {
        counts
    };
    println!("events,append_ms,microseconds_per_event");
    for count in counts {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();
        let started = Instant::now();
        for index in 0..count {
            store
                .append(
                    branch,
                    index as u64,
                    &format!("event:{index}"),
                    json!({"sequence": index}),
                    vec![EventDraft::new(EventKind::UnityEvent)],
                )
                .unwrap();
        }
        let ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(store.branch(branch).unwrap().head_sequence, count as u64);
        println!("{count},{ms:.3},{:.3}", ms * 1_000.0 / count as f64);
    }
}
