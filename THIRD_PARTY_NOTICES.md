# Third-party notices

## OBS Studio / libobs

Xyra installers contain a curated runtime from OBS Studio 32.2.1 and link directly to libobs for DirectX capture, WASAPI audio, replay buffering, and hardware encoding.

- Project: <https://github.com/obsproject/obs-studio>
- Exact source: <https://github.com/obsproject/obs-studio/tree/32.2.1>
- Release: <https://github.com/obsproject/obs-studio/releases/tag/32.2.1>
- License: GPL-2.0-or-later

The OBS runtime is staged without the OBS frontend, browser, WebSocket, scripting, or service plugins. Xyra loads only the capture, audio, filter, muxing, and encoder modules needed by the application.

## libobs-rs bindings

Xyra uses the maintained `libobs`, `libobs-wrapper`, and `libobs-simple` Rust crates to call libobs.

- Project: <https://github.com/sshcrack/libobs-rs>
- Crates: `libobs 5.0.1+32.0.4`, `libobs-wrapper 9.0.4+32.0.2`, `libobs-simple 8.0.1+32.0.2`
- License declared by the crates: GPL-3.0
- GPLv3 text included with Xyra: [`licenses/GPL-3.0.txt`](licenses/GPL-3.0.txt)

## FFmpeg

Xyra automatically downloads an FFmpeg executable on first launch and invokes it as a separate process for playback, trimming, export, and the optional legacy capture backend. The curated OBS runtime also includes OBS' FFmpeg libraries and muxer helper.

- Project: <https://ffmpeg.org/>
- Source code: <https://ffmpeg.org/download.html#get-sources>
- License information: <https://ffmpeg.org/legal.html>
- Windows binary provider: <https://www.gyan.dev/ffmpeg/builds/>

The Windows Essentials build currently selected by the managed downloader reports a GPLv3 configuration. The downloaded command-line executable remains a separate third-party program; Xyra's own source is GPL-2.0-or-later.

## ffmpeg-sidecar

Xyra uses `ffmpeg-sidecar` to download and extract the platform-appropriate FFmpeg command-line executable.

- Project: <https://github.com/nathanbabcock/ffmpeg-sidecar>
- License: MIT
