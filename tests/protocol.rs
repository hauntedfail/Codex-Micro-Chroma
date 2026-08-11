use codex_micro_chroma::protocol::{
    ambient_effect_request, frame_rpc_request, off_request, LightingEffect, REPORT_SIZE,
};

fn decode_frames(frames: &[[u8; REPORT_SIZE]]) -> String {
    let bytes = frames
        .iter()
        .flat_map(|report| report[3..3 + usize::from(report[2])].iter().copied())
        .collect::<Vec<_>>();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn frames_rgb_configuration_for_the_codex_micro_rpc_channel() {
    let request =
        ambient_effect_request(7, LightingEffect::Breath, 0x123456, 0.75, 0.85, 0.25).unwrap();
    let frames = frame_rpc_request(&request).unwrap();

    assert!(frames.len() >= 2);
    assert!(frames.iter().all(|frame| frame.len() == REPORT_SIZE));
    assert!(frames.iter().all(|frame| frame[0] == 0x06));
    assert!(frames.iter().all(|frame| frame[1] == 0x02));

    let decoded: serde_json::Value = serde_json::from_str(&decode_frames(&frames)).unwrap();
    assert_eq!(decoded["method"], "v.oai.rgbcfg");
    assert_eq!(decoded["id"], 7);
    assert_eq!(decoded["params"]["keys"]["e"], 0);
    assert_eq!(decoded["params"]["ambient"]["e"], 4);
    assert_eq!(decoded["params"]["ambient"]["b"], 0.75);
    assert_eq!(decoded["params"]["ambient"]["s"], 0.85);
    assert_eq!(decoded["params"]["ambient"]["m"], 0.25);
    assert_eq!(decoded["params"]["ambient"]["c"], 0x123456);
}

#[test]
fn every_known_effect_uses_the_discovered_device_code() {
    let cases = [
        (LightingEffect::Off, 0),
        (LightingEffect::Solid, 1),
        (LightingEffect::Snake, 2),
        (LightingEffect::Rainbow, 3),
        (LightingEffect::Breath, 4),
        (LightingEffect::Gradient, 5),
        (LightingEffect::ShallowBreath, 6),
    ];

    for (effect, expected_code) in cases {
        let request = ambient_effect_request(1, effect, 0xAABBCC, 1.0, 0.5, 0.0).unwrap();
        let frames = frame_rpc_request(&request).unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&decode_frames(&frames)).unwrap();
        assert_eq!(decoded["params"]["ambient"]["e"], expected_code);
    }
}

#[test]
fn off_request_disables_both_lighting_sides() {
    let request = off_request(42);
    let frames = frame_rpc_request(&request).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&decode_frames(&frames)).unwrap();

    assert_eq!(decoded["params"]["keys"]["e"], 0);
    assert_eq!(decoded["params"]["ambient"]["e"], 0);
    assert_eq!(decoded["params"]["ambient"]["b"], 0.0);
    assert_eq!(decoded["params"]["ambient"]["c"], 0);
}

#[test]
fn numeric_effect_parameters_must_be_unit_intervals() {
    assert!(ambient_effect_request(1, LightingEffect::Breath, 0, -0.1, 0.5, 0.0).is_err());
    assert!(ambient_effect_request(1, LightingEffect::Breath, 0, 1.0, 1.1, 0.0).is_err());
    assert!(ambient_effect_request(1, LightingEffect::Breath, 0, 1.0, 0.5, f32::NAN).is_err());
}
