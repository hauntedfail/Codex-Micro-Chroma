use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use codex_micro_chroma::{
    color::{ambient_color, Rgb},
    hid,
    media::{MediaRemoteSource, TrackSnapshot},
    protocol::LightingEffect,
    service,
};

static NEXT_REQUEST_ID: AtomicU16 = AtomicU16::new(1);

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
struct RunArgs {
    #[arg(long, default_value = "breath", value_parser = parse_lighting_effect)]
    effect: LightingEffect,
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
    if !Path::new("/usr/bin/perl").is_file() {
        bail!("MediaRemote host /usr/bin/perl is missing");
    }
    println!("Perl adapter host: /usr/bin/perl");

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
        None => println!("MediaRemote: adapter started; play a song to verify a payload"),
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

fn run(arguments: RunArgs) -> Result<()> {
    validate_effect_parameters(arguments.brightness, arguments.speed, arguments.magic)?;
    if arguments.poll_ms == 0 || arguments.refresh_ms == 0 {
        bail!("poll-ms and refresh-ms must be greater than zero");
    }

    let running = Arc::new(AtomicBool::new(true));
    let signal_state = Arc::clone(&running);
    ctrlc::set_handler(move || signal_state.store(false, Ordering::SeqCst))
        .context("could not install the shutdown handler")?;

    let source = MediaRemoteSource::new()?;
    let poll_interval = Duration::from_millis(arguments.poll_ms);
    let refresh_interval = Duration::from_millis(arguments.refresh_ms);
    let mut current_key = None::<String>;
    let mut observed_key = None::<String>;
    let mut current_color = None::<Rgb>;
    let mut next_refresh = Instant::now();
    let mut last_hid_error = None::<String>;

    println!(
        "Following local macOS Now Playing with {}. Press Control-C to stop and clear the ring.",
        arguments.effect
    );
    while running.load(Ordering::SeqCst) {
        if let Some(snapshot) = source.snapshot() {
            if snapshot.is_playing == Some(true) {
                if let Some(key) = snapshot.track_key() {
                    if observed_key.as_deref() != Some(&key) {
                        observed_key = Some(key.clone());
                        current_key = None;
                        if current_color.take().is_some() {
                            match hid::clear(next_request_id()) {
                                Ok(()) => {
                                    println!("Now Playing changed; waiting for the new thumbnail")
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
                                    next_refresh = Instant::now();
                                }
                                Err(error) => {
                                    eprintln!("Thumbnail color extraction failed: {error}")
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(color) = current_color.filter(|_| Instant::now() >= next_refresh) {
            match hid::set_ambient(
                next_request_id(),
                arguments.effect,
                color.packed(),
                arguments.brightness,
                arguments.speed,
                arguments.magic,
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

        thread::sleep(poll_interval);
    }

    hid::clear(next_request_id())
        .context("stopped, but the Codex Micro ring could not be cleared")?;
    println!("Stopped and cleared the Codex Micro ring");
    Ok(())
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
}
