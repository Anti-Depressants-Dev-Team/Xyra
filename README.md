# Xyra

Xyra is a Windows-first, local-first game clipping desktop app written in Rust. It provides a rolling replay buffer, clip library, native player, trim/export controls, and background publishing to YouTube and Odysee/LBRY.

## What works now

- Rust-native desktop UI (`egui`/`eframe`)
- Bundled OBS Studio 32.2.1/libobs capture runtime -- no separate OBS install required
- OBS-backed rolling desktop replay buffer using native DirectX display capture and WASAPI audio
- Automatic NVENC, AMF, Quick Sync, or software encoder detection
- Save the most recent 5-300 seconds without recording an entire session
- Local MP4 clip library
- Native in-app player with streaming audio, play/pause, seeking, skip controls, mute, and volume
- Trim in/out and background export to H.264/AAC MP4
- Background clip saving, library probing, exports, and uploads so the UI stays responsive
- Google OAuth with refresh tokens secured by Windows Credential Manager
- Resumable YouTube uploads: Public, Unlisted, or Private
- Odysee publishing through the local LBRY SDK (Public only)
- Optional automatic upload after a clip is saved

## Install it

Download `Xyra-Setup-<version>-x64.exe` from the GitHub Releases page and run the installer. Releases publish the installer and its SHA-256 checksum; the portable application executable is intentionally not published. The installer includes Xyra's curated OBS runtime and FFmpeg helper, so users do not need to install OBS Studio separately.

The installer uses the current-user Programs folder, adds Xyra to Windows' installed-app list, creates a Start Menu shortcut, offers an optional desktop shortcut, and includes an uninstaller. Administrator access is not required.

## Run it from source

1. Install OBS Studio 32.2.1 for Windows, or stage its curated runtime from an existing install:

   ```powershell
   & scripts/stage-obs-runtime.ps1 -Destination obs-runtime
   $env:XYRA_OBS_RUNTIME = (Resolve-Path obs-runtime).Path
   $env:PATH = "$env:XYRA_OBS_RUNTIME;$env:PATH"
   ```

2. Run:

   ```powershell
   cargo run --release
   ```

3. On first launch, Xyra downloads its separate FFmpeg utility into private app data for playback, trimming, exports, and the legacy capture fallback.
4. Click **Start replay buffer**, wait a few seconds, and then click **Save last 30s**.

A custom FFmpeg path remains available in Settings for development and troubleshooting.

## Connect publishing

- **YouTube:** Create a Google OAuth 2.0 Desktop client with the YouTube Data API enabled, paste its client ID (and its client secret when supplied) into the Publish tab, then choose **Connect YouTube**. Google opens in the system browser; Xyra stores the resulting refresh token in Windows Credential Manager.
- **Odysee:** Run an authenticated local LBRY SDK daemon, then configure its API URL, claim bid, and optional channel claim ID in the Publish tab.

OBS, libobs Rust bindings, and FFmpeg attribution and redistribution information is in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License

Xyra is licensed under GPL-2.0-or-later. The distributed Windows application links GPLv3 Rust bindings for libobs, so the combined installer is distributed under GPLv3-compatible terms and includes the GPLv3 text in [`licenses/GPL-3.0.txt`](licenses/GPL-3.0.txt). Source for the exact upstream OBS and binding versions is linked in the third-party notices.

## Product boundaries

YouTube exposes `public`, `unlisted`, and `private` through `videos.insert`. New, unaudited Google API projects are restricted to private uploads until the project passes Google's compliance audit.

LBRY publications are public blockchain claims. The local SDK does not expose YouTube-equivalent Private or Unlisted visibility, so Xyra only offers Public for Odysee instead of promising privacy the protocol cannot provide.

## Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md) for the remaining path from this alpha to a Medal-class product.
