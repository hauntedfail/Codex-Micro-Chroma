use codex_micro_chroma::audio::{AudioAnalyzer, AudioFeatureFrame};

fn stereo_sine(frequency: f32, sample_rate: f32, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let samples = (0..frames)
        .map(|index| {
            let phase = std::f32::consts::TAU * frequency * index as f32 / sample_rate;
            phase.sin() * 0.5
        })
        .collect::<Vec<_>>();
    (samples.clone(), samples)
}

#[test]
fn silence_produces_a_finite_silent_feature_frame() {
    let mut analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let silence = vec![0.0; analyzer.window_size()];
    let frame = analyzer
        .push_stereo(&silence, &silence, 0.0)
        .expect("one full window should produce features");

    assert!(frame.silent);
    assert_eq!(frame.loudness, 0.0);
    assert_eq!(frame.peak, 0.0);
    assert!(frame.values().all(f32::is_finite));
}

#[test]
fn frequency_bands_distinguish_bass_from_treble() {
    let mut bass_analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let (left, right) = stereo_sine(94.0, 48_000.0, bass_analyzer.window_size());
    let bass = bass_analyzer.push_stereo(&left, &right, 0.0).unwrap();

    let mut treble_analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let (left, right) = stereo_sine(7_000.0, 48_000.0, treble_analyzer.window_size());
    let treble = treble_analyzer.push_stereo(&left, &right, 0.0).unwrap();

    assert!(bass.bass > bass.treble * 4.0, "{bass:?}");
    assert!(treble.treble > treble.bass * 4.0, "{treble:?}");
    assert!(
        treble.centroid > bass.centroid,
        "bass={bass:?} treble={treble:?}"
    );
}

#[test]
fn a_new_signal_is_reported_as_an_onset() {
    let mut analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let silence = vec![0.0; analyzer.window_size()];
    let _ = analyzer.push_stereo(&silence, &silence, 0.0).unwrap();

    let (left, right) = stereo_sine(220.0, 48_000.0, analyzer.window_size());
    let onset = analyzer.push_stereo(&left, &right, 0.02).unwrap();
    let steady = analyzer.push_stereo(&left, &right, 0.04).unwrap();

    assert!(
        onset.onset > steady.onset + 0.2,
        "onset={onset:?} steady={steady:?}"
    );
}

#[test]
fn opposite_stereo_channels_are_wider_than_centered_audio() {
    let mut centered_analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let (left, right) = stereo_sine(440.0, 48_000.0, centered_analyzer.window_size());
    let centered = centered_analyzer.push_stereo(&left, &right, 0.0).unwrap();

    let mut wide_analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let inverted = right.iter().map(|sample| -*sample).collect::<Vec<_>>();
    let wide = wide_analyzer.push_stereo(&left, &inverted, 0.0).unwrap();

    assert!(wide.stereo_width > centered.stereo_width + 0.5);
}

#[test]
fn invalid_sample_rates_are_rejected() {
    assert!(AudioAnalyzer::new(0.0).is_err());
    assert!(AudioAnalyzer::new(f32::NAN).is_err());
}

#[test]
fn feature_frame_is_json_serializable_for_audio_probe() {
    let encoded = serde_json::to_value(AudioFeatureFrame::default()).unwrap();
    assert!(encoded.get("loudness").is_some());
    assert!(encoded.get("tempo_confidence").is_some());
}

#[test]
fn a_partial_window_keeps_its_newest_samples_when_the_next_packet_crosses_the_boundary() {
    let mut analyzer = AudioAnalyzer::new(48_000.0).unwrap();
    let first = vec![1.0; 1_000];
    assert!(analyzer.push_stereo(&first, &first, 0.0).is_none());

    let second = vec![0.0; 1_500];
    let frame = analyzer
        .push_stereo(&second, &second, 0.03)
        .expect("the combined packets exceed one analysis window");

    assert!(frame.loudness > 0.45, "{frame:?}");
    assert!(frame.loudness < 0.60, "{frame:?}");
    assert!(!frame.silent);
}
