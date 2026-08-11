use image::{DynamicImage, GenericImageView};

const POSITION_EDGE_TOLERANCE_SECONDS: f64 = 2.0;

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

    // media-remote 0.5.2 projects elapsed time from MediaRemote's timestamp in
    // get_info(), but that timestamp can remain at the start of a long pause when
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
        path::Path,
        process::{Command, Stdio},
        sync::{Arc, RwLock},
        time::{Instant, SystemTime},
    };

    use anyhow::{bail, Context, Result};
    use media_remote::{ListenerToken, NowPlayingInfo, NowPlayingPerl, Subscription};

    use super::{artwork_signature, position_at_reception, ArtworkDelivery, TrackSnapshot};

    struct ReceivedNowPlaying {
        info: NowPlayingInfo,
        position_at_received: Option<f64>,
        received_at: Instant,
    }

    impl ReceivedNowPlaying {
        fn from_event(info: NowPlayingInfo, previous: Option<&Self>) -> Self {
            let received_at = Instant::now();
            let resumed = previous.is_some_and(|previous| {
                same_track(&previous.info, &info)
                    && previous.info.is_playing == Some(false)
                    && info.is_playing == Some(true)
            });
            let update_age_seconds = info.info_update_time.and_then(|updated_at| {
                SystemTime::now()
                    .duration_since(updated_at)
                    .ok()
                    .map(|age| age.as_secs_f64())
            });
            let position_at_received = position_at_reception(
                info.elapsed_time,
                info.duration,
                info.is_playing,
                info.playback_rate,
                update_age_seconds,
                resumed,
            );
            Self {
                info,
                position_at_received,
                received_at,
            }
        }

        fn elapsed_time(&self) -> Option<f64> {
            let rate = self
                .info
                .playback_rate
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or_else(|| f64::from(self.info.is_playing == Some(true)));
            let projected = self.position_at_received?
                + if self.info.is_playing == Some(true) {
                    self.received_at.elapsed().as_secs_f64() * rate
                } else {
                    0.0
                };
            Some(
                self.info
                    .duration
                    .filter(|duration| duration.is_finite() && *duration > 0.0)
                    .map_or(projected, |duration| projected.min(duration)),
            )
        }
    }

    fn same_track(left: &NowPlayingInfo, right: &NowPlayingInfo) -> bool {
        left.bundle_id == right.bundle_id
            && left.title == right.title
            && left.artist == right.artist
            && left.album == right.album
    }

    pub struct MediaRemoteSource {
        _remote: NowPlayingPerl,
        _subscription: ListenerToken,
        latest: Arc<RwLock<Option<ReceivedNowPlaying>>>,
        artwork_delivery: RefCell<ArtworkDelivery>,
    }

    impl MediaRemoteSource {
        pub fn new() -> Result<Self> {
            if !Path::new("/usr/bin/perl").is_file() {
                bail!("macOS system Perl was not found at /usr/bin/perl");
            }

            let remote = std::panic::catch_unwind(NowPlayingPerl::new).map_err(|_| {
                anyhow::anyhow!("MediaRemote Perl adapter could not be initialized")
            })?;
            let latest = Arc::new(RwLock::new(None::<ReceivedNowPlaying>));
            let listener_latest = Arc::clone(&latest);
            let subscription = remote.subscribe(move |guard| {
                let Some(info) = guard.as_ref().cloned() else {
                    return;
                };
                if let Ok(mut latest) = listener_latest.write() {
                    let received = ReceivedNowPlaying::from_event(info, latest.as_ref());
                    *latest = Some(received);
                }
            });
            Ok(Self {
                _remote: remote,
                _subscription: subscription,
                latest,
                artwork_delivery: RefCell::new(ArtworkDelivery::default()),
            })
        }

        pub fn snapshot(&self) -> Option<TrackSnapshot> {
            let guard = self.latest.read().ok()?;
            let received = guard.as_ref()?;
            let info = &received.info;
            let mut snapshot = TrackSnapshot {
                is_playing: info.is_playing,
                title: info.title.clone(),
                artist: info.artist.clone(),
                album: info.album.clone(),
                bundle_id: info.bundle_id.clone(),
                elapsed_time: received.elapsed_time(),
                duration: info.duration,
                playback_rate: info.playback_rate,
                artwork: None,
                artwork_signature: info.album_cover.as_ref().map(artwork_signature),
            };
            let key = snapshot.track_key();
            let should_copy_artwork = self
                .artwork_delivery
                .borrow_mut()
                .should_deliver(key.as_deref(), info.album_cover.is_some());
            if should_copy_artwork {
                snapshot.artwork = info.album_cover.clone();
            }
            Some(snapshot)
        }

        pub fn invalidate_artwork_delivery(&self) {
            self.artwork_delivery.borrow_mut().invalidate();
        }
    }

    impl Drop for MediaRemoteSource {
        fn drop(&mut self) {
            // media-remote 0.5.2 does not expose its child handle and its reader can
            // remain blocked during Drop. Limit cleanup to adapter children of this
            // exact process so no unrelated Perl process can be affected.
            let _ = Command::new("/usr/bin/pkill")
                .args([
                    "-P",
                    &std::process::id().to_string(),
                    "-f",
                    "mediaremote-adapter.pl",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to stop MediaRemote adapter child");
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
