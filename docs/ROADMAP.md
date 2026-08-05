# Xyra product roadmap

## Architecture

Xyra keeps product policy independent from OS-specific capture and platform-specific upload code:

```text
Rust desktop UI
  |-- capture service --> libobs/Windows capture --> OBS replay buffer
  |-- media service ---> decoder/player + non-destructive edit graph + exporter
  |-- library service --> local metadata, thumbnails, tags, search
  `-- publish service --> background queue --+-- YouTube OAuth + resumable upload
                                             `-- local LBRY SDK publish API
```

The alpha now embeds libobs for capture and replay buffering. FFmpeg remains a separate utility for playback, trimming, export, and a legacy fallback. Capture engine policy remains isolated from clip, edit, and publishing models.

## Implemented in the current alpha

- Multiple global save-replay hotkeys and selectable clip durations
- Single-monitor capture on multi-monitor systems
- Desktop audio through native WASAPI loopback plus microphone input and separate tracks
- Automatic NVENC, AMF, Quick Sync, or software encoder detection
- Low-overhead NVIDIA Desktop Duplication capture with a cursor compatibility option
- Background clip saving, library probing, player decoding, exports, and uploads
- Native player controls for play/pause, seek, skip, restart, mute, and volume
- System-browser Google OAuth 2.0 with a loopback redirect
- Refresh-token storage in Windows Credential Manager
- Resumable YouTube uploads with Public, Unlisted, and Private visibility
- Odysee publishing through the local LBRY SDK using honest Public visibility
- User-controlled auto-publish after clipping; default remains off
- Installer, Windows startup settings, minimize-to-tray behavior, and tray capture controls
- Direct OBS Studio/libobs replay buffer with a curated backend-only runtime

## Reliable clipping still to do

- Game Capture source selection and automatic per-game switching through OBS win-capture
- Window/game selection and automatic game detection
- Crash-safe segment index and automatic disk-quota cleanup
- HDR-to-SDR tone mapping and capture performance telemetry

## Advanced player and editor

- Frame stepping, playback-speed controls, and proxy quality controls
- Waveform, audio scrubbing, track selection, and audio meters
- Non-destructive cuts, crop, captions, transitions, and text/sticker/image layers
- Background proxy generation and cancellable render jobs

## Publishing hardening

- Persistent retry queue with exponential backoff and resumable-session recovery
- YouTube thumbnails, tags, category, audience, and scheduling controls
- Per-platform byte-accurate upload progress and cancellation

## Medal-class experience

- Game/session timeline and clip suggestions
- Optional on-device speech transcription and highlight detection
- Friends, links, reactions, profiles, and a web viewer (requires a backend service)
- Cloud sync with quotas, deletion/export controls, and moderation tooling
- Signed auto-updates and opt-in crash reporting

## Security and privacy rules

- Clips stay local until a user explicitly enables a destination or an auto-publish rule.
- OAuth credentials go in the OS credential vault, never JSON config or logs.
- Every upload job stores its exact destination and visibility for auditability.
- Unlisted must never be represented as private. LBRY claims are exposed as public only.
