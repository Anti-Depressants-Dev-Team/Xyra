# Xyra product roadmap

## Architecture

Xyra keeps product policy independent from OS-specific capture and platform-specific upload code:

```text
Rust desktop UI
  ├── capture service ──> Windows capture backend ──> segmented replay buffer
  ├── media service ───> decoder/player + non-destructive edit graph + exporter
  ├── library service ─> local metadata, thumbnails, tags, search
  └── publish service ─> durable queue ─┬─ YouTube OAuth + resumable upload
                                       └─ Odysee wallet + publish API
```

The MVP uses an FFmpeg subprocess for capture/export. Replacing capture with Windows Graphics Capture and Media Foundation will not change clip, edit, or publishing models.

## Milestone 1 — reliable clipping

- Global “save replay” hotkey
- Window/game selection and automatic game detection
- Desktop + microphone audio with separate tracks
- NVENC, AMF, and Quick Sync hardware encoders with software fallback
- Crash-safe segment index and automatic disk quota cleanup
- Multi-monitor, HDR-to-SDR tone mapping, and variable-resolution handling

## Milestone 2 — advanced player and editor

- Extend the native FFmpeg-to-GPU player with frame stepping and proxy quality controls
- Waveform, audio scrubbing, and audio meters
- Non-destructive cuts, crop, aspect-ratio presets, volume, captions, and transitions
- Text/sticker/image layers, cursor zoom, and keyframes
- Background proxy generation and cancellable render jobs

## Milestone 3 — accounts and publishing

- System-browser Google OAuth 2.0 with loopback redirect
- Encrypted refresh-token storage using Windows Credential Manager
- Resumable YouTube uploads, thumbnails, tags, category, audience, and scheduling
- Odysee account connection with public and unlisted publishing
- Persistent retry queue with exponential backoff and per-platform progress
- User-controlled “auto-publish after clipping” rules; default remains off

## Milestone 4 — Medal-class experience

- Game/session timeline and clip suggestions
- Optional on-device speech transcription and highlight detection
- Friends, links, reactions, profiles, and a web viewer (requires a backend service)
- Cloud sync with quotas, deletion/export controls, and moderation tooling
- Signed auto-updates, crash reporting consent, installer, and capture performance telemetry

## Security and privacy rules

- Clips stay local until a user explicitly enables a destination or an auto-publish rule.
- OAuth credentials go in the OS credential vault, never JSON config or logs.
- Every upload job stores its exact destination and visibility for auditability.
- “Unlisted” must never be represented as “private.” Anyone with an Odysee unlisted link can view it.
