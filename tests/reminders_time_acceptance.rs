use neuromita::reminders::{
    AmbiguousPolicy, LocalDateTime, LocalTimePolicy, NonexistentPolicy, TimeZone,
};

fn resolve(zone: &str, local: &str, policy: LocalTimePolicy) -> String {
    TimeZone::from_id(zone)
        .unwrap()
        .resolve(LocalDateTime::parse(local).unwrap(), policy)
        .unwrap()
        .to_rfc3339()
}

#[test]
fn overlap_policy_selects_distinct_instants_for_the_same_wall_clock() {
    let early = LocalTimePolicy {
        ambiguous: AmbiguousPolicy::Earliest,
        nonexistent: NonexistentPolicy::Reject,
    };
    let late = LocalTimePolicy {
        ambiguous: AmbiguousPolicy::Latest,
        ..early
    };
    assert_eq!(
        resolve("Europe/Berlin", "2026-10-25T02:30:00", early),
        "2026-10-25T00:30:00Z"
    );
    assert_eq!(
        resolve("Europe/Berlin", "2026-10-25T02:30:00", late),
        "2026-10-25T01:30:00Z"
    );
    assert_eq!(
        resolve("Australia/Lord_Howe", "2026-04-05T01:45:00", early),
        "2026-04-04T14:45:00Z"
    );
    assert_eq!(
        resolve("Australia/Lord_Howe", "2026-04-05T01:45:00", late),
        "2026-04-04T15:15:00Z"
    );
}

#[test]
fn gap_policy_handles_half_hour_and_skipped_day_transitions() {
    for (zone, local, expected) in [
        (
            "Europe/Berlin",
            "2026-03-29T02:30:37",
            "2026-03-29T01:00:00Z",
        ),
        (
            "Australia/Lord_Howe",
            "2026-10-04T02:15:37",
            "2026-10-03T15:30:00Z",
        ),
        (
            "Pacific/Apia",
            "2011-12-30T00:00:37",
            "2011-12-30T10:00:00Z",
        ),
    ] {
        let tz = TimeZone::from_id(zone).unwrap();
        let time = LocalDateTime::parse(local).unwrap();
        assert!(tz.resolve(time, LocalTimePolicy::STRICT).is_err());
        assert_eq!(resolve(zone, local, LocalTimePolicy::LENIENT), expected);
    }
}

#[test]
fn calendar_day_repeat_preserves_wall_clock_across_twenty_three_and_twenty_five_hour_days() {
    let zone = TimeZone::from_id("Europe/Berlin").unwrap();
    for (before, after, elapsed_hours) in [
        ("2026-03-28T09:00:00", "2026-03-29T07:00:00Z", 23),
        ("2026-10-24T09:00:00", "2026-10-25T08:00:00Z", 25),
    ] {
        let local = LocalDateTime::parse(before).unwrap();
        let start = zone.resolve(local, LocalTimePolicy::STRICT).unwrap();
        let next = zone
            .next_local_days(local, 1, LocalTimePolicy::STRICT)
            .unwrap();
        assert_eq!(next.to_rfc3339(), after);
        assert_eq!(next.duration_since(start).as_secs(), elapsed_hours * 3600);
    }
}

#[test]
fn zone_names_and_invalid_calendar_values_are_rejected() {
    assert!(TimeZone::from_id("Europe/Imaginary").is_err());
    for input in [
        "2026-02-29T12:00:00",
        "2026-04-31T12:00:00",
        "2026-12-01T24:00:00",
    ] {
        assert!(LocalDateTime::parse(input).is_err(), "{input}");
    }
    assert_eq!(
        resolve(
            "Asia/Kathmandu",
            "2026-11-03T12:00:00",
            LocalTimePolicy::STRICT
        ),
        "2026-11-03T06:15:00Z"
    );
}
