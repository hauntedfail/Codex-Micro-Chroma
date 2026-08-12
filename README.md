# Codex Micro Chroma

Codex Micro Chroma is a local, audio-reactive lighting daemon for macOS, written in Rust. It extracts a representative colour from the artwork or thumbnail shown in macOS Now Playing and uses it to light the outer LED ring of the Work Louder Codex Micro.

It is not tied to Apple Music, Spotify, or any other service. Any music player, video player, or browser that publishes its current media and artwork through macOS Now Playing can use the same integration.

Lighting effects and brightness respond dynamically to the audio's musical dynamics, frequency balance, timbre, transients, rhythmic pulse, and stereo width. Everything is processed on your Mac: there is no OpenAI or Codex model usage, no Spotify or Apple Music API, no API key, no OAuth flow, and no external server.

## How it works

```text
macOS Now Playing (Music / Spotify / browser / other players)
  -> macOS MediaRemote
  -> ad-hoc signed embedded per-player session helper
  -> active-session arbitration from macOS-owned state
  -> local image decoding and representative-colour extraction
  -> artwork colour

macOS system output
  -> public Core Audio Process Tap
  -> local Rust DSP (loudness / bands / centroid / flatness / flux / onset /
                     stereo width / pulse / tempo confidence)
  -> LightingComposer (effect state machine / smoothing / hysteresis)
  -> persistent hidapi session
  -> Codex Micro v.oai.rgbcfg RPC
```

MediaRemote is a private Apple framework. Its behaviour may change after a macOS update, and this architecture is not suitable for App Store distribution. If a player does not publish artwork to Now Playing, Codex Micro Chroma cannot derive a colour from it.

When several applications publish Now Playing sessions simultaneously, Codex Micro Chroma enumerates the current OS sessions on every refresh and considers only sessions that macOS reports as playing with a resolved playback state. A per-session scoped-playing error leaves only that session unresolved and ineligible for selection. A completed metadata failure still publishes the authoritative playing session identity without stale track fields or artwork, so previous lighting is cleared while macOS continues reporting that session as active. A globally timed-out refresh is treated as a broken helper generation: the helper emits a reset control message, Rust clears the last Now Playing state, and the helper replaces its own process image before publishing a fresh ready message. The session with the newest macOS `lastPlayingDate` wins; the OS-elected Now Playing session breaks equal or unavailable-date ties, followed by a stable identifier for deterministic output. No separate playback-order history is persisted by this application. If the selected session stops, its lighting is cleared and the next still-playing session is selected from that same OS snapshot. If none remains, lighting stays off.

MediaRemote payloads are treated as untrusted local input. The helper bounds the number of processed player clients, copied text fields, raw artwork, and aggregate serialized artwork. Rust bounds each helper JSON line, rejects oversized encoded or decoded artwork before image decoding, and decodes images with explicit dimension and allocation limits. Oversized or invalid artwork is omitted gracefully while the authoritative playback candidate is still processed.

## Requirements

- macOS 14.2 or later for reactive mode; `--mode static` does not require a Process Tap
- A music player, video player, or browser that publishes artwork to macOS Now Playing
- A connected Work Louder Codex Micro
- Rust 1.88 or later
- Xcode Command Line Tools

## Compatibility and current status

The integration is service-agnostic, but artwork compatibility ultimately depends on each player publishing an image to macOS Now Playing. The browser/Helium path has been verified. Apple Music and Spotify artwork and colour output still require validation on the target Mac, as do long-running reconnect behaviour and final effect calibration on physical Codex Micro hardware.

Reactive analysis listens to the complete mixed macOS system output. If several applications play audio at once, the effects respond to that combined output while the base colour comes from the active session selected by the arbitration above.

Exactly one HID interface matching the Codex Micro is required. No matching device, more than one matching interface, or missing Input Monitoring access leaves `run` waiting for the controller and causes one-shot commands such as `probe` or `set` to report an error.

## Download

The current release is **v0.1.0**. Download the universal macOS archive and its SHA-256 checksum from GitHub Releases:

```bash
VERSION=0.1.0
BASE_URL="https://github.com/hauntedfail/Codex-Micro-Chroma/releases/download/v${VERSION}"
ARCHIVE="codex-micro-chroma-v${VERSION}-macos-universal.tar.gz"

curl -LO "${BASE_URL}/${ARCHIVE}"
curl -LO "${BASE_URL}/${ARCHIVE}.sha256"
shasum -a 256 -c "${ARCHIVE}.sha256"
tar -xzf "${ARCHIVE}"
cd "codex-micro-chroma-v${VERSION}-macos-universal"
./codex-micro-chroma --version
```

The release binary supports both Apple Silicon and Intel Macs. It is ad-hoc signed but is not Developer ID signed or notarised, so macOS may ask you to confirm the first launch in **System Settings > Privacy & Security**.

To install the downloaded binary as a per-user LaunchAgent:

```bash
./codex-micro-chroma install
```

## Build from source

Build the release binary from the project root:

```bash
cargo build --release
```

Check the local MediaRemote and HID connections, inspect the current artwork colour, and verify system-audio analysis:

```bash
./target/release/codex-micro-chroma probe
./target/release/codex-micro-chroma status
./target/release/codex-micro-chroma audio-probe --seconds 15
```

## Required user interaction and permissions

macOS privacy permissions cannot be granted by the installer or from the command line. You must approve them in System Settings. Permissions are associated with the executable that requests access, so a command run from Terminal and the installed background worker may need separate approval.

1. Connect one Codex Micro to the Mac.
2. Run `./target/release/codex-micro-chroma probe`.
3. If HID access is reported as `not permitted`, open **System Settings > Privacy & Security > Input Monitoring**, add and enable the terminal application you are using, then run `probe` again.
4. Start some media so that macOS Now Playing contains a current item and artwork, then run `status`.
5. Run `audio-probe --seconds 15` or start `run` in reactive mode. When macOS asks, approve **Screen & System Audio Recording**, or **System Audio Recording Only**, depending on your macOS version.

The embedded `NSAudioCaptureUsageDescription` explains that system audio is analysed locally to animate the LED ring. Audio is converted into feature values in memory; it is never recorded, saved, or sent over the network.

`run` retries HID access every two seconds until the device and Input Monitoring permission become available. In reactive mode it also retries the Process Tap every two seconds until System Audio Recording permission is available, so you do not normally need to restart it after granting access. `audio-probe`, `probe`, `status`, `set`, and `off` are one-shot commands; correct the permission or device problem and run the command again.

Use `--mode static` if you do not want to grant system-audio access. Static mode still needs HID access and Now Playing artwork, but it does not start a Core Audio Process Tap.

### Start automatically at login

Install and start the per-user LaunchAgent:

```bash
./target/release/codex-micro-chroma install
```

The command copies the release binary into Application Support, gives it an ad-hoc signature with the fixed identifier `com.local.codex-micro-chroma`, and registers an Aqua-session LaunchAgent. The installed worker is located at:

```text
~/Library/Application Support/CodexMicroChroma/codex-micro-chroma
```

If the system-audio permission prompt appears immediately after installation, grant it. If you dismiss it, add and enable the worker shown above in **System Settings > Privacy & Security > Screen & System Audio Recording**, then run `install` again.

The LaunchAgent cannot approve its own privacy prompts. If the background worker is waiting, manually add and enable this installed executable for both permissions that are required on your Mac:

- **Input Monitoring**, for Codex Micro HID access
- **Screen & System Audio Recording** or **System Audio Recording Only**, for reactive audio analysis

After changing a permission, check `~/Library/Logs/CodexMicroChroma/worker-error.log`. The worker normally resumes through its built-in retry loop. If macOS does not apply the change to the running worker, run `install` again to restart the LaunchAgent.

An ad-hoc signature's designated requirement includes the binary's CDHash. After rebuilding and reinstalling, macOS may therefore ask you to approve Input Monitoring or System Audio Recording again. If necessary, switch the installed worker's permission off and on. A distribution build signed with a Developer ID or local code-signing certificate can avoid this repeated approval.

Worker and track logs are stored in `~/Library/Logs/CodexMicroChroma/`.

To stop the LaunchAgent and remove the installed copy:

```bash
~/Library/Application\ Support/CodexMicroChroma/codex-micro-chroma uninstall
```

`uninstall` removes the LaunchAgent plist and installed executable, but preserves diagnostic logs.

## Usage

### Follow the current media

Start the default reactive mode:

```bash
./target/release/codex-micro-chroma run
```

- When the player or media changes, the new artwork is analysed automatically.
- A thumbnail fingerprint is used as the content identity when no title is available, and artwork changes are detected even when the title remains the same.
- White backgrounds and transparent pixels are excluded before the dominant colour is adjusted to be brighter and more vivid on the LEDs.
- The previous media colour is cleared while new artwork is pending.
- Relative loudness controls brightness, combined musical motion controls speed, and spatial width and change control the device's `magic` parameter.
- Brief onset, flux, pulse, and bass events are held with attack-and-release envelopes so that they can pass the 650 ms candidate dwell. Hysteresis preserves close-scoring candidates, allowing a beat to trigger a dynamic effect such as `snake` even if it has disappeared by the next audio frame.
- Effects have a two-second minimum hold to prevent flicker. If the same pattern remains the strongest candidate for more than 12 seconds, a semantically close runner-up may take over at a phrase boundary to avoid becoming stuck on a single effect such as `breath`. `rainbow` has a 12-second cooldown.
- Speed and `magic` use effect-specific profiles: `solid` remains still, `snake` follows the beat, `gradient` reflects stereo width, `rainbow` marks a climax, and the two breathing effects use different depths. A brief rest does not switch the ring off.
- A persistent HID connection is reused and updated every 100 ms by default. If the device or Input Monitoring permission is unavailable at start-up, the worker waits rather than exiting. It automatically reconnects once after a disconnection, response timeout, or other transport failure.
- If Process Tap frames stop for three seconds during playback, or remain completely zero-filled for 15 seconds, the tap and aggregate device are rebuilt with a 30-second recovery cooldown.
- Control-C or SIGTERM stops the process and switches the LEDs off.

The default values can be set explicitly:

```bash
./target/release/codex-micro-chroma run \
  --mode reactive \
  --brightness 1.0 \
  --poll-ms 250 \
  --device-ms 100
```

To follow artwork while keeping one fixed effect:

```bash
./target/release/codex-micro-chroma run \
  --mode static \
  --effect breath \
  --brightness 1.0 \
  --speed 0.85 \
  --magic 0.0 \
  --refresh-ms 750
```

### Test the lighting directly

Set a colour without reading Now Playing, then switch the lighting off. The default effect for both `set` and `run` is `breath`.

```bash
./target/release/codex-micro-chroma set --color '#33AAFF'
./target/release/codex-micro-chroma set --color '#33AAFF' --effect snake
./target/release/codex-micro-chroma off
```

`off` clears both the key lighting and ambient-ring lighting controlled by this tool.

### Inspect audio features

`audio-probe` does not write to the LEDs. It prints `AudioFeatureFrame` values calculated from the Process Tap as JSON Lines, which is useful for permission checks and algorithm tuning.

```bash
./target/release/codex-micro-chroma audio-probe --seconds 15
```

## Lighting effects

All currently known Codex Micro effects can be selected.

| CLI name | Device value | Role in reactive mode |
| --- | ---: | --- |
| `off` | 0 | Sustained silence or stopped playback |
| `solid` | 1 | Speech or direct, centre-panned sound |
| `snake` | 2 | Strong bass, pulse, or rhythmic periodicity |
| `rainbow` | 3 | A climax combining high level, strong flux, and pronounced onsets |
| `breath` | 4 | Smooth, sustained, tonal material |
| `gradient` | 5 | Wide stereo material |
| `shallow-breath` | 6 | Quiet passages, introductions, and outros |

The shared `--brightness`, `--speed`, and `--magic` parameters each accept values from 0 to 1. Their visible behaviour and useful combinations depend on the device firmware's implementation of each effect.

## Per-track effect logs

`run` follows the Now Playing `elapsed_time`, `duration`, and `playback_rate` values and automatically writes one JSON Lines log per track:

```text
~/Library/Logs/CodexMicroChroma/tracks/<timestamp>-<title>.jsonl
```

- `track_start`: media metadata, source application, duration, and observed starting position
- `effect_transition`: position, effect, colour, brightness, speed, `magic`, and all audio features at that moment
- `track_summary`: time and percentage per effect, transition count, observed time, and completion reason

`complete_track` is `true` only when observation began within the first two seconds, ended within the final two seconds, and covered at least 85% of the media's duration. Accounting pauses with playback and resumes in the same log when the same media continues.

If a repeat or crossfade causes the position to jump backwards to within the first five seconds, the current pass is finalised with `position_restarted` and the next pass is written to a separate log. The previous pass still counts as complete if at least 85% of its duration was observed. Records are flushed on every transition, so a live log can be inspected with `tail -f`.

MediaRemote elapsed time is anchored to a local monotonic clock when each event arrives. This prevents time spent paused from being added to the media position if MediaRemote returns an old timestamp after a long pause.

## Privacy and security

- No OpenAI, Codex, Spotify, Apple Music, or other external API is used.
- No network connection is made at runtime.
- System Integrity Protection is not changed.
- Root access, `sudo`, and code injection are not used.
- Process Tap PCM is not written to a file.
- Playback is observed but never controlled.
- HID access opens exactly one interface matching the Codex Micro VID, PID, and usage page.
- An inter-process lock serialises concurrent LED writes.
- The installed worker uses a fixed identifier and an ad-hoc signature, not a Developer ID certificate.

## Implementation notes

`CATapDescription` is an Objective-C API, so only Process Tap and aggregate-device creation and destruction are isolated in a small Objective-C bridge. After the bridge delivers Float32 PCM, the FFT, feature extraction, relative normalisation, effect scoring, state transitions, and HID control are implemented in Rust.

The audio callback sends fixed-size packets to a bounded channel with `try_send`; FFT work and locking never run on Core Audio's real-time thread.

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Publishing a release

The `Release macOS binary` GitHub Actions workflow builds an ad-hoc-signed universal binary, creates a checksum, and publishes both files in a new GitHub Release. The requested version must match the `version` in `Cargo.toml`.

To publish manually, open **Actions > Release macOS binary > Run workflow** and enter a version without the leading `v`, such as `0.1.0`. Alternatively, push a matching tag such as `v0.1.0`.

An existing release or manually supplied existing tag is never overwritten. Update `Cargo.toml` and the version shown in the Download section before publishing the next version.

## Licence

Codex Micro Chroma is available under the [MIT Licence](LICENSE).
