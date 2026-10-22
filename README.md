# Kagura 神楽 🕺

An MCP server (Rust, stdio) that overlays **green-screen anime dance clips** onto
your videos using ffmpeg chroma-key. It downloads clips straight from YouTube
with `yt-dlp`, auto-detects each clip's key colour and the dancer's bounding box,
strips black fade-in/out frames, and keeps a small library of "characters".

Named after *kagura* (神楽), the old Japanese sacred dance.

## Requirements

- `ffmpeg` + `ffprobe` (`brew install ffmpeg`)
- `yt-dlp` for downloading (`brew install yt-dlp`)
- Rust toolchain to build

Binaries are auto-located in `/opt/homebrew/bin` or on `PATH`. Override with
`FFMPEG_BIN`, `FFPROBE_BIN`, `YT_DLP_BIN`.

## Build

```sh
cargo build --release
# binary: target/release/kagura-mcp
```

## Register with Claude Code

```sh
claude mcp add -s user kagura -- /ABS/PATH/kagura-mcp/target/release/kagura-mcp
```

## Tools

| Tool | What it does |
|------|--------------|
| `list_characters` | List installed characters + the built-in presets |
| `install_preset` | Download a built-in preset from YouTube and register it |
| `add_character_from_youtube` | Download any YouTube URL and register it as a character |
| `inspect_video` | ffprobe summary (size / duration / fps / codec) |
| `preview_overlay` | Render one composite frame (PNG) to check placement fast |
| `overlay_dance` | Full render: overlay the character onto a video → mp4 |

### Built-in presets

| id | clip |
|----|------|
| `shigure-ui-dance` | Shigure Ui – Loli Dance |
| `shigure-ui-9mm` | Shigure Ui – 9mm Goes Bang (shadow) |
| `evernight-chibi` | Evernight (HSR) Chibi Dance – Free to Use 4K |

> ⚠️ The presets are fan/VTuber green-screen clips from YouTube. Fine for
> personal/local use; check licensing before publishing anything with them.
> `evernight-chibi` is marked "free to use" by its uploader.

## How it works

On install, each clip is processed once:

1. **Trim** — `blackdetect` strips the fade-in/out so a looped overlay never
   starts or wraps on a black frame.
2. **Key colour** — sampled ~30 % into the clip (green screens aren't `0x00FF00`;
   these are `~0x00BF4C` / `0x13FF0A`, and frame 0 is often black).
3. **Bounding box** — `cropdetect` on the keyed alpha mask finds the dancer's box
   across the whole dance, so corner placement lands the dancer, not the canvas.

Results are cached in `assets/characters.json`.

## Typical flow

1. `install_preset { "id": "evernight-chibi" }`
2. `preview_overlay { "input": "clip.mp4", "character": "evernight-chibi", "position": "bottom-right", "height_fraction": 0.45 }`
   → eyeball the PNG, tweak `position` / `similarity` / `blend` / `flip`
3. `overlay_dance { "input": "clip.mp4", "character": "evernight-chibi", "output": "clip_dancing.mp4" }`

### Overlay knobs

- `position`: `bottom-right` (default), `bottom-left`, `top-right`, `top-left`, `center`
- `height_fraction`: dancer height as a fraction of the video height (default `0.4`)
- `margin`: edge gap in px (default `16`)
- `similarity` / `blend`: chroma-key tuning (0–1)
- `flip`: mirror the dancer
- `loop`: loop the clip to cover the whole video (default `true`)

Audio from your source video is preserved; the dance clip's own audio is dropped.

## License

MIT
