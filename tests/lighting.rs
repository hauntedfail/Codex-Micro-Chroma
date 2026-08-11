use std::collections::BTreeSet;
use std::time::Duration;

use codex_micro_chroma::{
    audio::AudioFeatureFrame,
    color::Rgb,
    lighting::{LightingComposer, LightingScene},
    protocol::LightingEffect,
};

fn frame() -> AudioFeatureFrame {
    AudioFeatureFrame {
        loudness: 0.45,
        peak: 0.65,
        bass: 0.25,
        mid: 0.55,
        treble: 0.20,
        centroid: 0.35,
        flatness: 0.15,
        flux: 0.15,
        onset: 0.10,
        pan: 0.0,
        stereo_width: 0.20,
        pulse: 0.10,
        tempo_bpm: 0.0,
        tempo_confidence: 0.0,
        silent: false,
        timestamp_seconds: 0.0,
    }
}

fn settle(composer: &mut LightingComposer, input: AudioFeatureFrame) -> LightingScene {
    let mut scene = composer.update(input, Duration::from_millis(250));
    for _ in 0..24 {
        scene = composer.update(input, Duration::from_millis(250));
    }
    scene
}

#[test]
fn every_effect_has_a_semantic_audio_scene() {
    let color = Rgb::new(40, 100, 220);

    let mut composer = LightingComposer::new(color);
    let mut quiet = frame();
    quiet.loudness = 0.03;
    quiet.peak = 0.05;
    quiet.flux = 0.01;
    quiet.onset = 0.01;
    assert_eq!(
        settle(&mut composer, quiet).effect,
        LightingEffect::ShallowBreath
    );

    let mut composer = LightingComposer::new(color);
    let sustained = frame();
    assert_eq!(
        settle(&mut composer, sustained).effect,
        LightingEffect::Breath
    );

    let mut composer = LightingComposer::new(color);
    let mut wide = frame();
    wide.stereo_width = 0.95;
    wide.centroid = 0.55;
    assert_eq!(settle(&mut composer, wide).effect, LightingEffect::Gradient);

    let mut composer = LightingComposer::new(color);
    let mut rhythmic = frame();
    rhythmic.bass = 0.95;
    rhythmic.onset = 0.85;
    rhythmic.flux = 0.70;
    rhythmic.pulse = 0.95;
    rhythmic.tempo_confidence = 0.90;
    rhythmic.tempo_bpm = 128.0;
    assert_eq!(
        settle(&mut composer, rhythmic).effect,
        LightingEffect::Snake
    );

    let mut composer = LightingComposer::new(color);
    let mut climax = frame();
    climax.loudness = 0.98;
    climax.peak = 1.0;
    climax.bass = 0.8;
    climax.mid = 0.8;
    climax.treble = 0.9;
    climax.centroid = 0.75;
    climax.flatness = 0.55;
    climax.flux = 0.98;
    climax.onset = 0.98;
    climax.stereo_width = 0.85;
    climax.pulse = 0.75;
    assert_eq!(
        settle(&mut composer, climax).effect,
        LightingEffect::Rainbow
    );

    let mut composer = LightingComposer::new(color);
    let mut direct = frame();
    direct.mid = 0.95;
    direct.bass = 0.05;
    direct.treble = 0.10;
    direct.stereo_width = 0.02;
    direct.pulse = 0.0;
    direct.tempo_confidence = 0.0;
    direct.flatness = 0.35;
    assert_eq!(settle(&mut composer, direct).effect, LightingEffect::Solid);

    let mut composer = LightingComposer::new(color);
    let silent = AudioFeatureFrame {
        silent: true,
        ..AudioFeatureFrame::default()
    };
    assert_eq!(settle(&mut composer, silent).effect, LightingEffect::Off);
}

#[test]
fn brightness_follows_relative_loudness_without_exceeding_device_range() {
    let mut composer = LightingComposer::new(Rgb::new(120, 80, 200));
    let mut quiet = frame();
    quiet.loudness = 0.05;
    quiet.peak = 0.08;
    let quiet_scene = settle(&mut composer, quiet);

    let mut loud = frame();
    loud.loudness = 0.9;
    loud.peak = 1.0;
    let loud_scene = settle(&mut composer, loud);

    assert!(loud_scene.brightness > quiet_scene.brightness);
    for value in [loud_scene.brightness, loud_scene.speed, loud_scene.magic] {
        assert!((0.0..=1.0).contains(&value));
    }
}

#[test]
fn short_silence_does_not_immediately_turn_the_ring_off() {
    let mut composer = LightingComposer::new(Rgb::new(12, 140, 220));
    let active = settle(&mut composer, frame());
    assert_ne!(active.effect, LightingEffect::Off);

    let silent = AudioFeatureFrame {
        silent: true,
        ..AudioFeatureFrame::default()
    };
    let scene = composer.update(silent, Duration::from_millis(100));
    assert_ne!(scene.effect, LightingEffect::Off);
}

#[test]
fn percussive_burst_is_held_long_enough_to_reach_snake() {
    let mut composer = LightingComposer::new(Rgb::new(220, 80, 40));
    let sustained = frame();
    for _ in 0..16 {
        composer.update(sustained, Duration::from_millis(250));
    }

    let mut burst = sustained;
    burst.loudness = 0.12;
    burst.peak = 0.35;
    burst.bass = 0.98;
    burst.flux = 0.92;
    burst.onset = 0.98;
    burst.pulse = 0.95;
    burst.tempo_confidence = 0.90;
    composer.update(burst, Duration::from_millis(100));

    let mut reached_snake = false;
    let mut observed = Vec::new();
    for _ in 0..16 {
        let scene = composer.update(sustained, Duration::from_millis(100));
        observed.push(scene.effect);
        reached_snake |= scene.effect == LightingEffect::Snake;
    }
    assert!(
        reached_snake,
        "a real beat is shorter than the effect dwell; observed {observed:?}"
    );
}

#[test]
fn a_long_musical_phrase_does_not_remain_on_one_pattern_forever() {
    let mut composer = LightingComposer::new(Rgb::new(40, 160, 220));
    let sustained = frame();
    let mut effects = BTreeSet::new();

    for _ in 0..160 {
        let scene = composer.update(sustained, Duration::from_millis(250));
        effects.insert(scene.effect.code());
    }

    assert!(
        effects.len() >= 2,
        "phrase-level variety should prevent a single effect from monopolizing playback"
    );
}

#[test]
fn effects_receive_visibly_distinct_motion_profiles() {
    let color = Rgb::new(180, 60, 220);

    let mut solid_composer = LightingComposer::new(color);
    let mut direct = frame();
    direct.mid = 0.95;
    direct.bass = 0.05;
    direct.stereo_width = 0.02;
    direct.pulse = 0.0;
    direct.tempo_confidence = 0.0;
    direct.flatness = 0.35;
    let solid = settle(&mut solid_composer, direct);

    let mut snake_composer = LightingComposer::new(color);
    let mut rhythmic = frame();
    rhythmic.bass = 0.95;
    rhythmic.onset = 0.85;
    rhythmic.flux = 0.70;
    rhythmic.pulse = 0.95;
    rhythmic.tempo_confidence = 0.90;
    let snake = settle(&mut snake_composer, rhythmic);

    let mut gradient_composer = LightingComposer::new(color);
    let mut wide = frame();
    wide.stereo_width = 0.95;
    wide.centroid = 0.55;
    let gradient = settle(&mut gradient_composer, wide);

    assert_eq!(solid.effect, LightingEffect::Solid);
    assert!(solid.speed < 0.02 && solid.magic < 0.02);
    assert_eq!(snake.effect, LightingEffect::Snake);
    assert!(snake.speed > 0.65 && snake.magic > 0.45);
    assert_eq!(gradient.effect, LightingEffect::Gradient);
    assert!(gradient.magic > 0.70);
}
