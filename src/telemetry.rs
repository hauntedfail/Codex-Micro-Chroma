use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::{
    audio::AudioFeatureFrame, lighting::LightingScene, media::TrackSnapshot,
    protocol::LightingEffect,
};

const COMPLETE_EDGE_TOLERANCE_SECONDS: f64 = 2.0;
const COMPLETE_OBSERVED_FRACTION: f64 = 0.85;
const MAX_ACCOUNTING_STEP_SECONDS: f64 = 1.0;

#[derive(Clone, Debug, Serialize)]
pub struct TrackMetadata {
    pub key: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bundle_id: Option<String>,
    pub duration_seconds: Option<f64>,
}

impl TrackMetadata {
    fn from_snapshot(key: &str, snapshot: &TrackSnapshot) -> Self {
        Self {
            key: key.to_owned(),
            title: snapshot.title.clone(),
            artist: snapshot.artist.clone(),
            album: snapshot.album.clone(),
            bundle_id: snapshot.bundle_id.clone(),
            duration_seconds: valid_time(snapshot.duration),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectUsage {
    pub effect: LightingEffect,
    pub seconds: f64,
    pub percent: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TrackSummary {
    pub track: TrackMetadata,
    pub reason: String,
    pub started_at_position_seconds: Option<f64>,
    pub ended_at_position_seconds: Option<f64>,
    pub observed_seconds: f64,
    pub transitions: u32,
    pub complete_track: bool,
    pub effect_usage: Vec<EffectUsage>,
}

pub struct TrackUsageTracker {
    track: TrackMetadata,
    started_at_position: Option<f64>,
    latest_position: Option<f64>,
    current_effect: Option<LightingEffect>,
    effect_seconds: [f64; 7],
    observed_seconds: f64,
    transitions: u32,
}

impl TrackUsageTracker {
    pub fn new(key: &str, snapshot: &TrackSnapshot) -> Self {
        let position = valid_time(snapshot.elapsed_time);
        Self {
            track: TrackMetadata::from_snapshot(key, snapshot),
            started_at_position: position,
            latest_position: position,
            current_effect: None,
            effect_seconds: [0.0; 7],
            observed_seconds: 0.0,
            transitions: 0,
        }
    }

    pub fn update_snapshot(&mut self, snapshot: &TrackSnapshot) {
        if let Some(duration) = valid_time(snapshot.duration) {
            self.track.duration_seconds = Some(duration);
        }
        if let Some(position) = valid_time(snapshot.elapsed_time) {
            self.latest_position = Some(position);
        }
    }

    pub fn observe_scene(&mut self, effect: LightingEffect, elapsed: Duration) -> bool {
        let seconds = elapsed
            .as_secs_f64()
            .clamp(0.0, MAX_ACCOUNTING_STEP_SECONDS);
        self.effect_seconds[usize::from(effect.code())] += seconds;
        self.observed_seconds += seconds;

        if self.current_effect == Some(effect) {
            return false;
        }
        self.current_effect = Some(effect);
        self.transitions = self.transitions.saturating_add(1);
        true
    }

    pub fn summary(&self, reason: impl Into<String>) -> TrackSummary {
        let duration = self.track.duration_seconds;
        let started_near_beginning = self
            .started_at_position
            .is_some_and(|position| position <= COMPLETE_EDGE_TOLERANCE_SECONDS);
        let ended_near_end =
            duration
                .zip(self.latest_position)
                .is_some_and(|(duration, position)| {
                    position >= duration - COMPLETE_EDGE_TOLERANCE_SECONDS
                });
        let observed_enough = duration
            .is_some_and(|duration| self.observed_seconds >= duration * COMPLETE_OBSERVED_FRACTION);
        let denominator = self.observed_seconds.max(f64::EPSILON);
        let effect_usage = LightingEffect::ALL
            .into_iter()
            .map(|effect| {
                let seconds = self.effect_seconds[usize::from(effect.code())];
                EffectUsage {
                    effect,
                    seconds,
                    percent: seconds / denominator * 100.0,
                }
            })
            .collect();

        TrackSummary {
            track: self.track.clone(),
            reason: reason.into(),
            started_at_position_seconds: self.started_at_position,
            ended_at_position_seconds: self.latest_position,
            observed_seconds: self.observed_seconds,
            transitions: self.transitions,
            complete_track: started_near_beginning && ended_near_end && observed_enough,
            effect_usage,
        }
    }
}

#[derive(Debug)]
pub struct FinishedTrack {
    pub path: PathBuf,
    pub summary: TrackSummary,
}

pub struct TrackLogger {
    directory: PathBuf,
    active: Option<ActiveTrackLog>,
}

struct ActiveTrackLog {
    path: PathBuf,
    tracker: TrackUsageTracker,
    writer: BufWriter<File>,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum TrackRecord<'a> {
    TrackStart {
        track: &'a TrackMetadata,
        started_at_position_seconds: Option<f64>,
    },
    EffectTransition {
        track_position_seconds: Option<f64>,
        scene: LightingScene,
        audio: AudioFeatureFrame,
    },
    TrackSummary {
        summary: &'a TrackSummary,
    },
}

impl TrackLogger {
    pub fn new(directory: impl Into<PathBuf>) -> io::Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        Ok(Self {
            directory,
            active: None,
        })
    }

    pub fn active_key(&self) -> Option<&str> {
        self.active
            .as_ref()
            .map(|active| active.tracker.track.key.as_str())
    }

    pub fn start_track(&mut self, key: &str, snapshot: &TrackSnapshot) -> io::Result<PathBuf> {
        let path = unique_log_path(&self.directory, snapshot.title.as_deref())?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let tracker = TrackUsageTracker::new(key, snapshot);
        let mut active = ActiveTrackLog {
            path: path.clone(),
            tracker,
            writer: BufWriter::new(file),
        };
        let record = TrackRecord::TrackStart {
            track: &active.tracker.track,
            started_at_position_seconds: active.tracker.started_at_position,
        };
        write_record(&mut active.writer, &record)?;
        self.active = Some(active);
        Ok(path)
    }

    pub fn update_snapshot(&mut self, snapshot: &TrackSnapshot) {
        if let Some(active) = self.active.as_mut() {
            active.tracker.update_snapshot(snapshot);
        }
    }

    pub fn observe_scene(
        &mut self,
        scene: LightingScene,
        frame: AudioFeatureFrame,
        elapsed: Duration,
    ) -> io::Result<bool> {
        let Some(active) = self.active.as_mut() else {
            return Ok(false);
        };
        if !active.tracker.observe_scene(scene.effect, elapsed) {
            return Ok(false);
        }
        let record = TrackRecord::EffectTransition {
            track_position_seconds: active.tracker.latest_position,
            scene,
            audio: frame,
        };
        write_record(&mut active.writer, &record)?;
        Ok(true)
    }

    pub fn finish(&mut self, reason: &str) -> io::Result<Option<FinishedTrack>> {
        let Some(mut active) = self.active.take() else {
            return Ok(None);
        };
        let summary = active.tracker.summary(reason);
        write_record(
            &mut active.writer,
            &TrackRecord::TrackSummary { summary: &summary },
        )?;
        Ok(Some(FinishedTrack {
            path: active.path,
            summary,
        }))
    }
}

pub fn default_track_log_directory() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library/Logs/CodexMicroChroma")
        .join("tracks"))
}

fn write_record(writer: &mut BufWriter<File>, record: &TrackRecord<'_>) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, record).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn unique_log_path(directory: &Path, title: Option<&str>) -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let slug = slug(title.unwrap_or("unknown-track"));
    for suffix in 0..1_000_u16 {
        let name = if suffix == 0 {
            format!("{timestamp}-{slug}.jsonl")
        } else {
            format!("{timestamp}-{slug}-{suffix}.jsonl")
        };
        let candidate = directory.join(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique track log path",
    ))
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    let mut separated = false;
    for character in value.chars().take(48) {
        if character.is_alphanumeric() {
            output.extend(character.to_lowercase());
            separated = false;
        } else if !separated && !output.is_empty() {
            output.push('-');
            separated = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "unknown-track".into()
    } else {
        output
    }
}

fn valid_time(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}
