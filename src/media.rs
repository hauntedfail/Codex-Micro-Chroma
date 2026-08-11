use image::{DynamicImage, GenericImageView};
use serde::Deserialize;

const POSITION_EDGE_TOLERANCE_SECONDS: f64 = 2.0;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackCandidate {
    stable_id: String,
    #[serde(default)]
    bundle_id: Option<String>,
    playing: bool,
    #[serde(default)]
    playing_resolved: bool,
    #[serde(default)]
    last_playing_date: Option<f64>,
    #[serde(default)]
    elected: bool,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    elapsed_time: Option<f64>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    playback_rate: Option<f64>,
    #[serde(default)]
    info_update_date: Option<f64>,
    #[serde(default)]
    artwork_data: Option<String>,
}

fn select_playback_candidate(candidates: &[PlaybackCandidate]) -> Option<&PlaybackCandidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.playing && candidate.playing_resolved)
        .max_by(|left, right| {
            left.last_playing_date
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&right.last_playing_date.unwrap_or(f64::NEG_INFINITY))
                .then_with(|| left.elected.cmp(&right.elected))
                .then_with(|| right.stable_id.cmp(&left.stable_id))
        })
}

#[derive(Clone, Debug)]
pub struct TrackSnapshot {
    pub is_playing: Option<bool>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bundle_id: Option<String>,
    pub elapsed_time: Option<f64>,
    pub duration: Option<f64>,
    pub playback_rate: Option<f64>,
    pub artwork: Option<DynamicImage>,
    pub artwork_signature: Option<u64>,
}

impl TrackSnapshot {
    pub fn track_key(&self) -> Option<String> {
        let bundle_id = normalized(self.bundle_id.as_deref());
        let title = normalized(self.title.as_deref());
        let artist = normalized(self.artist.as_deref());
        let album = normalized(self.album.as_deref());
        if [bundle_id, title, artist, album]
            .into_iter()
            .all(str::is_empty)
            && self.artwork_signature.is_none()
        {
            return None;
        }
        Some(format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:016x}",
            bundle_id,
            title,
            artist,
            album,
            self.artwork_signature.unwrap_or_default()
        ))
    }
}

fn normalized(value: Option<&str>) -> &str {
    value.map(str::trim).unwrap_or_default()
}

fn artwork_signature(image: &DynamicImage) -> u64 {
    let (width, height) = image.dimensions();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in width.to_le_bytes().into_iter().chain(height.to_le_bytes()) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    if width == 0 || height == 0 {
        return hash;
    }
    for grid_y in 0..5_u64 {
        for grid_x in 0..5_u64 {
            let x = (u64::from(width - 1) * grid_x / 4) as u32;
            let y = (u64::from(height - 1) * grid_y / 4) as u32;
            for byte in image.get_pixel(x, y).0 {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    hash
}

fn position_at_reception(
    elapsed: Option<f64>,
    duration: Option<f64>,
    is_playing: Option<bool>,
    playback_rate: Option<f64>,
    update_age_seconds: Option<f64>,
    resumed: bool,
) -> Option<f64> {
    let elapsed = elapsed.filter(|value| value.is_finite() && *value >= 0.0)?;
    let duration = duration.filter(|value| value.is_finite() && *value > 0.0);
    if duration.is_some_and(|duration| elapsed > duration + POSITION_EDGE_TOLERANCE_SECONDS) {
        return None;
    }
    let rate = playback_rate
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or_else(|| f64::from(is_playing == Some(true)));
    let update_age = update_age_seconds.filter(|value| value.is_finite() && *value >= 0.0);

    // MediaRemote's timestamp can remain at the start of a long pause when
    // playback resumes. Anchor a resume at the newly delivered elapsed value.
    let projected = if resumed || is_playing != Some(true) || rate == 0.0 {
        elapsed
    } else {
        elapsed + update_age.unwrap_or(0.0) * rate
    };

    // A stale timestamp can put the projection beyond the track. In that case the
    // payload's elapsed value is the only usable local anchor.
    if duration.is_some_and(|duration| projected > duration + POSITION_EDGE_TOLERANCE_SECONDS) {
        Some(elapsed)
    } else {
        Some(projected)
    }
}

#[derive(Default)]
struct ArtworkDelivery {
    last_key: Option<String>,
}

impl ArtworkDelivery {
    fn should_deliver(&mut self, key: Option<&str>, artwork_available: bool) -> bool {
        let Some(key) = key.filter(|_| artwork_available) else {
            return false;
        };
        if self.last_key.as_deref() == Some(key) {
            return false;
        }
        self.last_key = Some(key.to_owned());
        true
    }

    fn invalidate(&mut self) {
        self.last_key = None;
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        cell::RefCell,
        fs,
        io::{BufRead, BufReader},
        path::Path,
        process::{Child, Command, Stdio},
        sync::{Arc, RwLock},
        thread::{self, JoinHandle},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use anyhow::{bail, Context, Result};
    use base64::{engine::general_purpose, Engine as _};
    use serde::Deserialize;
    use tempfile::TempDir;

    use super::{
        artwork_signature, position_at_reception, select_playback_candidate, ArtworkDelivery,
        PlaybackCandidate, TrackSnapshot,
    };

    const MEDIA_SESSIONS_DYLIB: &[u8] = include_bytes!(concat!(
        env!("OUT_DIR"),
        "/libcodex_micro_chroma_media_sessions.dylib"
    ));
    const MEDIA_SESSIONS_PERL: &str = include_str!("media_sessions.pl");

    #[derive(Deserialize)]
    struct SessionPayload {
        candidates: Vec<PlaybackCandidate>,
    }

    struct ReceivedNowPlaying {
        snapshot: TrackSnapshot,
        position_at_received: Option<f64>,
        received_at: Instant,
    }

    impl ReceivedNowPlaying {
        fn from_candidate(candidate: Option<&PlaybackCandidate>, previous: Option<&Self>) -> Self {
            let received_at = Instant::now();
            let artwork = candidate
                .and_then(|candidate| candidate.artwork_data.as_deref())
                .and_then(|encoded| general_purpose::STANDARD.decode(encoded).ok())
                .and_then(|bytes| image::load_from_memory(&bytes).ok());
            let mut snapshot = TrackSnapshot {
                is_playing: Some(candidate.is_some_and(|candidate| candidate.playing)),
                title: candidate.and_then(|candidate| candidate.title.clone()),
                artist: candidate.and_then(|candidate| candidate.artist.clone()),
                album: candidate.and_then(|candidate| candidate.album.clone()),
                bundle_id: candidate.and_then(|candidate| candidate.bundle_id.clone()),
                elapsed_time: candidate.and_then(|candidate| candidate.elapsed_time),
                duration: candidate.and_then(|candidate| candidate.duration),
                playback_rate: candidate.and_then(|candidate| candidate.playback_rate),
                artwork_signature: artwork.as_ref().map(artwork_signature),
                artwork,
            };
            let resumed = previous.is_some_and(|previous| {
                same_track(&previous.snapshot, &snapshot)
                    && previous.snapshot.is_playing == Some(false)
                    && snapshot.is_playing == Some(true)
            });
            let info_update_time = candidate
                .and_then(|candidate| candidate.info_update_date)
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs_f64(seconds)));
            let update_age_seconds = info_update_time.and_then(|updated_at| {
                SystemTime::now()
                    .duration_since(updated_at)
                    .ok()
                    .map(|age| age.as_secs_f64())
            });
            let position_at_received = position_at_reception(
                snapshot.elapsed_time,
                snapshot.duration,
                snapshot.is_playing,
                snapshot.playback_rate,
                update_age_seconds,
                resumed,
            );
            snapshot.elapsed_time = position_at_received;
            Self {
                snapshot,
                position_at_received,
                received_at,
            }
        }

        fn elapsed_time(&self) -> Option<f64> {
            let rate = self
                .snapshot
                .playback_rate
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or_else(|| f64::from(self.snapshot.is_playing == Some(true)));
            let projected = self.position_at_received?
                + if self.snapshot.is_playing == Some(true) {
                    self.received_at.elapsed().as_secs_f64() * rate
                } else {
                    0.0
                };
            Some(
                self.snapshot
                    .duration
                    .filter(|duration| duration.is_finite() && *duration > 0.0)
                    .map_or(projected, |duration| projected.min(duration)),
            )
        }
    }

    fn same_track(left: &TrackSnapshot, right: &TrackSnapshot) -> bool {
        left.bundle_id == right.bundle_id
            && left.title == right.title
            && left.artist == right.artist
            && left.album == right.album
    }

    pub struct MediaRemoteSource {
        child: Child,
        reader: Option<JoinHandle<()>>,
        _temp_dir: TempDir,
        latest: Arc<RwLock<Option<ReceivedNowPlaying>>>,
        artwork_delivery: RefCell<ArtworkDelivery>,
    }

    impl MediaRemoteSource {
        pub fn new() -> Result<Self> {
            if !Path::new("/usr/bin/perl").is_file() {
                bail!("macOS system Perl was not found at /usr/bin/perl");
            }

            let temp_dir = tempfile::Builder::new()
                .prefix("codex-micro-chroma-media-sessions")
                .tempdir()
                .context("could not create MediaRemote helper directory")?;
            let dylib_path = temp_dir.path().join("media_sessions.dylib");
            let perl_path = temp_dir.path().join("media_sessions.pl");
            fs::write(&dylib_path, MEDIA_SESSIONS_DYLIB)
                .context("could not extract MediaRemote session helper")?;
            fs::write(&perl_path, MEDIA_SESSIONS_PERL)
                .context("could not extract MediaRemote Perl shim")?;

            let mut child = Command::new("/usr/bin/perl")
                .arg(&perl_path)
                .arg(&dylib_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .context("could not start MediaRemote session helper")?;
            let stdout = child
                .stdout
                .take()
                .context("MediaRemote session helper stdout is unavailable")?;
            let latest = Arc::new(RwLock::new(None::<ReceivedNowPlaying>));
            let reader_latest = Arc::clone(&latest);
            let reader = thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let Ok(payload) = serde_json::from_str::<SessionPayload>(&line) else {
                        continue;
                    };
                    let selected = select_playback_candidate(&payload.candidates);
                    if let Ok(mut latest) = reader_latest.write() {
                        *latest = Some(ReceivedNowPlaying::from_candidate(
                            selected,
                            latest.as_ref(),
                        ));
                    }
                }
            });
            Ok(Self {
                child,
                reader: Some(reader),
                _temp_dir: temp_dir,
                latest,
                artwork_delivery: RefCell::new(ArtworkDelivery::default()),
            })
        }

        pub fn snapshot(&self) -> Option<TrackSnapshot> {
            let guard = self.latest.read().ok()?;
            let received = guard.as_ref()?;
            let mut snapshot = received.snapshot.clone();
            snapshot.elapsed_time = received.elapsed_time();
            let key = snapshot.track_key();
            let should_copy_artwork = self
                .artwork_delivery
                .borrow_mut()
                .should_deliver(key.as_deref(), snapshot.artwork.is_some());
            if !should_copy_artwork {
                snapshot.artwork = None;
            }
            Some(snapshot)
        }

        pub fn invalidate_artwork_delivery(&self) {
            self.artwork_delivery.borrow_mut().invalidate();
        }
    }

    impl Drop for MediaRemoteSource {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{bail, Result};

    use super::TrackSnapshot;

    pub struct MediaRemoteSource;

    impl MediaRemoteSource {
        pub fn new() -> Result<Self> {
            bail!("MediaRemote is only available on macOS")
        }

        pub fn snapshot(&self) -> Option<TrackSnapshot> {
            None
        }

        pub fn invalidate_artwork_delivery(&self) {}
    }
}

pub use platform::MediaRemoteSource;

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(bundle_id: Option<&str>, title: Option<&str>) -> TrackSnapshot {
        TrackSnapshot {
            is_playing: Some(true),
            title: title.map(str::to_owned),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            bundle_id: bundle_id.map(str::to_owned),
            elapsed_time: Some(0.0),
            duration: Some(180.0),
            playback_rate: Some(1.0),
            artwork: None,
            artwork_signature: None,
        }
    }

    fn candidate(
        stable_id: &str,
        playing: bool,
        last_playing_date: Option<f64>,
        elected: bool,
    ) -> PlaybackCandidate {
        PlaybackCandidate {
            stable_id: stable_id.into(),
            bundle_id: None,
            playing,
            playing_resolved: true,
            last_playing_date,
            elected,
            title: None,
            artist: None,
            album: None,
            elapsed_time: None,
            duration: None,
            playback_rate: None,
            info_update_date: None,
            artwork_data: None,
        }
    }

    #[test]
    fn most_recently_played_active_session_wins() {
        let candidates = [
            candidate("spotify", true, Some(100.0), true),
            candidate("music", true, Some(200.0), false),
        ];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("music")
        );
    }

    #[test]
    fn stopping_the_selected_session_falls_back_to_another_active_session() {
        let candidates = [
            candidate("spotify", true, Some(100.0), false),
            candidate("music", false, Some(200.0), true),
        ];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("spotify")
        );
    }

    #[test]
    fn os_elected_session_breaks_equal_date_ties() {
        let candidates = [
            candidate("spotify", true, Some(100.0), false),
            candidate("music", true, Some(100.0), true),
        ];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("music")
        );
    }

    #[test]
    fn no_active_session_turns_the_selection_off() {
        let candidates = [
            candidate("spotify", false, Some(100.0), false),
            candidate("music", false, Some(200.0), true),
        ];

        assert!(select_playback_candidate(&candidates).is_none());
    }

    #[test]
    fn unresolved_scoped_playback_state_does_not_revive_a_stale_playback_rate() {
        let mut stale = candidate("music", true, Some(200.0), true);
        stale.playing_resolved = false;
        let candidates = [candidate("spotify", true, Some(100.0), false), stale];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("spotify")
        );
    }

    #[test]
    fn stable_identifier_makes_a_missing_date_tie_deterministic() {
        let candidates = [
            candidate("spotify", true, None, false),
            candidate("music", true, None, false),
        ];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("music")
        );
    }

    #[test]
    fn os_session_payload_uses_camel_case_fields() {
        let candidate: PlaybackCandidate = serde_json::from_str(
            r#"{
                "stableId":"com.apple.Music:default",
                "bundleId":"com.apple.Music",
                "playing":true,
                "playingResolved":true,
                "lastPlayingDate":123.5,
                "elected":true,
                "title":"Song"
            }"#,
        )
        .expect("valid adapter payload");

        assert_eq!(candidate.bundle_id.as_deref(), Some("com.apple.Music"));
        assert_eq!(candidate.last_playing_date, Some(123.5));
        assert_eq!(candidate.title.as_deref(), Some("Song"));
    }

    #[test]
    fn track_key_distinguishes_now_playing_applications() {
        assert_ne!(
            snapshot(Some("com.apple.Music"), Some("Song")).track_key(),
            snapshot(Some("com.spotify.client"), Some("Song")).track_key()
        );
    }

    #[test]
    fn track_key_requires_some_content_identity() {
        let mut empty = snapshot(None, None);
        empty.artist = None;
        empty.album = None;
        assert!(empty.track_key().is_none());
        assert!(snapshot(Some("com.apple.Music"), Some("  "))
            .track_key()
            .is_some());
        assert_eq!(
            snapshot(Some("com.apple.Music"), Some("Song")).track_key(),
            Some("com.apple.Music\u{1f}Song\u{1f}Artist\u{1f}Album\u{1f}0000000000000000".into())
        );
    }

    #[test]
    fn track_key_accepts_artwork_without_a_title_and_tracks_artwork_changes() {
        let mut first = snapshot(Some("com.example.browser"), None);
        first.artist = None;
        first.album = None;
        first.artwork_signature = Some(1);
        let mut second = first.clone();
        second.artwork_signature = Some(2);

        assert!(first.track_key().is_some());
        assert_ne!(first.track_key(), second.track_key());
    }

    #[test]
    fn artwork_signature_is_deterministic_and_sensitive_to_sampled_pixels() {
        use image::{Rgba, RgbaImage};

        let first = DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 3, Rgba([10, 20, 30, 255])));
        let mut changed = first.clone().to_rgba8();
        changed.put_pixel(1, 1, Rgba([30, 20, 10, 255]));
        let changed = DynamicImage::ImageRgba8(changed);

        assert_eq!(artwork_signature(&first), artwork_signature(&first));
        assert_ne!(artwork_signature(&first), artwork_signature(&changed));
    }

    #[test]
    fn artwork_can_be_delivered_again_after_consumer_invalidation() {
        let mut delivery = ArtworkDelivery::default();

        assert!(delivery.should_deliver(Some("track"), true));
        assert!(!delivery.should_deliver(Some("track"), true));
        delivery.invalidate();
        assert!(delivery.should_deliver(Some("track"), true));
    }

    #[test]
    fn resumed_playback_ignores_a_stale_media_remote_timestamp() {
        assert_eq!(
            position_at_reception(
                Some(15.0),
                Some(224.0),
                Some(true),
                Some(1.0),
                Some(4_400.0),
                true,
            ),
            Some(15.0)
        );
    }

    #[test]
    fn initial_playback_projects_a_plausible_media_remote_timestamp() {
        assert_eq!(
            position_at_reception(
                Some(10.0),
                Some(224.0),
                Some(true),
                Some(1.0),
                Some(5.0),
                false,
            ),
            Some(15.0)
        );
    }

    #[test]
    fn impossible_timestamp_projection_falls_back_to_payload_elapsed_time() {
        assert_eq!(
            position_at_reception(
                Some(15.0),
                Some(224.0),
                Some(true),
                Some(1.0),
                Some(4_400.0),
                false,
            ),
            Some(15.0)
        );
    }

    #[test]
    fn impossible_payload_elapsed_time_is_rejected() {
        assert_eq!(
            position_at_reception(
                Some(4_400.0),
                Some(224.0),
                Some(true),
                Some(1.0),
                Some(0.0),
                false,
            ),
            None
        );
    }
}
