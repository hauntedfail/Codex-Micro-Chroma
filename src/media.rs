use image::DynamicImage;

#[derive(Clone, Debug)]
pub struct TrackSnapshot {
    pub is_playing: Option<bool>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bundle_id: Option<String>,
    pub artwork: Option<DynamicImage>,
}

impl TrackSnapshot {
    pub fn track_key(&self) -> Option<String> {
        let title = self.title.as_deref()?.trim();
        if title.is_empty() {
            return None;
        }
        Some(format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.bundle_id.as_deref().unwrap_or_default(),
            title,
            self.artist.as_deref().unwrap_or_default(),
            self.album.as_deref().unwrap_or_default()
        ))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        cell::RefCell,
        path::Path,
        process::{Command, Stdio},
    };

    use anyhow::{bail, Context, Result};
    use media_remote::NowPlayingPerl;

    use super::TrackSnapshot;

    pub struct MediaRemoteSource {
        remote: NowPlayingPerl,
        last_artwork_key: RefCell<Option<String>>,
    }

    impl MediaRemoteSource {
        pub fn new() -> Result<Self> {
            if !Path::new("/usr/bin/perl").is_file() {
                bail!("macOS system Perl was not found at /usr/bin/perl");
            }

            let remote = std::panic::catch_unwind(NowPlayingPerl::new).map_err(|_| {
                anyhow::anyhow!("MediaRemote Perl adapter could not be initialized")
            })?;
            Ok(Self {
                remote,
                last_artwork_key: RefCell::new(None),
            })
        }

        pub fn snapshot(&self) -> Option<TrackSnapshot> {
            let guard = self.remote.get_info();
            let info = guard.as_ref()?;
            let mut snapshot = TrackSnapshot {
                is_playing: info.is_playing,
                title: info.title.clone(),
                artist: info.artist.clone(),
                album: info.album.clone(),
                bundle_id: info.bundle_id.clone(),
                artwork: None,
            };
            let key = snapshot.track_key();
            let should_copy_artwork = info.album_cover.is_some()
                && key.is_some()
                && self.last_artwork_key.borrow().as_ref() != key.as_ref();
            if should_copy_artwork {
                snapshot.artwork = info.album_cover.clone();
                *self.last_artwork_key.borrow_mut() = key;
            }
            Some(snapshot)
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
            artwork: None,
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
    fn track_key_requires_a_title() {
        assert!(snapshot(Some("com.apple.Music"), None)
            .track_key()
            .is_none());
        assert!(snapshot(Some("com.apple.Music"), Some("  "))
            .track_key()
            .is_none());
        assert_eq!(
            snapshot(Some("com.apple.Music"), Some("Song")).track_key(),
            Some("com.apple.Music\u{1f}Song\u{1f}Artist\u{1f}Album".into())
        );
    }
}
