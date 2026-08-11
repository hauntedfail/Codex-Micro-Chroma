use std::time::Duration;

use crate::{audio::AudioFeatureFrame, color::Rgb, protocol::LightingEffect};

const EFFECT_DWELL: Duration = Duration::from_millis(900);
const MIN_EFFECT_HOLD: Duration = Duration::from_secs(2);
const SILENCE_HOLD: Duration = Duration::from_millis(1_200);
const RAINBOW_COOLDOWN: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightingScene {
    pub effect: LightingEffect,
    pub color: Rgb,
    pub brightness: f32,
    pub speed: f32,
    pub magic: f32,
}

pub struct LightingComposer {
    color: Rgb,
    current_effect: LightingEffect,
    current_effect_age: Duration,
    candidate: Option<LightingEffect>,
    candidate_age: Duration,
    silence_age: Duration,
    rainbow_cooldown: Duration,
    brightness: f32,
    speed: f32,
    magic: f32,
    loudness_floor: f32,
    loudness_ceiling: f32,
}

impl LightingComposer {
    pub fn new(color: Rgb) -> Self {
        Self {
            color,
            current_effect: LightingEffect::Breath,
            current_effect_age: Duration::ZERO,
            candidate: None,
            candidate_age: Duration::ZERO,
            silence_age: Duration::ZERO,
            rainbow_cooldown: Duration::ZERO,
            brightness: 0.35,
            speed: 0.35,
            magic: 0.0,
            loudness_floor: 1.0,
            loudness_ceiling: 0.10,
        }
    }

    pub fn set_color(&mut self, color: Rgb) {
        self.color = color;
    }

    pub fn update(&mut self, frame: AudioFeatureFrame, elapsed: Duration) -> LightingScene {
        self.current_effect_age = self.current_effect_age.saturating_add(elapsed);
        self.rainbow_cooldown = self.rainbow_cooldown.saturating_sub(elapsed);

        if frame.silent {
            self.silence_age = self.silence_age.saturating_add(elapsed);
        } else {
            self.silence_age = Duration::ZERO;
            self.observe_loudness(frame.loudness, elapsed);
        }

        let desired = if self.silence_age >= SILENCE_HOLD {
            LightingEffect::Off
        } else if frame.silent {
            self.current_effect
        } else {
            self.select_effect(frame)
        };
        self.advance_effect(desired, elapsed);

        let normalized_loudness = if frame.silent {
            0.0
        } else {
            ((frame.loudness - self.loudness_floor)
                / (self.loudness_ceiling - self.loudness_floor).max(0.02))
            .clamp(0.0, 1.0)
        };
        let target_brightness = if self.current_effect == LightingEffect::Off {
            0.0
        } else {
            (0.12 + normalized_loudness * 0.72 + frame.onset * 0.16).clamp(0.0, 1.0)
        };
        let motion = (frame.pulse * 0.34
            + frame.onset * 0.22
            + frame.flux * 0.20
            + frame.bass * 0.14
            + frame.tempo_confidence * 0.10)
            .clamp(0.0, 1.0);
        let target_speed = (0.10 + motion * 0.82).clamp(0.0, 1.0);
        let target_magic =
            (frame.stereo_width * 0.45 + frame.flux * 0.35 + frame.flatness * 0.20).clamp(0.0, 1.0);

        let seconds = elapsed.as_secs_f32().max(0.001);
        self.brightness = smooth(
            self.brightness,
            target_brightness,
            if target_brightness > self.brightness {
                12.0
            } else {
                3.5
            },
            seconds,
        );
        self.speed = smooth(self.speed, target_speed, 2.2, seconds);
        self.magic = smooth(self.magic, target_magic, 1.6, seconds);

        LightingScene {
            effect: self.current_effect,
            color: self.color,
            brightness: finite_unit(self.brightness),
            speed: finite_unit(self.speed),
            magic: finite_unit(self.magic),
        }
    }

    fn observe_loudness(&mut self, loudness: f32, elapsed: Duration) {
        let seconds = elapsed.as_secs_f32().max(0.001);
        self.loudness_floor = if loudness < self.loudness_floor {
            smooth(self.loudness_floor, loudness, 4.0, seconds)
        } else {
            smooth(self.loudness_floor, loudness, 0.025, seconds)
        };
        self.loudness_ceiling = if loudness > self.loudness_ceiling {
            smooth(self.loudness_ceiling, loudness, 5.0, seconds)
        } else {
            smooth(self.loudness_ceiling, loudness.max(0.10), 0.04, seconds)
        };
    }

    fn select_effect(&self, frame: AudioFeatureFrame) -> LightingEffect {
        let quiet = ((0.12 - frame.loudness) / 0.12).clamp(0.0, 1.0);
        let tonal = 1.0 - frame.flatness;
        let sustained = 1.0 - frame.onset;
        let stable = 1.0 - frame.flux;
        let center = 1.0 - frame.stereo_width;
        let irregular = 1.0 - frame.tempo_confidence;

        let scores = [
            (
                LightingEffect::Solid,
                center * 1.0 + frame.mid * 0.8 + irregular * 0.35 - frame.bass * 0.25,
            ),
            (
                LightingEffect::Snake,
                frame.pulse * 1.1
                    + frame.bass * 0.6
                    + frame.onset * 0.5
                    + frame.tempo_confidence * 0.25,
            ),
            (
                LightingEffect::Rainbow,
                if self.rainbow_cooldown.is_zero() || self.current_effect == LightingEffect::Rainbow
                {
                    frame.loudness * 0.7
                        + frame.flux * 0.8
                        + frame.onset * 0.6
                        + frame.treble * 0.4
                        + frame.stereo_width * 0.2
                } else {
                    -1.0
                },
            ),
            (
                LightingEffect::Breath,
                0.55 + tonal * 0.65 + sustained * 0.45 + stable * 0.35,
            ),
            (
                LightingEffect::Gradient,
                frame.stereo_width * 1.7 + frame.centroid * 0.25 + tonal * 0.15,
            ),
            (
                LightingEffect::ShallowBreath,
                quiet * 2.4 + sustained * 0.25 + stable * 0.20,
            ),
        ];

        scores
            .into_iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(effect, _)| effect)
            .unwrap_or(LightingEffect::Breath)
    }

    fn advance_effect(&mut self, desired: LightingEffect, elapsed: Duration) {
        if desired == self.current_effect {
            self.candidate = None;
            self.candidate_age = Duration::ZERO;
            return;
        }

        if self.candidate == Some(desired) {
            self.candidate_age = self.candidate_age.saturating_add(elapsed);
        } else {
            self.candidate = Some(desired);
            self.candidate_age = elapsed;
        }

        let silence_override = desired == LightingEffect::Off && self.silence_age >= SILENCE_HOLD;
        if self.candidate_age >= EFFECT_DWELL
            && (self.current_effect_age >= MIN_EFFECT_HOLD || silence_override)
        {
            self.current_effect = desired;
            self.current_effect_age = Duration::ZERO;
            self.candidate = None;
            self.candidate_age = Duration::ZERO;
            if desired == LightingEffect::Rainbow {
                self.rainbow_cooldown = RAINBOW_COOLDOWN;
            }
        }
    }
}

fn smooth(current: f32, target: f32, rate: f32, seconds: f32) -> f32 {
    let alpha = 1.0 - (-rate * seconds).exp();
    current + (target - current) * alpha
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
