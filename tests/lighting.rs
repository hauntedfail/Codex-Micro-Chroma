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
