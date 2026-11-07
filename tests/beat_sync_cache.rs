use neuromita::music::{
    BeatBackend, BeatCache, BeatCacheError, BeatCacheKey, BeatPoint, BeatTimeline, CacheLookup,
};
use std::fs;

fn fixture() -> BeatTimeline {
    BeatTimeline {
        beats: vec![
            BeatPoint {
                time_seconds: 0.25,
                confidence: 0.4,
            },
            BeatPoint {
                time_seconds: 0.50,
                confidence: 0.9,
            },
            BeatPoint {
                time_seconds: 0.75,
                confidence: 0.7,
            },
        ],
        duration_seconds: 1.0,
        sample_rate_hz: 44100,
        method: "dsp".into(),
        bpm: Some(120.0),
    }
}

fn key(bytes: &[u8], revision: &str) -> BeatCacheKey {
    BeatCacheKey::for_audio_bytes(bytes, BeatBackend::Auto, revision, "wav-v1", "analysis-v1")
}

struct TempDirectory(std::path::PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("neuromita-beat-cache-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn same_audio_and_configuration_hits_after_reopening_cache() {
    let tmp = TempDirectory::new();
    let request = key(b"fixed fixture bytes", "backend-v1");
    let cache = BeatCache::new(&tmp.0);
    assert_eq!(cache.lookup(&request).unwrap(), CacheLookup::Miss);
    cache.store(&request, &fixture()).unwrap();
    assert_eq!(
        BeatCache::new(&tmp.0).lookup(&request).unwrap(),
        CacheLookup::Hit(fixture())
    );
}

#[test]
fn changing_bytes_or_backend_or_revision_causes_cache_miss() {
    let tmp = TempDirectory::new();
    let cache = BeatCache::new(&tmp.0);
    let initial = key(b"fixture", "v1");
    cache.store(&initial, &fixture()).unwrap();
    assert_eq!(
        cache.lookup(&key(b"different", "v1")).unwrap(),
        CacheLookup::Miss
    );
    assert_eq!(
        cache.lookup(&key(b"fixture", "v2")).unwrap(),
        CacheLookup::Miss
    );
    let mut different_backend = initial.clone();
    different_backend.backend = BeatBackend::Dsp;
    assert_eq!(cache.lookup(&different_backend).unwrap(), CacheLookup::Miss);
    let mut different_settings = initial.clone();
    different_settings.analysis_settings_revision = "analysis-v2".into();
    assert_eq!(
        cache.lookup(&different_settings).unwrap(),
        CacheLookup::Miss
    );
    let mut different_decoder = initial.clone();
    different_decoder.decoder_revision = "decoder-v2".into();
    assert_eq!(cache.lookup(&different_decoder).unwrap(), CacheLookup::Miss);
}

#[test]
fn confidence_is_post_filter_and_does_not_change_stored_timeline() {
    let tmp = TempDirectory::new();
    let cache = BeatCache::new(&tmp.0);
    let request = key(b"fixture", "v1");
    cache.store(&request, &fixture()).unwrap();
    let CacheLookup::Hit(timeline) = cache.lookup(&request).unwrap() else {
        panic!("expected hit");
    };
    let filtered = timeline.with_min_confidence(0.8).unwrap();
    assert_eq!(filtered.beats.len(), 1);
    assert_eq!(filtered.beats[0].time_seconds, 0.5);
    assert_eq!(filtered.method, "dsp");
    assert_eq!(cache.lookup(&request).unwrap(), CacheLookup::Hit(fixture()));
    assert!(matches!(
        timeline.with_min_confidence(f32::NAN),
        Err(BeatCacheError::InvalidThreshold)
    ));
}

#[test]
fn damaged_cache_is_never_returned_as_valid_analysis() {
    let tmp = TempDirectory::new();
    let cache = BeatCache::new(&tmp.0);
    let request = key(b"fixture", "v1");
    cache.store(&request, &fixture()).unwrap();
    let path = fs::read_dir(cache.root())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&path, b"{ corrupt json ").unwrap();
    assert_eq!(cache.lookup(&request).unwrap(), CacheLookup::Corrupt);
    cache.store(&request, &fixture()).unwrap();
    assert_eq!(cache.lookup(&request).unwrap(), CacheLookup::Hit(fixture()));
}

#[test]
fn malformed_beats_cannot_be_cached() {
    let tmp = TempDirectory::new();
    let cache = BeatCache::new(&tmp.0);
    let request = key(b"fixture", "v1");
    let mut bad = fixture();
    bad.beats[1].time_seconds = 0.1;
    assert!(matches!(
        cache.store(&request, &bad),
        Err(BeatCacheError::InvalidTimeline)
    ));
    bad = fixture();
    bad.beats[0].confidence = f32::NAN;
    assert!(matches!(
        cache.store(&request, &bad),
        Err(BeatCacheError::InvalidTimeline)
    ));
    bad = fixture();
    bad.beats[2].time_seconds = 1.1;
    assert!(matches!(
        cache.store(&request, &bad),
        Err(BeatCacheError::InvalidTimeline)
    ));
    assert_eq!(cache.lookup(&request).unwrap(), CacheLookup::Miss);
}

#[test]
fn invalid_identity_and_threshold_are_rejected() {
    let tmp = TempDirectory::new();
    let cache = BeatCache::new(&tmp.0);
    let mut request = key(b"fixture", "v1");
    request.backend_revision = "".into();
    assert!(matches!(
        cache.lookup(&request),
        Err(BeatCacheError::InvalidKey)
    ));
    assert!(matches!(
        fixture().with_min_confidence(-0.1),
        Err(BeatCacheError::InvalidThreshold)
    ));
}
