# Recap

Give Recap a YouTube link, an audio file or a video file. It gives back a timestamped transcript
plus a structured summary: TL;DR, key points, action items and chapters.

Recap is a native Windows app (Rust + egui, a single ~9 MB `recap.exe`). There's no browser
and no web server. It uses the same look as [JAMP](https://github.com/The1Azreen/Just-Another-Music-Player).

```
Input  →  Get text  →  Clean/chunk  →  Summarize  →  Output
```

| Input | How the text is obtained |
|---|---|
| YouTube link | The video's own captions, fetched with `yt-dlp` without downloading the video. This takes seconds. With no captions, the audio is downloaded and transcribed. |
| Other video link | Audio downloaded with `yt-dlp`, then transcribed. |
| Audio file | Transcribed locally with whisper.cpp. |
| Video file | `ffmpeg` pulls the audio out, which is then transcribed. |
| Subtitle file (`.srt`, `.vtt`) | Read as it is, no transcription. A video or audio file with the same name next to it is kept for playback. |
| YouTube playlist link | Every video in it is added to the queue. |

## AMD GPU transcription

Recap transcribes with **whisper.cpp built with Vulkan**. Vulkan runs on any recent AMD, NVIDIA
or Intel GPU on Windows, including RDNA2 cards like the RX 6800 XT. The other options don't fit:
faster-whisper only accelerates on NVIDIA, and ROCm on Windows targets RDNA3/4.

whisper.cpp doesn't publish a Windows Vulkan binary, so build it once:

```powershell
winget install --id KhronosGroup.VulkanSDK -e      # once; needs VS 2022 C++ tools + CMake
powershell -ExecutionPolicy Bypass -File scripts\build_whisper_vulkan.ps1
```

This puts `whisper-cli.exe` and its DLLs in `~\Recap\tools\whisper.cpp`. The first transcription
downloads the `large-v3-turbo` model (1.6 GB) into `~\Recap\models`. The top bar shows
**GPU: Vulkan** once the GPU build is found.

## Setup

1. `cargo build --release` produces `target\release\recap.exe`.
2. External tools are looked up next to `recap.exe`, then in `~\Recap\tools`, then in the repo's
   git-ignored `tools\` folder (for dev builds under `target\`), then on `PATH`:
   - `ffmpeg.exe` (and `ffprobe.exe`)
   - `yt-dlp.exe` from [github.com/yt-dlp/yt-dlp](https://github.com/yt-dlp/yt-dlp/releases).
     Recap updates it to the latest nightly build in the background, at most once a day.
   - `whisper-cli.exe` from the build script above
3. For summaries and Q&A, add a Claude API key in **Settings** (kept in the Windows Credential
   Manager, not in a file), or set `ANTHROPIC_API_KEY`.
   Transcripts work without a key.

Settings → *Open tools folder* shows where to drop the tools. To make Recap portable, create a
`recap-data` folder next to `recap.exe`; everything is then kept there.

## Using it

- Drop several files (or pick several, or paste a playlist link) and they're recapped one
  after another; while a recap runs, **Add to queue** lines up the next one.
- Paste a link or drop a file on the window, pick a summary style in the top bar (General,
  Meeting, Lecture, Podcast) and press **Recap it**.
- Click a transcript line or chapter to play from that moment. Playback is native, through the
  same audio engine as JAMP. Uploaded files keep their audio. For YouTube recaps, press
  **Download audio** to play them inside Recap. **Open on YouTube** is the only thing that
  opens a browser, and only when you click it.
- **Transcripts** (top bar) opens a file explorer over the library: folders by source, month,
  summary style and method, large icons, a details pane, sorting, and a search through the full
  text of every transcript. Double-click a transcript to open it (at the match, when searching).
- The Library search covers every saved transcript. The Ask window answers questions about the
  open recap, with clickable timestamps.
- **Export…** writes `.md` (with YouTube timestamp links), `.srt`, `.txt` and `.json`.

From a terminal, the same pipeline runs without the window:

```powershell
recap --cli https://youtu.be/VIDEO_ID
recap --cli meeting.m4a --style meeting
recap --cli talk.mp4 --no-summary --lang en
```

Recaps are saved to `~\Recap\library\<id>\` (override with `RECAP_HOME`).

## How summaries work

Summaries call the Claude API (`claude-opus-5-5`) with structured JSON output. Transcripts up
to about two hours of speech go to Claude in one request. Longer ones are split on segment
boundaries; each part becomes dense notes (map), and the notes are merged into one summary
(reduce). Every transcript line carries its `[hh:mm:ss]` time, so chapters keep accurate
timestamps. For Q&A, the transcript is prompt-cached, so follow-up questions are cheaper.

## Layout

```
src/
  main.rs        window + --cli entry
  app.rs         UI (panels, library, transcript, player, settings)
  theme.rs       JAMP's look: night sky, neon windows, glow, pixel font
  audio.rs       JAMP's playback engine (Symphonia, ffmpeg for everything else)
  sources.rs     YouTube captions / downloads (yt-dlp), files
  transcribe.rs  whisper.cpp (Vulkan) runner + model download
  claude.rs      map-reduce summaries + Q&A
  library.rs     saved recaps
  export.rs      md / srt / txt / json
scripts/build_whisper_vulkan.ps1
```

## Not done yet

- Speaker labels. pyannote needs PyTorch, which has no AMD GPU path for this card on Windows.
- Video frames aren't shown. Recaps of video files play their audio; **Open video** opens the
  original in your default player.
