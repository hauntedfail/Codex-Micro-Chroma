# README design notes

Research checked on 27 September 2026. These projects are references for presentation,
not evidence that a README layout causes stars. Audience size, distribution, project
age, and the usefulness of the product also affect adoption.

## References and decisions

| Project | What its README does | Application here |
| --- | --- | --- |
| [Glow](https://github.com/charmbracelet/glow#readme) | Opens with a short identity and visual presentation, then explains the tool and how to install it. | Give the lighting project a recognisable visual identity and explain the outcome before its internals. |
| [uv](https://github.com/astral-sh/uv#readme) | Puts its purpose, a benchmark graphic, highlights, and installation near the top; links to deeper documentation. | Make the core value visible immediately and move implementation detail to a technical guide. A concept diagram communicates the pipeline but does not provide the proof of a benchmark or recording. |
| [Starship](https://github.com/starship/starship#readme) | Provides prominent navigation, compact benefits, and installation steps with explicit prerequisites. | Put download, effects, compatibility, and guide links at the top; distinguish runtime requirements from build tools. |
| [CAVA](https://github.com/karlstav/cava#readme) | Gives an audio visualiser a demo link before its lengthy platform setup reference. | Prioritise seeing the lighting result. Use an explicitly labelled illustration until real hardware footage is available. |

## What changed

- Lead with artwork colour and audio-driven movement, rather than daemon architecture.
- Add an original, self-contained SVG. It uses no player logos, borrowed album artwork,
  fake hardware photographs, animation, scripts, or external fonts.
- Keep badges to release, CI, platform, and licence; avoid a wall of counters.
- Separate a short download → check → run path from detailed permissions and DSP notes.
- Retain all previous technical material in [the guide](guide.md).
- Keep player validation gaps, mixed-system-audio behaviour, private MediaRemote use,
  and signing limitations visible. Do not claim universal player support.
- Verify that the recommended release exists. At review time, v0.1.0 is published;
  the v0.2.0 version-bump PR is still open.
- Give readers specific ways to contribute: test a player, report a bug, or share a clip.
  Include one modest star invitation after the useful content.

## Best next visual

A short real-device clip would provide evidence the illustration cannot. Frame the
Codex Micro and current artwork together, show a track change, then show the ring
reacting to a change in the music. Label the macOS version, Chroma version, and player.
Keep exposures consistent and avoid synchronising an edited animation to imply measured
latency. Use media you have permission to share.

Place the clip or a compact GIF with a still fallback immediately below the introduction.
Keep the illustration only if it still helps explain the two inputs. No demonstration
footage was available in the repository during this edit.

## Evaluating the change

Compare repository visitors, release downloads, player reports, and stars over similar
time windows, noting any launches or social posts. A change in stars alone cannot
establish that the README caused it. This edit improves presentation and onboarding;
it does not establish product adoption or hardware validation.
