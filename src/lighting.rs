use std::time::Duration;

use crate::{audio::AudioFeatureFrame, color::Rgb, protocol::LightingEffect};

const EFFECT_DWELL: Duration = Duration::from_millis(650);
const MIN_EFFECT_HOLD: Duration = Duration::from_secs(2);
const MAX_EFFECT_HOLD: Duration = Duration::from_secs(12);
const SILENCE_HOLD: Duration = Duration::from_millis(1_200);
const RAINBOW_COOLDOWN: Duration = Duration::from_secs(12);
const CANDIDATE_SCORE_MARGIN: f32 = 0.24;
const VARIETY_SCORE_MARGIN: f32 = 0.28;

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
    bass_envelope: f32,
    flux_envelope: f32,
    onset_envelope: f32,
    pulse_envelope: f32,
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
            bass_envelope: 0.0,
            flux_envelope: 0.0,
            onset_envelope: 0.0,
            pulse_envelope: 0.0,
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
            self.observe_motion(frame, elapsed);
        }

        let normalized_loudness = if frame.silent {
            0.0
        } else {
            ((frame.loudness - self.loudness_floor)
                / (self.loudness_ceiling - self.loudness_floor).max(0.02))
            .clamp(0.0, 1.0)
        };
        let level = perceptual_level(frame, normalized_loudness);
        let desired = if self.silence_age >= SILENCE_HOLD {
            LightingEffect::Off
        } else if frame.silent {
            self.current_effect
        } else {
            self.select_effect(frame, level)
        };
        self.advance_effect(desired, elapsed);

        let target_brightness = if self.current_effect == LightingEffect::Off {
            0.0
        } else {
            (0.12 + level * 0.72 + frame.onset * 0.16).clamp(0.0, 1.0)
        };
        let motion = (self.pulse_envelope * 0.34
            + self.onset_envelope * 0.22
            + self.flux_envelope * 0.20
            + self.bass_envelope * 0.14
            + frame.tempo_confidence * 0.10)
            .clamp(0.0, 1.0);
        let generic_speed = (0.10 + motion * 0.82).clamp(0.0, 1.0);
        let generic_magic =
            (frame.stereo_width * 0.45 + frame.flux * 0.35 + frame.flatness * 0.20).clamp(0.0, 1.0);
        let (target_speed, target_magic) = effect_parameters(
            self.current_effect,
            generic_speed,
            generic_magic,
            self.pulse_envelope,
            self.onset_envelope,
            frame.stereo_width,
        );

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

    fn observe_motion(&mut self, frame: AudioFeatureFrame, elapsed: Duration) {
        let seconds = elapsed.as_secs_f32().max(0.001);
        self.bass_envelope = envelope(self.bass_envelope, frame.bass, 3.5, 0.8, seconds);
        self.flux_envelope = envelope(self.flux_envelope, frame.flux, 10.0, 0.45, seconds);
        self.onset_envelope = envelope(self.onset_envelope, frame.onset, 14.0, 0.40, seconds);
        self.pulse_envelope = envelope(self.pulse_envelope, frame.pulse, 6.0, 0.35, seconds);
    }

    fn select_effect(&self, frame: AudioFeatureFrame, level: f32) -> LightingEffect {
        let quiet = ((0.32 - level) / 0.32).clamp(0.0, 1.0);
        let tonal = 1.0 - frame.flatness;
        let sustained = 1.0 - self.onset_envelope;
        let stable = 1.0 - self.flux_envelope;
        let center = 1.0 - frame.stereo_width;
        let irregular = 1.0 - frame.tempo_confidence;

        let scores = [
            (
                LightingEffect::Solid,
                0.30 + center * 0.55 + frame.mid * 0.45 + irregular * 0.15
                    - self.bass_envelope * 0.15,
            ),
            (
                LightingEffect::Snake,
                0.15 + self.pulse_envelope * 0.85
                    + self.bass_envelope * 0.40
                    + self.onset_envelope * 0.45
                    + frame.tempo_confidence * 0.25,
            ),
            (
                LightingEffect::Rainbow,
                if (self.rainbow_cooldown.is_zero()
                    || self.current_effect == LightingEffect::Rainbow)
                    && level >= 0.72
                    && self.onset_envelope >= 0.55
                    && self.flux_envelope >= 0.55
                    && (frame.treble >= 0.35 || frame.stereo_width >= 0.55)
                {
                    0.20 + level * 0.45
                        + self.flux_envelope * 0.55
                        + self.onset_envelope * 0.45
                        + frame.treble * 0.25
                        + frame.stereo_width * 0.20
                } else {
                    -1.0
                },
            ),
            (
                LightingEffect::Breath,
                0.30 + tonal * 0.45 + sustained * 0.30 + stable * 0.20,
            ),
            (
                LightingEffect::Gradient,
                0.10 + frame.stereo_width * 1.05 + frame.centroid * 0.20 + tonal * 0.10,
            ),
            (
                LightingEffect::ShallowBreath,
                quiet * 1.40 + sustained * 0.20 + stable * 0.15,
            ),
        ];

        let best = scores
            .into_iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .unwrap_or((LightingEffect::Breath, 0.0));
        if let Some(candidate) = self.candidate {
            if scores.into_iter().any(|(effect, score)| {
                effect == candidate && score >= best.1 - CANDIDATE_SCORE_MARGIN
            }) {
                return candidate;
            }
        }
        if best.0 != self.current_effect || self.current_effect_age < MAX_EFFECT_HOLD {
            return best.0;
        }

        scores
            .into_iter()
            .filter(|(effect, score)| {
                *effect != self.current_effect && *score >= best.1 - VARIETY_SCORE_MARGIN
            })
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map_or(best.0, |(effect, _)| effect)
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

fn perceptual_level(frame: AudioFeatureFrame, relative_loudness: f32) -> f32 {
    if frame.silent {
        return 0.0;
    }
    let rms = (frame.loudness / 0.20).clamp(0.0, 1.0);
    let peak = (frame.peak / 0.70).clamp(0.0, 1.0);
    relative_loudness.max(rms * 0.55 + peak * 0.45)
}

fn effect_parameters(
    effect: LightingEffect,
    speed: f32,
    magic: f32,
    pulse: f32,
    onset: f32,
    width: f32,
) -> (f32, f32) {
    match effect {
        LightingEffect::Off | LightingEffect::Solid => (0.0, 0.0),
        LightingEffect::Snake => (
            (0.28 + speed * 0.52 + pulse * 0.20).clamp(0.0, 1.0),
            (0.12 + onset * 0.58 + magic * 0.30).clamp(0.0, 1.0),
        ),
        LightingEffect::Rainbow => (
            (0.42 + speed * 0.58).clamp(0.0, 1.0),
            (0.55 + magic * 0.45).clamp(0.0, 1.0),
        ),
        LightingEffect::Breath => (
            (0.10 + speed * 0.38).clamp(0.0, 1.0),
            (magic * 0.30).clamp(0.0, 1.0),
        ),
        LightingEffect::Gradient => (
            (0.16 + speed * 0.42).clamp(0.0, 1.0),
            (0.38 + width * 0.42 + magic * 0.20).clamp(0.0, 1.0),
        ),
        LightingEffect::ShallowBreath => (
            (0.06 + speed * 0.22).clamp(0.0, 1.0),
            (magic * 0.16).clamp(0.0, 1.0),
        ),
    }
}

fn envelope(current: f32, target: f32, attack: f32, release: f32, seconds: f32) -> f32 {
    smooth(
        current,
        target,
        if target > current { attack } else { release },
        seconds,
    )
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
