use std::{fs, time::Duration};

use codex_micro_chroma::{
    audio::AudioFeatureFrame,
    color::Rgb,
    lighting::LightingScene,
    media::TrackSnapshot,
    protocol::LightingEffect,
    telemetry::{TrackLogger, TrackUsageTracker},
};

fn snapshot(position: f64, duration: f64) -> TrackSnapshot {
    TrackSnapshot {
        is_playing: Some(true),
        title: Some("Telemetry Song".into()),
        artist: Some("Test Artist".into()),
        album: Some("Test Album".into()),
        bundle_id: Some("com.apple.Music".into()),
        elapsed_time: Some(position),
        duration: Some(duration),
        playback_rate: Some(1.0),
        artwork: None,
        artwork_signature: Some(42),
    }
}

#[test]
fn complete_song_summary_tracks_effect_seconds_and_percentages() {
    let mut tracker = TrackUsageTracker::new("track", &snapshot(0.0, 180.0));
    for _ in 0..60 {
        tracker.observe_scene(LightingEffect::Breath, Duration::from_secs(1));
    }
    for _ in 0..60 {
        tracker.observe_scene(LightingEffect::Snake, Duration::from_secs(1));
    }
    for _ in 0..60 {
        tracker.observe_scene(LightingEffect::Gradient, Duration::from_secs(1));
    }
    tracker.update_snapshot(&snapshot(180.0, 180.0));

    let summary = tracker.summary("track_changed");
    assert!(summary.complete_track);
    assert_eq!(summary.transitions, 3);
    assert_eq!(summary.observed_seconds, 180.0);
    for effect in [
        LightingEffect::Breath,
        LightingEffect::Snake,
        LightingEffect::Gradient,
    ] {
        let usage = summary
            .effect_usage
            .iter()
            .find(|usage| usage.effect == effect)
            .unwrap();
        assert_eq!(usage.seconds, 60.0);
        assert!((usage.percent - 100.0 / 3.0).abs() < 0.001);
    }
}

#[test]
fn starting_mid_song_is_reported_as_incomplete() {
    let mut tracker = TrackUsageTracker::new("track", &snapshot(30.0, 180.0));
    for _ in 0..150 {
        tracker.observe_scene(LightingEffect::Breath, Duration::from_secs(1));
    }
    tracker.update_snapshot(&snapshot(180.0, 180.0));

    let summary = tracker.summary("track_changed");
    assert!(!summary.complete_track);
    assert_eq!(summary.started_at_position_seconds, Some(30.0));
}

#[test]
fn same_track_position_restart_finishes_one_crossfaded_playthrough() {
    let mut tracker = TrackUsageTracker::new("track", &snapshot(0.2, 207.0));
    for _ in 0..180 {
        tracker.observe_scene(LightingEffect::Breath, Duration::from_secs(1));
    }
    assert!(!tracker.update_snapshot(&snapshot(191.0, 207.0)));
    assert!(tracker.update_snapshot(&snapshot(0.4, 207.0)));

    let summary = tracker.summary("position_restarted");
    assert!(summary.complete_track);
    assert_eq!(summary.ended_at_position_seconds, Some(191.0));
    assert_eq!(summary.observed_seconds, 180.0);
}

#[test]
fn early_manual_restart_is_not_reported_as_a_complete_track() {
    let mut tracker = TrackUsageTracker::new("track", &snapshot(0.0, 207.0));
    for _ in 0..60 {
        tracker.observe_scene(LightingEffect::Breath, Duration::from_secs(1));
    }
    assert!(!tracker.update_snapshot(&snapshot(60.0, 207.0)));
    assert!(tracker.update_snapshot(&snapshot(0.0, 207.0)));

    assert!(!tracker.summary("position_restarted").complete_track);
}

#[test]
fn logger_flushes_start_transition_and_summary_as_json_lines() {
    let directory = std::env::temp_dir().join(format!(
        "codex-micro-chroma-telemetry-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    let mut logger = TrackLogger::new(&directory).unwrap();
    let snapshot = snapshot(0.0, 180.0);
    let path = logger.start_track("track", &snapshot).unwrap();
    let scene = LightingScene {
        effect: LightingEffect::Snake,
        color: Rgb::new(20, 80, 220),
        brightness: 0.8,
        speed: 0.9,
        magic: 0.6,
    };
    assert!(logger
        .observe_scene(
            scene,
            AudioFeatureFrame::default(),
            Duration::from_millis(250)
        )
        .unwrap());
    let finished = logger.finish("stopped").unwrap().unwrap();

    assert_eq!(finished.path, path);
    let records = fs::read_to_string(&path).unwrap();
    let lines = records.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("\"event\":\"track_start\""));
    assert!(lines[1].contains("\"event\":\"effect_transition\""));
    assert!(lines[1].contains("\"effect\":\"snake\""));
    assert!(lines[2].contains("\"event\":\"track_summary\""));

    fs::remove_dir_all(directory).unwrap();
}
