use image::DynamicImage;
#[cfg(any(target_os = "macos", test))]
use image::GenericImageView;
#[cfg(any(target_os = "macos", test))]
use serde::Deserialize;

#[cfg(any(target_os = "macos", test))]
const POSITION_EDGE_TOLERANCE_SECONDS: f64 = 2.0;

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
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
    #[cfg(any(target_os = "macos", test))]
    fn clone_without_artwork(&self) -> Self {
        Self {
            is_playing: self.is_playing,
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            bundle_id: self.bundle_id.clone(),
            elapsed_time: self.elapsed_time,
            duration: self.duration,
            playback_rate: self.playback_rate,
            artwork: None,
            artwork_signature: self.artwork_signature,
        }
    }

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

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
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
#[cfg(any(target_os = "macos", test))]
struct ArtworkDelivery {
    last_key: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
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
        io::{BufRead, BufReader, Cursor},
        os::unix::fs::PermissionsExt,
        process::{Child, Command, Stdio},
        sync::{mpsc, Arc, RwLock},
        thread::{self, JoinHandle},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use anyhow::{bail, Context, Result};
    use base64::{engine::general_purpose, Engine as _};
    use image::{ImageReader, Limits};
    use serde::Deserialize;
    use tempfile::TempDir;

    use super::{
        artwork_signature, position_at_reception, select_playback_candidate, ArtworkDelivery,
        PlaybackCandidate, TrackSnapshot,
    };

    const MEDIA_SESSIONS_HELPER: &[u8] = include_bytes!(concat!(
        env!("OUT_DIR"),
        "/codex_micro_chroma_media_sessions"
    ));
    const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(2);
    const MAX_HELPER_JSON_LINE_BYTES: usize = 24 * 1024 * 1024;
    const MAX_ENCODED_ARTWORK_BYTES: usize = 12 * 1024 * 1024;
    const MAX_RAW_ARTWORK_BYTES: usize = 8 * 1024 * 1024;
    const MAX_IMAGE_WIDTH: u32 = 4096;
    const MAX_IMAGE_HEIGHT: u32 = 4096;
    const MAX_IMAGE_DECODER_ALLOC_BYTES: u64 = 96 * 1024 * 1024;

    #[derive(Deserialize)]
    struct SessionPayload {
        candidates: Vec<PlaybackCandidate>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum HelperControlMessage {
        Ready,
        Reset { reason: Option<String> },
        Invalid(String),
    }

    fn parse_helper_control_line(line: &str) -> HelperControlMessage {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(serde_json::Value::Object(map))
                if map
                    .get("ready")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false) =>
            {
                HelperControlMessage::Ready
            }
            Ok(serde_json::Value::Object(map)) if map.contains_key("reset") => {
                HelperControlMessage::Reset {
                    reason: map
                        .get("reset")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                }
            }
            _ => HelperControlMessage::Invalid(line.to_owned()),
        }
    }

    #[derive(Clone)]
    struct ReceivedNowPlaying {
        snapshot: TrackSnapshot,
        artwork: Option<CachedArtwork>,
        position_at_received: Option<f64>,
        received_at: Instant,
    }

    #[derive(Default)]
    struct NowPlayingState {
        latest: Option<ReceivedNowPlaying>,
        pending_reset: Option<ReceivedNowPlaying>,
    }

    struct CachedArtwork {
        key: Arc<ArtworkCacheKey>,
        image: Arc<image::DynamicImage>,
        signature: u64,
    }

    impl Clone for CachedArtwork {
        fn clone(&self) -> Self {
            Self {
                key: self.key.clone(),
                image: Arc::clone(&self.image),
                signature: self.signature,
            }
        }
    }

    #[derive(Clone, PartialEq, Eq)]
    struct ArtworkCacheKey {
        stable_id: String,
        encoded_artwork: String,
    }

    impl ArtworkCacheKey {
        fn matches_candidate(&self, candidate: &PlaybackCandidate, encoded_artwork: &str) -> bool {
            self.stable_id == candidate.stable_id.as_str()
                && self.encoded_artwork == encoded_artwork
        }
    }

    impl ReceivedNowPlaying {
        fn from_candidate(candidate: Option<&PlaybackCandidate>, previous: Option<&Self>) -> Self {
            let received_at = Instant::now();
            let Some(candidate) = candidate else {
                return Self::stopped_from_previous(previous, received_at);
            };
            let artwork = Self::artwork_from_candidate(candidate, previous);
            let mut snapshot = TrackSnapshot {
                is_playing: Some(candidate.playing),
                title: candidate.title.clone(),
                artist: candidate.artist.clone(),
                album: candidate.album.clone(),
                bundle_id: candidate.bundle_id.clone(),
                elapsed_time: candidate.elapsed_time,
                duration: candidate.duration,
                playback_rate: candidate.playback_rate,
                artwork_signature: artwork.as_ref().map(|artwork| artwork.signature),
                artwork: None,
            };
            let resumed = previous.is_some_and(|previous| {
                same_track(&previous.snapshot, &snapshot)
                    && previous.snapshot.is_playing == Some(false)
                    && snapshot.is_playing == Some(true)
            });
            let info_update_time = candidate
                .info_update_date
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
                artwork,
                position_at_received,
                received_at,
            }
        }

        fn artwork_from_candidate(
            candidate: &PlaybackCandidate,
            previous: Option<&Self>,
        ) -> Option<CachedArtwork> {
            let encoded_artwork = candidate.artwork_data.as_ref()?;
            if encoded_artwork.len() > MAX_ENCODED_ARTWORK_BYTES {
                return None;
            }
            if let Some(artwork) = previous
                .and_then(|previous| previous.artwork.as_ref())
                .filter(|artwork| artwork.key.matches_candidate(candidate, encoded_artwork))
            {
                return Some(artwork.clone());
            }
            let bytes = general_purpose::STANDARD.decode(encoded_artwork).ok()?;
            if bytes.len() > MAX_RAW_ARTWORK_BYTES {
                return None;
            }
            let mut reader = ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .ok()?;
            let mut limits = Limits::default();
            limits.max_image_width = Some(MAX_IMAGE_WIDTH);
            limits.max_image_height = Some(MAX_IMAGE_HEIGHT);
            limits.max_alloc = Some(MAX_IMAGE_DECODER_ALLOC_BYTES);
            reader.limits(limits);
            let image = reader.decode().ok()?;
            let signature = artwork_signature(&image);
            Some(CachedArtwork {
                key: Arc::new(ArtworkCacheKey {
                    stable_id: candidate.stable_id.clone(),
                    encoded_artwork: encoded_artwork.clone(),
                }),
                image: Arc::new(image),
                signature,
            })
        }

        fn stopped_from_previous(previous: Option<&Self>, received_at: Instant) -> Self {
            let mut snapshot = previous.map_or_else(
                || TrackSnapshot {
                    is_playing: Some(false),
                    title: None,
                    artist: None,
                    album: None,
                    bundle_id: None,
                    elapsed_time: None,
                    duration: None,
                    playback_rate: None,
                    artwork: None,
                    artwork_signature: None,
                },
                |previous| previous.snapshot.clone_without_artwork(),
            );
            snapshot.is_playing = Some(false);
            snapshot.artwork = None;
            let position_at_received = previous.and_then(Self::elapsed_time);
            snapshot.elapsed_time = position_at_received;
            Self {
                snapshot,
                artwork: None,
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

    enum StoppedDelivery {
        Authoritative,
        OneShotReset,
    }

    fn publish_stopped(
        state: &RwLock<NowPlayingState>,
        reason: &str,
        delivery: StoppedDelivery,
    ) -> std::io::Result<()> {
        let mut state = state
            .write()
            .map_err(|_| std::io::Error::other("MediaRemote state lock is poisoned"))?;
        let stopped = ReceivedNowPlaying::from_candidate(None, state.latest.as_ref());
        state.latest = Some(stopped.clone());
        if matches!(delivery, StoppedDelivery::OneShotReset) {
            state.pending_reset = Some(stopped);
        }
        eprintln!("MediaRemote session helper stopped: {reason}; clearing Now Playing state");
        Ok(())
    }

    fn process_started_helper_line(
        state: &RwLock<NowPlayingState>,
        reported_parse_error: &mut bool,
        line: &str,
    ) {
        match parse_helper_control_line(line) {
            HelperControlMessage::Ready => {
                return;
            }
            HelperControlMessage::Reset { reason } => {
                let reason = reason.as_deref().unwrap_or("control reset requested");
                let _ = publish_stopped(state, reason, StoppedDelivery::OneShotReset);
                return;
            }
            HelperControlMessage::Invalid(_) => {}
        }
        let payload = match serde_json::from_str::<SessionPayload>(line) {
            Ok(payload) => payload,
            Err(error) => {
                if !*reported_parse_error {
                    *reported_parse_error = true;
                    eprintln!("MediaRemote session helper sent an unparsable payload: {error}");
                }
                return;
            }
        };
        let selected = select_playback_candidate(&payload.candidates);
        if let Ok(mut state) = state.write() {
            state.latest = Some(ReceivedNowPlaying::from_candidate(
                selected,
                state.latest.as_ref(),
            ));
        }
    }

    fn read_bounded_line<R: BufRead>(
        reader: &mut R,
        max_bytes: usize,
    ) -> std::io::Result<Option<String>> {
        let mut bytes = Vec::new();
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if bytes.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let newline_index = available.iter().position(|byte| *byte == b'\n');
            let segment_len = newline_index.map_or(available.len(), |index| index + 1);
            let remaining = max_bytes.saturating_sub(bytes.len());
            if segment_len > remaining {
                reader.consume(remaining);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("helper JSON line exceeded {max_bytes} bytes"),
                ));
            }
            bytes.extend_from_slice(&available[..segment_len]);
            reader.consume(segment_len);
            if newline_index.is_some() {
                break;
            }
        }
        if bytes.ends_with(b"\n") {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        String::from_utf8(bytes).map(Some).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("helper JSON line was not UTF-8: {error}"),
            )
        })
    }

    fn process_started_helper_stream<R: BufRead>(
        state: &RwLock<NowPlayingState>,
        reader: &mut R,
        publish_stopped_on_eof: bool,
    ) -> std::io::Result<()> {
        let mut reported_parse_error = false;
        loop {
            match read_bounded_line(reader, MAX_HELPER_JSON_LINE_BYTES) {
                Ok(Some(line)) => {
                    process_started_helper_line(state, &mut reported_parse_error, &line);
                }
                Ok(None) if publish_stopped_on_eof => {
                    return publish_stopped(
                        state,
                        "stdout reached EOF",
                        StoppedDelivery::Authoritative,
                    );
                }
                Ok(None) => return Ok(()),
                Err(error) => {
                    let _ = publish_stopped(
                        state,
                        &format!("stdout read error: {error}"),
                        StoppedDelivery::Authoritative,
                    );
                    return Err(error);
                }
            }
        }
    }

    fn snapshot_from_state(
        state: &RwLock<NowPlayingState>,
        artwork_delivery: &RefCell<ArtworkDelivery>,
    ) -> Option<TrackSnapshot> {
        let (received, from_reset) = {
            let mut state = state.write().ok()?;
            if let Some(received) = state.pending_reset.take() {
                (received, true)
            } else {
                (state.latest.as_ref()?.clone(), false)
            }
        };
        if from_reset {
            artwork_delivery.borrow_mut().invalidate();
        }
        let key = received.snapshot.track_key();
        let should_copy_artwork = artwork_delivery
            .borrow_mut()
            .should_deliver(key.as_deref(), !from_reset && received.artwork.is_some());
        let mut snapshot = TrackSnapshot {
            artwork: if should_copy_artwork {
                received
                    .artwork
                    .as_ref()
                    .map(|artwork| (*artwork.image).clone())
            } else {
                None
            },
            ..received.snapshot.clone_without_artwork()
        };
        snapshot.elapsed_time = received.elapsed_time();
        Some(snapshot)
    }

    fn terminate_helper_before_ready(child: &mut Child, reader: JoinHandle<()>) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
    }

    pub struct MediaRemoteSource {
        child: Child,
        reader: Option<JoinHandle<()>>,
        _temp_dir: TempDir,
        state: Arc<RwLock<NowPlayingState>>,
        artwork_delivery: RefCell<ArtworkDelivery>,
    }

    impl MediaRemoteSource {
        pub fn new() -> Result<Self> {
            let temp_dir = tempfile::Builder::new()
                .prefix("codex-micro-chroma-media-sessions")
                .tempdir()
                .context("could not create MediaRemote helper directory")?;
            let helper_path = temp_dir.path().join("media_sessions");
            fs::write(&helper_path, MEDIA_SESSIONS_HELPER)
                .context("could not extract MediaRemote session helper")?;
            let mut permissions = fs::metadata(&helper_path)
                .context("could not stat MediaRemote session helper")?
                .permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&helper_path, permissions)
                .context("could not make MediaRemote session helper executable")?;

            let mut child = Command::new(&helper_path)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .context("could not start MediaRemote session helper")?;
            let stdout = child
                .stdout
                .take()
                .context("MediaRemote session helper stdout is unavailable")?;
            let state = Arc::new(RwLock::new(NowPlayingState::default()));
            let reader_state = Arc::clone(&state);
            let (ready_tx, ready_rx) = mpsc::channel();
            let reader = thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                match read_bounded_line(&mut reader, MAX_HELPER_JSON_LINE_BYTES) {
                    Ok(Some(line)) => match parse_helper_control_line(&line) {
                        HelperControlMessage::Ready => {
                            let _ = ready_tx.send(Ok(()));
                        }
                        HelperControlMessage::Reset { .. } => {
                            let _ = ready_tx.send(Err(format!(
                                "MediaRemote session helper reset before ready: {line}"
                            )));
                            return;
                        }
                        HelperControlMessage::Invalid(line) => {
                            let _ = ready_tx.send(Err(format!(
                                "malformed pre-ready output from MediaRemote session helper: {line}"
                            )));
                            return;
                        }
                    },
                    Ok(None) => {
                        let _ = ready_tx.send(Err(
                            "MediaRemote session helper exited before ready".to_owned(),
                        ));
                        return;
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!(
                            "could not read MediaRemote session helper ready message: {error}"
                        )));
                        return;
                    }
                }
                let _ = process_started_helper_stream(&reader_state, &mut reader, true);
            });
            let ready_error = match ready_rx.recv_timeout(HELPER_READY_TIMEOUT) {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(mpsc::RecvTimeoutError::Timeout) => Some(format!(
                    "MediaRemote session helper did not become ready within {HELPER_READY_TIMEOUT:?}"
                )),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Some("MediaRemote session helper reader stopped before ready".to_owned())
                }
            };
            if let Some(error) = ready_error {
                terminate_helper_before_ready(&mut child, reader);
                bail!("{error}");
            }
            Ok(Self {
                child,
                reader: Some(reader),
                _temp_dir: temp_dir,
                state,
                artwork_delivery: RefCell::new(ArtworkDelivery::default()),
            })
        }

        pub fn snapshot(&self) -> Option<TrackSnapshot> {
            snapshot_from_state(&self.state, &self.artwork_delivery)
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use image::{ImageFormat, Rgba, RgbaImage};
        use std::io::Cursor;

        fn candidate(title: &str, elapsed_time: f64, info_update_date: f64) -> PlaybackCandidate {
            PlaybackCandidate {
                stable_id: "music".into(),
                bundle_id: Some("com.apple.Music".into()),
                playing: true,
                playing_resolved: true,
                last_playing_date: Some(100.0),
                elected: true,
                title: Some(title.into()),
                artist: Some("Artist".into()),
                album: Some("Album".into()),
                elapsed_time: Some(elapsed_time),
                duration: Some(240.0),
                playback_rate: Some(1.0),
                info_update_date: Some(info_update_date),
                artwork_data: None,
            }
        }

        fn encoded_artwork(red: u8, green: u8, blue: u8) -> String {
            let image = image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                2,
                2,
                Rgba([red, green, blue, 255]),
            ));
            let mut bytes = Vec::new();
            image
                .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
                .expect("test image encodes");
            general_purpose::STANDARD.encode(bytes)
        }

        fn candidate_with_artwork(stable_id: &str, encoded_artwork: String) -> PlaybackCandidate {
            PlaybackCandidate {
                stable_id: stable_id.into(),
                artwork_data: Some(encoded_artwork),
                ..candidate("Song", 30.0, 0.0)
            }
        }

        fn identity_only_candidate(stable_id: &str, bundle_id: &str) -> PlaybackCandidate {
            PlaybackCandidate {
                stable_id: stable_id.into(),
                bundle_id: Some(bundle_id.into()),
                playing: true,
                playing_resolved: true,
                last_playing_date: Some(200.0),
                elected: true,
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
        fn stopped_state_preserves_identity_without_artwork_for_resume() {
            let playing =
                ReceivedNowPlaying::from_candidate(Some(&candidate("Song", 30.0, 0.0)), None);
            let stopped = ReceivedNowPlaying::from_candidate(None, Some(&playing));

            assert_eq!(stopped.snapshot.is_playing, Some(false));
            assert_eq!(stopped.snapshot.title.as_deref(), Some("Song"));
            assert_eq!(stopped.snapshot.artist.as_deref(), Some("Artist"));
            assert_eq!(stopped.snapshot.album.as_deref(), Some("Album"));
            assert_eq!(
                stopped.snapshot.bundle_id.as_deref(),
                Some("com.apple.Music")
            );
            assert!(stopped.snapshot.artwork.is_none());
            assert!(same_track(&playing.snapshot, &stopped.snapshot));
        }

        #[test]
        fn resume_after_stopped_state_anchors_at_fresh_elapsed_time() {
            let stale_update_time = 0.0;
            let playing = ReceivedNowPlaying::from_candidate(
                Some(&candidate("Song", 30.0, stale_update_time)),
                None,
            );
            let stopped = ReceivedNowPlaying::from_candidate(None, Some(&playing));
            let resumed = ReceivedNowPlaying::from_candidate(
                Some(&candidate("Song", 42.0, stale_update_time)),
                Some(&stopped),
            );

            assert_eq!(resumed.snapshot.is_playing, Some(true));
            assert_eq!(resumed.position_at_received, Some(42.0));
            assert_eq!(resumed.snapshot.elapsed_time, Some(42.0));
        }

        #[test]
        fn publish_stopped_overwrites_a_cached_playing_snapshot() {
            let state = RwLock::new(NowPlayingState {
                latest: Some(ReceivedNowPlaying::from_candidate(
                    Some(&candidate("Song", 30.0, 0.0)),
                    None,
                )),
                pending_reset: None,
            });

            publish_stopped(&state, "test termination", StoppedDelivery::Authoritative)
                .expect("state update succeeds");

            let state = state.read().expect("state lock is readable");
            let snapshot = &state
                .latest
                .as_ref()
                .expect("stopped state is published")
                .snapshot;
            assert_eq!(snapshot.is_playing, Some(false));
            assert_eq!(snapshot.title.as_deref(), Some("Song"));
            assert!(snapshot.artwork.is_none());
            assert!(state.pending_reset.is_none());
        }

        #[test]
        fn unchanged_session_and_artwork_reuses_decoded_image() {
            let encoded_artwork = encoded_artwork(10, 20, 30);
            let first_candidate = candidate_with_artwork("music", encoded_artwork.clone());
            let first = ReceivedNowPlaying::from_candidate(Some(&first_candidate), None);
            let second_candidate = PlaybackCandidate {
                elapsed_time: Some(42.0),
                info_update_date: Some(10.0),
                ..candidate_with_artwork("music", encoded_artwork)
            };

            let second = ReceivedNowPlaying::from_candidate(Some(&second_candidate), Some(&first));

            let first_artwork = first.artwork.as_ref().expect("first artwork decodes");
            let second_artwork = second.artwork.as_ref().expect("second artwork is present");
            assert!(Arc::ptr_eq(&first_artwork.key, &second_artwork.key));
            assert!(Arc::ptr_eq(&first_artwork.image, &second_artwork.image));
            assert_eq!(first_artwork.signature, second_artwork.signature);
            assert!(second.snapshot.artwork.is_none());
        }

        #[test]
        fn changed_session_identity_decodes_artwork_again() {
            let encoded_artwork = encoded_artwork(10, 20, 30);
            let first_candidate = candidate_with_artwork("music", encoded_artwork.clone());
            let first = ReceivedNowPlaying::from_candidate(Some(&first_candidate), None);
            let second_candidate = candidate_with_artwork("spotify", encoded_artwork);

            let second = ReceivedNowPlaying::from_candidate(Some(&second_candidate), Some(&first));

            let first_artwork = first.artwork.as_ref().expect("first artwork decodes");
            let second_artwork = second.artwork.as_ref().expect("second artwork decodes");
            assert!(!Arc::ptr_eq(&first_artwork.image, &second_artwork.image));
            assert_eq!(first_artwork.signature, second_artwork.signature);
        }

        #[test]
        fn changed_encoded_artwork_decodes_and_updates_signature() {
            let first_candidate = candidate_with_artwork("music", encoded_artwork(10, 20, 30));
            let first = ReceivedNowPlaying::from_candidate(Some(&first_candidate), None);
            let second_candidate = candidate_with_artwork("music", encoded_artwork(30, 20, 10));

            let second = ReceivedNowPlaying::from_candidate(Some(&second_candidate), Some(&first));

            let first_artwork = first.artwork.as_ref().expect("first artwork decodes");
            let second_artwork = second.artwork.as_ref().expect("second artwork decodes");
            assert!(!Arc::ptr_eq(&first_artwork.image, &second_artwork.image));
            assert_ne!(first_artwork.signature, second_artwork.signature);
            assert_ne!(
                first.snapshot.artwork_signature,
                second.snapshot.artwork_signature
            );
        }

        #[test]
        fn stopped_state_clears_cached_artwork() {
            let playing = ReceivedNowPlaying::from_candidate(
                Some(&candidate_with_artwork(
                    "music",
                    encoded_artwork(10, 20, 30),
                )),
                None,
            );

            let stopped = ReceivedNowPlaying::from_candidate(None, Some(&playing));

            assert!(stopped.artwork.is_none());
            assert!(stopped.snapshot.artwork_signature.is_some());
            assert!(stopped.snapshot.artwork.is_none());
        }

        #[test]
        fn identity_only_playing_candidate_clears_stale_metadata_and_artwork() {
            let playing = ReceivedNowPlaying::from_candidate(
                Some(&candidate_with_artwork(
                    "music",
                    encoded_artwork(10, 20, 30),
                )),
                None,
            );

            let identity_only = ReceivedNowPlaying::from_candidate(
                Some(&identity_only_candidate("music", "com.apple.Music")),
                Some(&playing),
            );

            assert_eq!(identity_only.snapshot.is_playing, Some(true));
            assert_eq!(
                identity_only.snapshot.bundle_id.as_deref(),
                Some("com.apple.Music")
            );
            assert_eq!(identity_only.snapshot.title, None);
            assert_eq!(identity_only.snapshot.artist, None);
            assert_eq!(identity_only.snapshot.album, None);
            assert!(identity_only.artwork.is_none());
            assert!(identity_only.snapshot.artwork.is_none());
            assert_eq!(identity_only.snapshot.artwork_signature, None);
            assert_ne!(
                identity_only.snapshot.track_key(),
                playing.snapshot.track_key()
            );
        }

        #[test]
        fn helper_control_line_accepts_ready_message() {
            assert_eq!(
                parse_helper_control_line(r#"{"ready":true}"#),
                HelperControlMessage::Ready
            );
        }

        #[test]
        fn helper_control_line_accepts_reset_message() {
            assert_eq!(
                parse_helper_control_line(r#"{"reset":"timeout"}"#),
                HelperControlMessage::Reset {
                    reason: Some("timeout".to_owned())
                }
            );
        }

        #[test]
        fn helper_control_line_rejects_session_payload_before_ready() {
            assert_eq!(
                parse_helper_control_line(r#"{"candidates":[]}"#),
                HelperControlMessage::Invalid(r#"{"candidates":[]}"#.to_owned())
            );
        }

        #[test]
        fn helper_control_line_rejects_malformed_before_ready() {
            assert_eq!(
                parse_helper_control_line("not json"),
                HelperControlMessage::Invalid("not json".to_owned())
            );
        }

        #[test]
        fn reset_control_line_after_ready_clears_cached_playing_state() {
            let state = RwLock::new(NowPlayingState {
                latest: Some(ReceivedNowPlaying::from_candidate(
                    Some(&candidate("Song", 30.0, 0.0)),
                    None,
                )),
                pending_reset: None,
            });
            let mut reported_parse_error = false;

            process_started_helper_line(
                &state,
                &mut reported_parse_error,
                r#"{"reset":"timeout"}"#,
            );

            assert!(!reported_parse_error);
            let state = state.read().expect("state lock is readable");
            let snapshot = &state
                .latest
                .as_ref()
                .expect("stopped state is published")
                .snapshot;
            assert_eq!(snapshot.is_playing, Some(false));
            assert_eq!(snapshot.title.as_deref(), Some("Song"));
            assert!(snapshot.artwork.is_none());
            assert!(state.pending_reset.is_some());
        }

        #[test]
        fn ready_control_line_after_reset_is_not_reported_as_parse_error() {
            let state = RwLock::new(NowPlayingState {
                latest: Some(ReceivedNowPlaying::from_candidate(
                    Some(&candidate("Song", 30.0, 0.0)),
                    None,
                )),
                pending_reset: None,
            });
            let mut reported_parse_error = false;

            process_started_helper_line(
                &state,
                &mut reported_parse_error,
                r#"{"reset":"timeout"}"#,
            );
            process_started_helper_line(&state, &mut reported_parse_error, r#"{"ready":true}"#);

            assert!(!reported_parse_error);
            let state = state.read().expect("state lock is readable");
            assert_eq!(
                state
                    .latest
                    .as_ref()
                    .expect("state remains published")
                    .snapshot
                    .is_playing,
                Some(false)
            );
        }

        #[test]
        fn post_ready_stream_accepts_reset_ready_and_payload_without_eof() {
            let state = RwLock::new(NowPlayingState {
                latest: Some(ReceivedNowPlaying::from_candidate(
                    Some(&candidate("Old Song", 30.0, 0.0)),
                    None,
                )),
                pending_reset: None,
            });
            let payload = r#"{"candidates":[{"stableId":"music","bundleId":"com.apple.Music","playing":true,"playingResolved":true,"lastPlayingDate":500.0,"elected":true,"title":"New Song"}]}"#;
            let mut stream = Cursor::new(format!(
                "{{\"reset\":\"timeout\"}}\n{{\"ready\":true}}\n{payload}\n"
            ));

            process_started_helper_stream(&state, &mut stream, false)
                .expect("stream lines are processed");

            let state = state.read().expect("state lock is readable");
            let snapshot = &state
                .latest
                .as_ref()
                .expect("payload is published")
                .snapshot;
            assert_eq!(snapshot.is_playing, Some(true));
            assert_eq!(snapshot.title.as_deref(), Some("New Song"));
            assert_eq!(snapshot.bundle_id.as_deref(), Some("com.apple.Music"));
        }

        #[test]
        fn reset_delivery_is_sticky_until_snapshot_observes_it() {
            let state = RwLock::new(NowPlayingState {
                latest: Some(ReceivedNowPlaying::from_candidate(
                    Some(&candidate("Old Song", 30.0, 0.0)),
                    None,
                )),
                pending_reset: None,
            });
            let artwork_delivery = RefCell::new(ArtworkDelivery::default());
            let mut reported_parse_error = false;
            let replacement = r#"{"candidates":[{"stableId":"music","bundleId":"com.apple.Music","playing":true,"playingResolved":true,"lastPlayingDate":500.0,"elected":true,"title":"New Song"}]}"#;

            process_started_helper_line(
                &state,
                &mut reported_parse_error,
                r#"{"reset":"timeout"}"#,
            );
            process_started_helper_line(&state, &mut reported_parse_error, replacement);

            let stopped =
                snapshot_from_state(&state, &artwork_delivery).expect("reset is delivered first");
            let playing = snapshot_from_state(&state, &artwork_delivery)
                .expect("replacement is delivered after reset");

            assert!(!reported_parse_error);
            assert_eq!(stopped.is_playing, Some(false));
            assert_eq!(stopped.title.as_deref(), Some("Old Song"));
            assert!(stopped.artwork.is_none());
            assert_eq!(playing.is_playing, Some(true));
            assert_eq!(playing.title.as_deref(), Some("New Song"));
        }

        #[test]
        fn multiple_resets_coalesce_into_one_stopped_snapshot() {
            let state = RwLock::new(NowPlayingState {
                latest: Some(ReceivedNowPlaying::from_candidate(
                    Some(&candidate("Song", 30.0, 0.0)),
                    None,
                )),
                pending_reset: None,
            });
            let artwork_delivery = RefCell::new(ArtworkDelivery::default());
            let mut reported_parse_error = false;

            process_started_helper_line(
                &state,
                &mut reported_parse_error,
                r#"{"reset":"timeout"}"#,
            );
            process_started_helper_line(
                &state,
                &mut reported_parse_error,
                r#"{"reset":"timeout"}"#,
            );

            let stopped =
                snapshot_from_state(&state, &artwork_delivery).expect("coalesced reset is present");
            let next = snapshot_from_state(&state, &artwork_delivery)
                .expect("latest state remains present");

            assert!(!reported_parse_error);
            assert_eq!(stopped.is_playing, Some(false));
            assert!(stopped.artwork.is_none());
            assert_eq!(next.is_playing, Some(false));
        }

        #[test]
        fn reset_snapshot_omits_artwork_and_allows_replacement_redelivery() {
            let encoded_artwork = encoded_artwork(10, 20, 30);
            let playing = ReceivedNowPlaying::from_candidate(
                Some(&candidate_with_artwork("music", encoded_artwork.clone())),
                None,
            );
            let state = RwLock::new(NowPlayingState {
                latest: Some(playing),
                pending_reset: None,
            });
            let artwork_delivery = RefCell::new(ArtworkDelivery::default());
            let mut reported_parse_error = false;
            let initial =
                snapshot_from_state(&state, &artwork_delivery).expect("initial artwork snapshot");
            let replacement = format!(
                r#"{{"candidates":[{{"stableId":"music","bundleId":"com.apple.Music","playing":true,"playingResolved":true,"lastPlayingDate":500.0,"elected":true,"title":"Song","artist":"Artist","album":"Album","artworkData":"{encoded_artwork}"}}]}}"#
            );

            process_started_helper_line(
                &state,
                &mut reported_parse_error,
                r#"{"reset":"timeout"}"#,
            );
            process_started_helper_line(&state, &mut reported_parse_error, &replacement);

            let stopped =
                snapshot_from_state(&state, &artwork_delivery).expect("reset is delivered first");
            let redelivered = snapshot_from_state(&state, &artwork_delivery)
                .expect("replacement is delivered after reset");

            assert!(!reported_parse_error);
            assert!(initial.artwork.is_some());
            assert_eq!(stopped.is_playing, Some(false));
            assert!(stopped.artwork.is_none());
            assert_eq!(redelivered.is_playing, Some(true));
            assert!(redelivered.artwork.is_some());
        }

        #[test]
        fn bounded_line_reader_rejects_oversized_helper_line_without_growing_past_cap() {
            let mut stream = Cursor::new(vec![b'a'; 17]);

            let error = read_bounded_line(&mut stream, 16).expect_err("line exceeds cap");

            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(error
                .to_string()
                .contains("helper JSON line exceeded 16 bytes"));
        }

        #[test]
        fn oversized_encoded_artwork_is_omitted_without_dropping_candidate() {
            let candidate =
                candidate_with_artwork("music", "A".repeat(MAX_ENCODED_ARTWORK_BYTES + 1));

            let received = ReceivedNowPlaying::from_candidate(Some(&candidate), None);

            assert_eq!(received.snapshot.is_playing, Some(true));
            assert!(received.artwork.is_none());
            assert_eq!(received.snapshot.artwork_signature, None);
        }

        #[test]
        fn oversized_raw_artwork_is_omitted_without_dropping_candidate() {
            let candidate = candidate_with_artwork(
                "music",
                general_purpose::STANDARD.encode(vec![0_u8; MAX_RAW_ARTWORK_BYTES + 1]),
            );

            let received = ReceivedNowPlaying::from_candidate(Some(&candidate), None);

            assert_eq!(received.snapshot.is_playing, Some(true));
            assert!(received.artwork.is_none());
            assert_eq!(received.snapshot.artwork_signature, None);
        }

        #[test]
        fn oversized_image_dimensions_are_omitted_without_dropping_candidate() {
            let image = image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                MAX_IMAGE_WIDTH + 1,
                1,
                Rgba([10, 20, 30, 255]),
            ));
            let mut bytes = Vec::new();
            image
                .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
                .expect("test image encodes");
            let candidate =
                candidate_with_artwork("music", general_purpose::STANDARD.encode(bytes));

            let received = ReceivedNowPlaying::from_candidate(Some(&candidate), None);

            assert_eq!(received.snapshot.is_playing, Some(true));
            assert!(received.artwork.is_none());
            assert_eq!(received.snapshot.artwork_signature, None);
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
    fn os_elected_session_breaks_missing_date_ties() {
        let candidates = [
            candidate("spotify", true, None, false),
            candidate("music", true, None, true),
        ];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("music")
        );
    }

    #[test]
    fn missing_date_candidate_still_participates_in_stop_fallback() {
        let candidates = [
            candidate("spotify", false, Some(300.0), true),
            candidate("music", true, None, false),
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
        stale.playback_rate = Some(1.0);
        let candidates = [candidate("spotify", true, Some(100.0), false), stale];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("spotify")
        );
    }

    #[test]
    fn unresolved_scoped_playback_state_does_not_prevent_stop_fallback() {
        let mut unresolved_newer = candidate("music", true, Some(300.0), true);
        unresolved_newer.playing_resolved = false;
        unresolved_newer.playback_rate = Some(1.0);
        let candidates = [
            candidate("spotify", true, Some(100.0), false),
            unresolved_newer,
        ];

        assert_eq!(
            select_playback_candidate(&candidates).map(|value| value.stable_id.as_str()),
            Some("spotify")
        );
    }

    #[test]
    fn unresolved_scoped_playback_state_can_turn_selection_off() {
        let mut unresolved = candidate("music", true, Some(300.0), true);
        unresolved.playing_resolved = false;
        unresolved.playback_rate = Some(1.0);
        let candidates = [candidate("spotify", false, Some(100.0), false), unresolved];

        assert!(select_playback_candidate(&candidates).is_none());
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
    fn metadata_less_playing_candidate_remains_selectable() {
        let mut metadata_less = candidate("music", true, Some(200.0), false);
        metadata_less.bundle_id = Some("com.apple.Music".into());
        let candidates = [candidate("spotify", true, Some(100.0), true), metadata_less];

        let selected = select_playback_candidate(&candidates).expect("active candidate selected");

        assert_eq!(selected.stable_id, "music");
        assert_eq!(selected.bundle_id.as_deref(), Some("com.apple.Music"));
        assert_eq!(selected.title, None);
        assert_eq!(selected.artwork_data, None);
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
        .expect("valid helper payload");

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
    fn clone_without_artwork_preserves_non_artwork_fields() {
        use image::{Rgba, RgbaImage};

        let mut original = snapshot(Some("com.apple.Music"), Some("Song"));
        original.elapsed_time = Some(42.0);
        original.duration = Some(240.0);
        original.playback_rate = Some(0.5);
        original.artwork_signature = Some(7);
        original.artwork = Some(DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            1,
            1,
            Rgba([10, 20, 30, 255]),
        )));

        let cloned = original.clone_without_artwork();

        assert_eq!(cloned.is_playing, original.is_playing);
        assert_eq!(cloned.title, original.title);
        assert_eq!(cloned.artist, original.artist);
        assert_eq!(cloned.album, original.album);
        assert_eq!(cloned.bundle_id, original.bundle_id);
        assert_eq!(cloned.elapsed_time, original.elapsed_time);
        assert_eq!(cloned.duration, original.duration);
        assert_eq!(cloned.playback_rate, original.playback_rate);
        assert_eq!(cloned.artwork_signature, original.artwork_signature);
        assert!(cloned.artwork.is_none());
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
