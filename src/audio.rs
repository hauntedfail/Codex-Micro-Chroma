use std::collections::VecDeque;

use serde::Serialize;
use thiserror::Error;

const WINDOW_SIZE: usize = 2_048;
const MIN_AUDIBLE_PEAK: f32 = 0.0001;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct AudioFeatureFrame {
    pub loudness: f32,
    pub peak: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub centroid: f32,
    pub flatness: f32,
    pub flux: f32,
    pub onset: f32,
    pub pan: f32,
    pub stereo_width: f32,
    pub pulse: f32,
    pub tempo_bpm: f32,
    pub tempo_confidence: f32,
    pub silent: bool,
    pub timestamp_seconds: f64,
}

impl AudioFeatureFrame {
    pub fn values(self) -> impl Iterator<Item = f32> {
        [
            self.loudness,
            self.peak,
            self.bass,
            self.mid,
            self.treble,
            self.centroid,
            self.flatness,
            self.flux,
            self.onset,
            self.pan,
            self.stereo_width,
            self.pulse,
            self.tempo_bpm,
            self.tempo_confidence,
        ]
        .into_iter()
    }
}

#[derive(Debug, Error)]
pub enum AudioAnalyzerError {
    #[error("sample rate must be finite and greater than zero")]
    InvalidSampleRate,
}

pub struct AudioAnalyzer {
    sample_rate: f32,
    left: Vec<f32>,
    right: Vec<f32>,
    filled: usize,
    previous_spectrum: Vec<f32>,
    previous_loudness: f32,
    onset_times: VecDeque<f64>,
}

impl AudioAnalyzer {
    pub fn new(sample_rate: f32) -> Result<Self, AudioAnalyzerError> {
        if !sample_rate.is_finite() || sample_rate <= 0.0 {
            return Err(AudioAnalyzerError::InvalidSampleRate);
        }
        Ok(Self {
            sample_rate,
            left: vec![0.0; WINDOW_SIZE],
            right: vec![0.0; WINDOW_SIZE],
            filled: 0,
            previous_spectrum: vec![0.0; WINDOW_SIZE / 2],
            previous_loudness: 0.0,
            onset_times: VecDeque::with_capacity(16),
        })
    }

    pub const fn window_size(&self) -> usize {
        WINDOW_SIZE
    }

    pub fn push_stereo(
        &mut self,
        left: &[f32],
        right: &[f32],
        timestamp_seconds: f64,
    ) -> Option<AudioFeatureFrame> {
        let frames = left.len().min(right.len());
        if frames == 0 {
            return None;
        }

        if frames >= WINDOW_SIZE {
            let offset = frames - WINDOW_SIZE;
            self.left.copy_from_slice(&left[offset..frames]);
            self.right.copy_from_slice(&right[offset..frames]);
            self.filled = WINDOW_SIZE;
        } else if self.filled + frames <= WINDOW_SIZE {
            self.left[self.filled..self.filled + frames].copy_from_slice(&left[..frames]);
            self.right[self.filled..self.filled + frames].copy_from_slice(&right[..frames]);
            self.filled += frames;
        } else {
            let retained = WINDOW_SIZE - frames;
            let retained_start = self.filled - retained;
            self.left.copy_within(retained_start..self.filled, 0);
            self.right.copy_within(retained_start..self.filled, 0);
            self.left[retained..].copy_from_slice(&left[..frames]);
            self.right[retained..].copy_from_slice(&right[..frames]);
            self.filled = WINDOW_SIZE;
        }

        if self.filled < WINDOW_SIZE {
            return None;
        }

        Some(self.analyze(timestamp_seconds))
    }

    fn analyze(&mut self, timestamp_seconds: f64) -> AudioFeatureFrame {
        let mut mono = vec![0.0; WINDOW_SIZE];
        let mut sum_squares = 0.0;
        let mut left_squares = 0.0;
        let mut right_squares = 0.0;
        let mut mid_squares = 0.0;
        let mut side_squares = 0.0;
        let mut peak = 0.0_f32;

        for (index, (left, right)) in self.left.iter().zip(&self.right).enumerate() {
            let middle = (*left + *right) * 0.5;
            let side = (*left - *right) * 0.5;
            mono[index] = middle;
            sum_squares += middle * middle;
            left_squares += left * left;
            right_squares += right * right;
            mid_squares += middle * middle;
            side_squares += side * side;
            peak = peak.max(left.abs()).max(right.abs());
        }

        let divisor = WINDOW_SIZE as f32;
        let loudness = (sum_squares / divisor).sqrt().clamp(0.0, 1.0);
        let left_rms = (left_squares / divisor).sqrt();
        let right_rms = (right_squares / divisor).sqrt();
        let pan = ((right_rms - left_rms) / (right_rms + left_rms + f32::EPSILON)).clamp(-1.0, 1.0);
        let mid_rms = (mid_squares / divisor).sqrt();
        let side_rms = (side_squares / divisor).sqrt();
        let stereo_width = (side_rms / (side_rms + mid_rms + f32::EPSILON)).clamp(0.0, 1.0);

        let spectrum = magnitude_spectrum(&mono);
        let spectrum_sum = spectrum.iter().sum::<f32>();
        let power_sum = spectrum
            .iter()
            .map(|magnitude| magnitude * magnitude)
            .sum::<f32>();
        let (bass_power, mid_power, treble_power) = band_powers(&spectrum, self.sample_rate);
        let normalizer = power_sum.max(f32::EPSILON);
        let bass = (bass_power / normalizer).clamp(0.0, 1.0);
        let mid = (mid_power / normalizer).clamp(0.0, 1.0);
        let treble = (treble_power / normalizer).clamp(0.0, 1.0);

        let bin_hz = self.sample_rate / WINDOW_SIZE as f32;
        let centroid_hz = spectrum
            .iter()
            .enumerate()
            .map(|(index, magnitude)| index as f32 * bin_hz * magnitude)
            .sum::<f32>()
            / spectrum_sum.max(f32::EPSILON);
        let centroid = (centroid_hz / (self.sample_rate * 0.5)).clamp(0.0, 1.0);
        let flatness = spectral_flatness(&spectrum);
        let positive_difference = spectrum
            .iter()
            .zip(&self.previous_spectrum)
            .map(|(current, previous)| (current - previous).max(0.0))
            .sum::<f32>();
        let flux = (positive_difference / spectrum_sum.max(f32::EPSILON)).clamp(0.0, 1.0);
        let energy_rise = ((loudness - self.previous_loudness).max(0.0)
            / loudness.max(f32::EPSILON))
        .clamp(0.0, 1.0);
        let onset = (flux * 0.7 + energy_rise * 0.3).clamp(0.0, 1.0);

        self.previous_spectrum.copy_from_slice(&spectrum);
        self.previous_loudness = loudness;

        if onset >= 0.45 && peak >= MIN_AUDIBLE_PEAK {
            let sufficiently_separated = self
                .onset_times
                .back()
                .is_none_or(|previous| timestamp_seconds - previous >= 0.12);
            if sufficiently_separated {
                self.onset_times.push_back(timestamp_seconds);
            }
        }
        while self.onset_times.len() > 12 {
            self.onset_times.pop_front();
        }
        while self
            .onset_times
            .front()
            .is_some_and(|time| timestamp_seconds - time > 12.0)
        {
            self.onset_times.pop_front();
        }
        let (tempo_bpm, tempo_confidence) = tempo_estimate(&self.onset_times);
        let pulse = (tempo_confidence * 0.65 + onset * 0.35).clamp(0.0, 1.0);
        let silent = peak < MIN_AUDIBLE_PEAK;

        AudioFeatureFrame {
            loudness: finite_unit(loudness),
            peak: finite_unit(peak),
            bass: finite_unit(bass),
            mid: finite_unit(mid),
            treble: finite_unit(treble),
            centroid: finite_unit(centroid),
            flatness: finite_unit(flatness),
            flux: finite_unit(flux),
            onset: finite_unit(onset),
            pan: if pan.is_finite() { pan } else { 0.0 },
            stereo_width: finite_unit(stereo_width),
            pulse: finite_unit(pulse),
            tempo_bpm: if tempo_bpm.is_finite() {
                tempo_bpm
            } else {
                0.0
            },
            tempo_confidence: finite_unit(tempo_confidence),
            silent,
            timestamp_seconds,
        }
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn band_powers(spectrum: &[f32], sample_rate: f32) -> (f32, f32, f32) {
    let bin_hz = sample_rate / WINDOW_SIZE as f32;
    let mut bass = 0.0;
    let mut mid = 0.0;
    let mut treble = 0.0;
    for (index, magnitude) in spectrum.iter().enumerate().skip(1) {
        let frequency = index as f32 * bin_hz;
        let power = magnitude * magnitude;
        if frequency < 250.0 {
            bass += power;
        } else if frequency < 4_000.0 {
            mid += power;
        } else {
            treble += power;
        }
    }
    (bass, mid, treble)
}

fn spectral_flatness(spectrum: &[f32]) -> f32 {
    let usable = &spectrum[1..];
    if usable.is_empty() {
        return 0.0;
    }
    let epsilon = 1.0e-12_f32;
    let log_mean = usable
        .iter()
        .map(|value| value.max(epsilon).ln())
        .sum::<f32>()
        / usable.len() as f32;
    let arithmetic_mean = usable.iter().sum::<f32>() / usable.len() as f32;
    (log_mean.exp() / arithmetic_mean.max(epsilon)).clamp(0.0, 1.0)
}

fn tempo_estimate(onsets: &VecDeque<f64>) -> (f32, f32) {
    if onsets.len() < 4 {
        return (0.0, 0.0);
    }
    let mut intervals = onsets
        .iter()
        .zip(onsets.iter().skip(1))
        .map(|(left, right)| right - left)
        .filter(|interval| (0.25..=1.5).contains(interval))
        .collect::<Vec<_>>();
    if intervals.len() < 3 {
        return (0.0, 0.0);
    }
    intervals.sort_by(f64::total_cmp);
    let median = intervals[intervals.len() / 2];
    let deviation = intervals
        .iter()
        .map(|interval| (interval - median).abs())
        .sum::<f64>()
        / intervals.len() as f64;
    let confidence = (1.0 - deviation / median.max(f64::EPSILON)).clamp(0.0, 1.0) as f32;
    let bpm = (60.0 / median) as f32;
    (bpm, confidence)
}

#[derive(Clone, Copy, Default)]
struct Complex {
    real: f32,
    imaginary: f32,
}

impl Complex {
    fn magnitude(self) -> f32 {
        (self.real * self.real + self.imaginary * self.imaginary).sqrt()
    }
}

fn magnitude_spectrum(samples: &[f32]) -> Vec<f32> {
    debug_assert_eq!(samples.len(), WINDOW_SIZE);
    let mut values = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let window =
                0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / (WINDOW_SIZE - 1) as f32).cos();
            Complex {
                real: sample * window,
                imaginary: 0.0,
            }
        })
        .collect::<Vec<_>>();
    fft(&mut values);
    values[..WINDOW_SIZE / 2]
        .iter()
        .copied()
        .map(Complex::magnitude)
        .collect()
}

fn fft(values: &mut [Complex]) {
    let length = values.len();
    debug_assert!(length.is_power_of_two());

    let mut target = 0;
    for index in 1..length {
        let mut bit = length >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target ^= bit;
        if index < target {
            values.swap(index, target);
        }
    }

    let mut span = 2;
    while span <= length {
        let angle = -std::f32::consts::TAU / span as f32;
        let step = Complex {
            real: angle.cos(),
            imaginary: angle.sin(),
        };
        for start in (0..length).step_by(span) {
            let mut rotation = Complex {
                real: 1.0,
                imaginary: 0.0,
            };
            for offset in 0..span / 2 {
                let even = values[start + offset];
                let odd = values[start + offset + span / 2];
                let rotated = Complex {
                    real: odd.real * rotation.real - odd.imaginary * rotation.imaginary,
                    imaginary: odd.real * rotation.imaginary + odd.imaginary * rotation.real,
                };
                values[start + offset] = Complex {
                    real: even.real + rotated.real,
                    imaginary: even.imaginary + rotated.imaginary,
                };
                values[start + offset + span / 2] = Complex {
                    real: even.real - rotated.real,
                    imaginary: even.imaginary - rotated.imaginary,
                };
                rotation = Complex {
                    real: rotation.real * step.real - rotation.imaginary * step.imaginary,
                    imaginary: rotation.real * step.imaginary + rotation.imaginary * step.real,
                };
            }
        }
        span *= 2;
    }
}
