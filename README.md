# Xyra

Xyra is a Windows-first, local-first game clipping desktop app written in Rust. The current MVP provides a rolling replay buffer, a clip library, basic trim/export controls, and a provider-aware publishing queue for YouTube and Odysee.

## What works now

- Rust-native desktop UI (`egui`/`eframe`)
- Automatically managed FFmpeg runtime—no separate install or PATH setup
- FFmpeg-backed rolling desktop replay buffer
- Save the most recent 5–300 seconds without recording an entire session
- Local MP4 clip library
- Native in-app video preview with play, pause, and timeline seeking
- Trim in/out and export to H.264/AAC MP4
- Per-destination visibility validation
- YouTube: Public, Unlisted, Private
- Odysee: Public and Unlisted (the service does not offer ordinary private uploads)

## Install it

Download `Xyra-Setup-<version>-x64.exe` from the GitHub Releases page and run the installer. Releases publish the installer and its SHA-256 checksum; the portable application executable is intentionally not published.

The installer uses the current-user Programs folder, adds Xyra to Windows' installed-app list, creates a Start Menu shortcut, offers an optional desktop shortcut, and includes an uninstaller. Administrator access is not required.

## Run it from source

1. Run:

   ```powershell
   cargo run --release
   ```

2. On first launch, Xyra downloads FFmpeg into its private app-data runtime folder and shows live progress. This is a one-time download.
3. Click **Start replay buffer**, wait a few seconds, and then click **Save last 30s**.

A custom FFmpeg path remains available in Settings for development and troubleshooting.

FFmpeg attribution and redistribution information is in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Product boundaries

YouTube exposes `public`, `unlisted`, and `private` through `videos.insert`. New, unaudited Google API projects are restricted to private uploads until the project passes Google's compliance audit.

Odysee offers Public and Unlisted; anyone with the link can view an unlisted upload. It does not offer a normal Private choice, so Xyra does not promise one. A future encrypted-file feature could provide confidentiality, but it would not behave like a normal playable Odysee post.

## Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md) for the path from this executable MVP to a Medal-class product.
