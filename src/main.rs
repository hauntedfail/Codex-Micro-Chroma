use std::{
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use codex_micro_chroma::{
    color::{ambient_color, Rgb},
    hid,
    lighting::{LightingComposer, LightingScene},
    media::{MediaRemoteSource, TrackSnapshot},
    protocol::LightingEffect,
    service,
    system_audio::SystemAudioSource,
    telemetry::{default_track_log_directory, FinishedTrack, TrackLogger},
};

static NEXT_REQUEST_ID: AtomicU16 = AtomicU16::new(1);
const STARTUP_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const AUDIO_FRAME_STALL_THRESHOLD: Duration = Duration::from_secs(3);
const AUDIO_ZERO_RECOVERY_THRESHOLD: Duration = Duration::from_secs(15);
const AUDIO_RECOVERY_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AudioRecoveryReason {
    Missing,
    Stalled,
    ZeroFilled,
}

impl std::fmt::Display for AudioRecoveryReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "audio source is unavailable",
            Self::Stalled => "audio callbacks stopped advancing",
            Self::ZeroFilled => "audio callbacks remained zero-filled",
        })
    }
}

#[derive(Parser)]
#[command(version, about = "macOS Now Playing artwork lighting for Codex Micro")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the local MediaRemote and Codex Micro boundaries.
    Probe,
    /// Print the current local Now Playing information and detected color.
    Status(StatusArgs),
    /// Print real-time audio features from a Core Audio Process Tap as JSON lines.
    AudioProbe(AudioProbeArgs),
    /// Follow macOS Now Playing and keep the Codex Micro ambient ring updated.
    Run(RunArgs),
    /// Set one ambient-ring color without reading Now Playing.
    Set(SetArgs),
    /// Turn off the key and ambient lighting controlled by this tool.
    Off,
    /// Install and start the per-user LaunchAgent.
    Install,
    /// Stop the LaunchAgent and remove its installed executable and plist.
    Uninstall,
}

#[derive(Args)]
struct StatusArgs {
    #[arg(long, default_value_t = 5)]
    timeout_seconds: u64,
}

#[derive(Args)]
struct AudioProbeArgs {
    #[arg(long, default_value_t = 15)]
    seconds: u64,
    #[arg(long, default_value_t = 100)]
    interval_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RunMode {
    Reactive,
    Static,
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, value_enum, default_value_t = RunMode::Reactive)]
    mode: RunMode,
    /// Static effect, and the fallback before the first reactive audio frame.
    #[arg(long, default_value = "breath", value_parser = parse_lighting_effect)]
    effect: LightingEffect,
    /// Master brightness in reactive mode; fixed brightness in static mode.
    #[arg(long, default_value_t = 1.0)]
    brightness: f32,
    #[arg(long, default_value_t = 0.85)]
    speed: f32,
    #[arg(long, default_value_t = 0.0)]
    magic: f32,
    #[arg(long, default_value_t = 250)]
    poll_ms: u64,
    #[arg(long, default_value_t = 750)]
    refresh_ms: u64,
    /// Reactive HID update interval. Lower values are more responsive but harder on the device.
    #[arg(long, default_value_t = 100)]
    device_ms: u64,
}

#[derive(Args)]
struct SetArgs {
    #[arg(long, value_parser = parse_hex_color)]
    color: Rgb,
    #[arg(long, default_value = "breath", value_parser = parse_lighting_effect)]
    effect: LightingEffect,
    #[arg(long, default_value_t = 1.0)]
    brightness: f32,
    #[arg(long, default_value_t = 0.85)]
    speed: f32,
    #[arg(long, default_value_t = 0.0)]
    magic: f32,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Probe => probe(),
        Command::Status(arguments) => status(arguments),
        Command::AudioProbe(arguments) => audio_probe(arguments),
        Command::Run(arguments) => run(arguments),
        Command::Set(arguments) => {
            hid::set_ambient(
                next_request_id(),
                arguments.effect,
                arguments.color.packed(),
                arguments.brightness,
                arguments.speed,
                arguments.magic,
            )?;
            println!(
                "Codex Micro ambient ring set to {} with {}",
                arguments.color.to_hex(),
                arguments.effect
            );
            Ok(())
        }
        Command::Off => {
            hid::clear(next_request_id())?;
            println!("Codex Micro lighting cleared");
            Ok(())
        }
        Command::Install => {
            let paths = service::install()?;
            println!("LaunchAgent installed: {}", paths.plist.display());
            println!("Worker executable: {}", paths.executable.display());
            println!("Logs: {}", paths.log_directory.display());
            Ok(())
        }
        Command::Uninstall => {
            let paths = service::uninstall()?;
            println!("LaunchAgent removed: {}", paths.plist.display());
            println!("Logs were preserved at {}", paths.log_directory.display());
            Ok(())
        }
    }
}

fn probe() -> Result<()> {
    let device = hid::probe().context("Codex Micro HID probe failed")?;
    println!(
        "Codex Micro HID: {}{}",
        device.product.as_deref().unwrap_or("connected"),
        device
            .serial
            .as_deref()
            .map(|serial| format!(" ({serial})"))
            .unwrap_or_default()
    );

    let source = MediaRemoteSource::new()?;
    match wait_for_snapshot(&source, Duration::from_secs(3)) {
        Some(snapshot) => println!(
            "MediaRemote: {} — {} [{}]",
            snapshot.artist.as_deref().unwrap_or("unknown artist"),
            snapshot.title.as_deref().unwrap_or("unknown title"),
            snapshot.bundle_id.as_deref().unwrap_or("unknown app")
        ),
        None => println!("MediaRemote: helper started; play a song to verify a payload"),
    }
    println!("SIP changes: not required");
    Ok(())
}

fn status(arguments: StatusArgs) -> Result<()> {
    let source = MediaRemoteSource::new()?;
    let snapshot = wait_for_snapshot(&source, Duration::from_secs(arguments.timeout_seconds))
        .context("MediaRemote returned no Now Playing information before the timeout")?;

    print_snapshot(&snapshot);
    if let Some(artwork) = snapshot.artwork.as_ref() {
        let color = ambient_color(artwork)?;
        println!(
            "Artwork: {}x{}, ambient color {}",
            artwork.width(),
            artwork.height(),
            color.to_hex()
        );
    } else {
        println!("Artwork: not available yet");
    }
    Ok(())
}

fn audio_probe(arguments: AudioProbeArgs) -> Result<()> {
    if arguments.seconds == 0 || arguments.interval_ms == 0 {
        bail!("seconds and interval-ms must be greater than zero");
    }
    let source = SystemAudioSource::start().context("Core Audio Process Tap could not start")?;
    println!("Core Audio Process Tap: {:.0} Hz", source.sample_rate());
    println!("Play audio now; feature frames follow as JSON lines.");

    let deadline = Instant::now() + Duration::from_secs(arguments.seconds);
    let interval = Duration::from_millis(arguments.interval_ms);
    let mut last_timestamp = None;
    while Instant::now() < deadline {
        if let Some(frame) = source.latest() {
            if last_timestamp != Some(frame.timestamp_seconds) {
                println!("{}", serde_json::to_string(&frame)?);
                last_timestamp = Some(frame.timestamp_seconds);
            }
        }
        thread::sleep(interval);
    }
    Ok(())
}

fn run(arguments: RunArgs) -> Result<()> {
    validate_effect_parameters(arguments.brightness, arguments.speed, arguments.magic)?;
    if arguments.poll_ms == 0 || arguments.refresh_ms == 0 || arguments.device_ms == 0 {
        bail!("poll-ms, refresh-ms, and device-ms must be greater than zero");
    }

    let running = Arc::new(AtomicBool::new(true));
    let signal_state = Arc::clone(&running);
    ctrlc::set_handler(move || signal_state.store(false, Ordering::SeqCst))
        .context("could not install the shutdown handler")?;

    let Some(mut controller) = wait_for_controller(&running) else {
        println!("Stopped before the Codex Micro LED controller became available");
        return Ok(());
    };
    let source = MediaRemoteSource::new()?;
    let track_log_directory = default_track_log_directory()
        .context("could not locate the per-track effect log directory")?;
    let mut track_logger = Some(
        TrackLogger::new(&track_log_directory)
            .context("could not initialize per-track effect logging")?,
    );
    let mut audio = match arguments.mode {
        RunMode::Reactive => {
            let Some(source) = wait_for_audio_source(&running) else {
                println!("Stopped before Core Audio Process Tap became available");
                return Ok(());
            };
            Some(source)
        }
        RunMode::Static => None,
    };
    let poll_interval = Duration::from_millis(arguments.poll_ms);
    let refresh_interval = match arguments.mode {
        RunMode::Reactive => Duration::from_millis(arguments.device_ms),
        RunMode::Static => Duration::from_millis(arguments.refresh_ms),
    };
    let mut current_key = None::<String>;
    let mut observed_key = None::<String>;
    let mut current_color = None::<Rgb>;
    let mut composer = None::<LightingComposer>;
    let mut current_scene = None::<LightingScene>;
    let mut last_audio_timestamp = None::<f64>;
    let mut last_composer_update = Instant::now();
    let mut last_effect = None::<LightingEffect>;
    let mut next_refresh = Instant::now();
    let mut next_media_poll = Instant::now();
    let mut last_hid_error = None::<String>;
    let mut playback_active = false;
    let mut last_audio_frame_at = Instant::now();
    let mut zero_audio_since = None::<Instant>;
    let mut next_audio_recovery = Instant::now() + AUDIO_RECOVERY_COOLDOWN;

    match arguments.mode {
        RunMode::Reactive => println!(
            "Following Now Playing color with reactive system-audio lighting at {:.0} Hz. Press Control-C to stop.",
            audio.as_ref().map_or(0.0, SystemAudioSource::sample_rate)
        ),
        RunMode::Static => println!(
            "Following local macOS Now Playing with static {}. Press Control-C to stop.",
            arguments.effect
        ),
    }
    while running.load(Ordering::SeqCst) {
        if Instant::now() >= next_media_poll {
            next_media_poll = Instant::now() + poll_interval;
            if let Some(snapshot) = source.snapshot() {
                let snapshot_key = snapshot.track_key();
                let position_restarted = if let (Some(logger), Some(key)) =
                    (track_logger.as_mut(), snapshot_key.as_deref())
                {
                    logger.active_key() == Some(key) && logger.update_snapshot(&snapshot)
                } else {
                    false
                };
                if position_restarted {
                    finish_track_log(&mut track_logger, "position_restarted");
                    if let Some(key) = snapshot_key.as_deref() {
                        start_track_log(&mut track_logger, key, &snapshot);
                    }
                }
                if snapshot.is_playing == Some(false) {
                    if snapshot_reached_end(&snapshot) {
                        finish_track_log(&mut track_logger, "ended");
                    }
                    playback_active = false;
                    zero_audio_since = None;
                    source.invalidate_artwork_delivery();
                    observed_key = None;
                    current_key = None;
                    composer = None;
                    current_scene = None;
                    last_audio_timestamp = None;
                    last_effect = None;
                    if current_color.take().is_some() {
                        match controller.clear(next_request_id()) {
                            Ok(()) => println!("Now Playing paused; lighting cleared"),
                            Err(error) => eprintln!("Could not clear paused lighting: {error}"),
                        }
                    }
                } else if snapshot.is_playing == Some(true) {
                    if !playback_active {
                        last_audio_frame_at = Instant::now();
                        zero_audio_since = None;
                    }
                    playback_active = true;
                    if let Some(key) = snapshot_key {
                        if observed_key.as_deref() != Some(&key) {
                            start_track_log(&mut track_logger, &key, &snapshot);
                            observed_key = Some(key.clone());
                            current_key = None;
                            composer = None;
                            current_scene = None;
                            last_audio_timestamp = None;
                            last_effect = None;
                            if current_color.take().is_some() {
                                match controller.clear(next_request_id()) {
                                    Ok(()) => {
                                        println!(
                                            "Now Playing changed; waiting for the new thumbnail"
                                        )
                                    }
                                    Err(error) => {
                                        eprintln!(
                                            "Could not clear the previous thumbnail color: {error}"
                                        )
                                    }
                                }
                            }
                        }

                        if current_key.as_deref() != Some(&key) {
                            if let Some(artwork) = snapshot.artwork.as_ref() {
                                match ambient_color(artwork) {
                                    Ok(color) => {
                                        println!(
                                            "{} — {} [{}] -> {} ({})",
                                            snapshot.artist.as_deref().unwrap_or("unknown artist"),
                                            snapshot.title.as_deref().unwrap_or("unknown title"),
                                            snapshot.bundle_id.as_deref().unwrap_or("unknown app"),
                                            color.to_hex(),
                                            arguments.effect
                                        );
                                        current_key = Some(key);
                                        current_color = Some(color);
                                        composer = Some(LightingComposer::new(color));
                                        current_scene = Some(LightingScene {
                                            effect: arguments.effect,
                                            color,
                                            brightness: arguments.brightness,
                                            speed: arguments.speed,
                                            magic: arguments.magic,
                                        });
                                        last_audio_timestamp = None;
                                        last_composer_update = Instant::now();
                                        next_refresh = Instant::now();
                                    }
                                    Err(error) => {
                                        eprintln!("Thumbnail color extraction failed: {error}");
                                        source.invalidate_artwork_delivery();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(audio) = audio.as_ref() {
            if let Some(frame) = audio.latest() {
                if last_audio_timestamp != Some(frame.timestamp_seconds) {
                    let now = Instant::now();
                    last_audio_frame_at = now;
                    if frame.silent {
                        zero_audio_since.get_or_insert(now);
                    } else {
                        zero_audio_since = None;
                    }
                    let elapsed = now.saturating_duration_since(last_composer_update);
                    last_composer_update = now;
                    last_audio_timestamp = Some(frame.timestamp_seconds);
                    if let Some(composer) = composer.as_mut() {
                        let mut scene = composer.update(frame, elapsed);
                        scene.brightness =
                            (scene.brightness * arguments.brightness).clamp(0.0, 1.0);
                        current_scene = Some(scene);
                        if let Some(logger) = track_logger.as_mut() {
                            if let Err(error) = logger.observe_scene(scene, frame, elapsed) {
                                eprintln!("Track effect logging failed and was disabled: {error}");
                                track_logger = None;
                            }
                        }
                        if last_effect != Some(scene.effect) {
                            println!(
                                "Audio scene -> {} (brightness {:.2}, speed {:.2}, magic {:.2})",
                                scene.effect, scene.brightness, scene.speed, scene.magic
                            );
                            last_effect = Some(scene.effect);
                        }
                    }
                }
            }
        }

        let now = Instant::now();
        let recovery_reason = audio_recovery_reason(
            arguments.mode == RunMode::Reactive,
            playback_active,
            audio.is_some(),
            now.saturating_duration_since(last_audio_frame_at),
            zero_audio_since.map(|since| now.saturating_duration_since(since)),
            now >= next_audio_recovery,
        );
        if let Some(reason) = recovery_reason {
            eprintln!("Core Audio Process Tap recovery: {reason}");
            audio.take();
            match SystemAudioSource::start() {
                Ok(replacement) => {
                    println!(
                        "Core Audio Process Tap recovered at {:.0} Hz",
                        replacement.sample_rate()
                    );
                    audio = Some(replacement);
                }
                Err(error) => {
                    eprintln!("Core Audio Process Tap recovery failed; will retry: {error}");
                }
            }
            last_audio_timestamp = None;
            last_audio_frame_at = Instant::now();
            zero_audio_since = None;
            next_audio_recovery = Instant::now() + AUDIO_RECOVERY_COOLDOWN;
        }

        if let Some(scene) = current_scene.filter(|_| Instant::now() >= next_refresh) {
            match controller.set_ambient(
                next_request_id(),
                scene.effect,
                scene.color.packed(),
                scene.brightness,
                scene.speed,
                scene.magic,
            ) {
                Ok(()) => last_hid_error = None,
                Err(error) => {
                    let message = error.to_string();
                    if last_hid_error.as_deref() != Some(&message) {
                        eprintln!("Codex Micro write failed; will retry: {message}");
                    }
                    last_hid_error = Some(message);
                }
            }
            next_refresh = Instant::now() + refresh_interval;
        }

        // MediaRemote can be polled comparatively slowly, while audio frames and the HID
        // governor need a tighter wake-up cadence for perceptually responsive lighting.
        thread::sleep(Duration::from_millis(20));
    }

    finish_track_log(&mut track_logger, "stopped");
    controller
        .clear(next_request_id())
        .context("stopped, but the Codex Micro ring could not be cleared")?;
    println!("Stopped and cleared the Codex Micro ring");
    Ok(())
}

fn audio_recovery_reason(
    reactive: bool,
    playback_active: bool,
    audio_available: bool,
    since_last_frame: Duration,
    zero_audio_for: Option<Duration>,
    cooldown_elapsed: bool,
) -> Option<AudioRecoveryReason> {
    if !reactive || !playback_active || !cooldown_elapsed {
        return None;
    }
    if !audio_available {
        return Some(AudioRecoveryReason::Missing);
    }
    if since_last_frame >= AUDIO_FRAME_STALL_THRESHOLD {
        return Some(AudioRecoveryReason::Stalled);
    }
    if zero_audio_for.is_some_and(|duration| duration >= AUDIO_ZERO_RECOVERY_THRESHOLD) {
        return Some(AudioRecoveryReason::ZeroFilled);
    }
    None
}

fn wait_for_controller(running: &AtomicBool) -> Option<hid::Controller> {
    let mut last_error = None::<String>;
    while running.load(Ordering::SeqCst) {
        match hid::Controller::open() {
            Ok(controller) => {
                println!("Codex Micro LED controller connected");
                return Some(controller);
            }
            Err(error) => {
                let message = error.to_string();
                if last_error.as_deref() != Some(&message) {
                    eprintln!(
                        "Codex Micro LED controller is unavailable; waiting for the device or Input Monitoring permission: {message}"
                    );
                    last_error = Some(message);
                }
            }
        }

        let retry_at = Instant::now() + STARTUP_RETRY_INTERVAL;
        while running.load(Ordering::SeqCst) && Instant::now() < retry_at {
            thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

fn wait_for_audio_source(running: &AtomicBool) -> Option<SystemAudioSource> {
    let mut last_error = None::<String>;
    while running.load(Ordering::SeqCst) {
        match SystemAudioSource::start() {
            Ok(source) => return Some(source),
            Err(error) => {
                let message = error.to_string();
                if last_error.as_deref() != Some(&message) {
                    eprintln!(
                        "Core Audio Process Tap is unavailable; waiting for System Audio Recording permission: {message}"
                    );
                    last_error = Some(message);
                }
            }
        }

        let retry_at = Instant::now() + STARTUP_RETRY_INTERVAL;
        while running.load(Ordering::SeqCst) && Instant::now() < retry_at {
            thread::sleep(Duration::from_millis(100));
        }
    }
    None
}

fn wait_for_snapshot(source: &MediaRemoteSource, timeout: Duration) -> Option<TrackSnapshot> {
    let deadline = Instant::now() + timeout;
    let mut latest = None;
    loop {
        if let Some(snapshot) = source.snapshot() {
            if snapshot.title.is_some()
                || snapshot.bundle_id.is_some()
                || snapshot.artwork.is_some()
            {
                if snapshot.artwork.is_some() {
                    return Some(snapshot);
                }
                latest = Some(snapshot);
            }
        }
        if Instant::now() >= deadline {
            return latest;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn print_snapshot(snapshot: &TrackSnapshot) {
    println!(
        "Application: {}",
        snapshot.bundle_id.as_deref().unwrap_or("unknown")
    );
    println!("Playing: {}", snapshot.is_playing.unwrap_or(false));
    println!("Title: {}", snapshot.title.as_deref().unwrap_or("unknown"));
    println!(
        "Artist: {}",
        snapshot.artist.as_deref().unwrap_or("unknown")
    );
    println!("Album: {}", snapshot.album.as_deref().unwrap_or("unknown"));
    if let Some(elapsed) = snapshot.elapsed_time {
        println!("Elapsed: {elapsed:.1}s");
    }
    if let Some(duration) = snapshot.duration {
        println!("Duration: {duration:.1}s");
    }
}

fn start_track_log(logger: &mut Option<TrackLogger>, key: &str, snapshot: &TrackSnapshot) {
    let Some(active) = logger.as_mut() else {
        return;
    };
    if active.active_key() == Some(key) {
        active.update_snapshot(snapshot);
        return;
    }
    match active.finish("track_changed") {
        Ok(Some(finished)) => print_finished_track(&finished),
        Ok(None) => {}
        Err(error) => {
            eprintln!("Could not finish the previous track effect log: {error}");
            *logger = None;
            return;
        }
    }
    match active.start_track(key, snapshot) {
        Ok(path) => println!(
            "Track effect log started at {:.1}/{:.1}s: {}",
            snapshot.elapsed_time.unwrap_or(0.0),
            snapshot.duration.unwrap_or(0.0),
            path.display()
        ),
        Err(error) => {
            eprintln!("Track effect logging failed and was disabled: {error}");
            *logger = None;
        }
    }
}

fn finish_track_log(logger: &mut Option<TrackLogger>, reason: &str) {
    let Some(active) = logger.as_mut() else {
        return;
    };
    match active.finish(reason) {
        Ok(Some(finished)) => print_finished_track(&finished),
        Ok(None) => {}
        Err(error) => {
            eprintln!("Could not finish the track effect log: {error}");
            *logger = None;
        }
    }
}

fn print_finished_track(finished: &FinishedTrack) {
    let usage = finished
        .summary
        .effect_usage
        .iter()
        .filter(|usage| usage.seconds >= 0.01)
        .map(|usage| {
            format!(
                "{} {:.1}s/{:.1}%",
                usage.effect, usage.seconds, usage.percent
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "Track effect log finished (complete={}, observed {:.1}s, transitions {}): {} [{}]",
        finished.summary.complete_track,
        finished.summary.observed_seconds,
        finished.summary.transitions,
        finished.path.display(),
        usage
    );
}

fn snapshot_reached_end(snapshot: &TrackSnapshot) -> bool {
    snapshot
        .elapsed_time
        .zip(snapshot.duration)
        .is_some_and(|(elapsed, duration)| {
            elapsed.is_finite()
                && duration.is_finite()
                && duration > 0.0
                && elapsed >= duration - 2.0
        })
}

fn next_request_id() -> u16 {
    NEXT_REQUEST_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
            Some(if id >= 999 { 1 } else { id + 1 })
        })
        .unwrap_or(1)
}

fn parse_hex_color(value: &str) -> Result<Rgb, String> {
    let normalized = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    if normalized.len() != 6 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("color must contain exactly six hexadecimal digits".into());
    }
    let packed = u32::from_str_radix(normalized, 16).map_err(|error| error.to_string())?;
    Ok(Rgb::new(
        ((packed >> 16) & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        (packed & 0xFF) as u8,
    ))
}

fn parse_lighting_effect(value: &str) -> Result<LightingEffect, String> {
    value
        .parse::<LightingEffect>()
        .map_err(|error| error.to_string())
}

fn validate_effect_parameters(brightness: f32, speed: f32, magic: f32) -> Result<()> {
    for (name, value) in [
        ("brightness", brightness),
        ("speed", speed),
        ("magic", magic),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            bail!("{name} must be a finite value between 0 and 1");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_hex_forms() {
        assert_eq!(
            parse_hex_color("#123456").unwrap(),
            Rgb::new(0x12, 0x34, 0x56)
        );
        assert_eq!(
            parse_hex_color("0xABCDEF").unwrap(),
            Rgb::new(0xAB, 0xCD, 0xEF)
        );
        assert!(parse_hex_color("12345").is_err());
    }

    #[test]
    fn breath_is_the_default_effect_for_run_and_set() {
        let run = Cli::try_parse_from(["codex-micro-chroma", "run"]).unwrap();
        let Command::Run(arguments) = run.command else {
            panic!("expected run command");
        };
        assert_eq!(arguments.effect, LightingEffect::Breath);

        let set = Cli::try_parse_from(["codex-micro-chroma", "set", "--color", "123456"]).unwrap();
        let Command::Set(arguments) = set.command else {
            panic!("expected set command");
        };
        assert_eq!(arguments.effect, LightingEffect::Breath);
    }

    #[test]
    fn audio_recovery_is_gated_by_playback_mode_and_cooldown() {
        assert_eq!(
            audio_recovery_reason(true, true, true, AUDIO_FRAME_STALL_THRESHOLD, None, true,),
            Some(AudioRecoveryReason::Stalled)
        );
        assert_eq!(
            audio_recovery_reason(
                true,
                true,
                true,
                Duration::ZERO,
                Some(AUDIO_ZERO_RECOVERY_THRESHOLD),
                true,
            ),
            Some(AudioRecoveryReason::ZeroFilled)
        );
        assert_eq!(
            audio_recovery_reason(true, true, false, Duration::ZERO, None, true),
            Some(AudioRecoveryReason::Missing)
        );
        assert_eq!(
            audio_recovery_reason(
                false,
                true,
                false,
                AUDIO_FRAME_STALL_THRESHOLD,
                Some(AUDIO_ZERO_RECOVERY_THRESHOLD),
                true,
            ),
            None
        );
        assert_eq!(
            audio_recovery_reason(
                true,
                false,
                false,
                AUDIO_FRAME_STALL_THRESHOLD,
                Some(AUDIO_ZERO_RECOVERY_THRESHOLD),
                true,
            ),
            None
        );
        assert_eq!(
            audio_recovery_reason(
                true,
                true,
                false,
                AUDIO_FRAME_STALL_THRESHOLD,
                Some(AUDIO_ZERO_RECOVERY_THRESHOLD),
                false,
            ),
            None
        );
    }
}
