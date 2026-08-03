# Third-party notices

## FFmpeg

Xyra automatically downloads an FFmpeg executable on first launch and invokes it as a separate process for capture and export.

- Project: <https://ffmpeg.org/>
- Source code: <https://ffmpeg.org/download.html#get-sources>
- License information: <https://ffmpeg.org/legal.html>
- Windows binary provider: <https://www.gyan.dev/ffmpeg/builds/>

The Windows Essentials build currently selected by the managed downloader reports a GPLv3 configuration. FFmpeg remains a separate third-party program and is not covered by Xyra's MIT license.

## ffmpeg-sidecar

Xyra uses `ffmpeg-sidecar` to download and extract the platform-appropriate FFmpeg command-line executable.

- Project: <https://github.com/nathanbabcock/ffmpeg-sidecar>
- License: MIT
