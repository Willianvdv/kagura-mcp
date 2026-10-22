//! ffmpeg/ffprobe glue: probe a video, build the chroma-key overlay filter,
//! and run full renders or single-frame previews.

use crate::assets::Character;
use crate::util::{ffmpeg_bin, ffprobe_bin, log};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub fps: f64,
    pub codec: String,
}

/// Probe a video file with ffprobe.
pub fn probe(path: &str) -> Result<VideoInfo, String> {
    let bin = ffprobe_bin();
    let out = Command::new(&bin)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,codec_name",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("could not run ffprobe ({bin}): {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "ffprobe failed for {path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe json: {e}"))?;
    let stream = v
        .get("streams")
        .and_then(|s| s.get(0))
        .ok_or_else(|| format!("no video stream in {path}"))?;

    let width = stream.get("width").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let height = stream.get("height").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let codec = stream
        .get("codec_name")
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string();
    let fps = stream
        .get("r_frame_rate")
        .and_then(|x| x.as_str())
        .map(parse_fraction)
        .unwrap_or(0.0);
    let duration = v
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);

    if width == 0 || height == 0 {
        return Err(format!("could not read dimensions of {path}"));
    }

    Ok(VideoInfo {
        width,
        height,
        duration,
        fps,
        codec,
    })
}

/// Sample the top-left corner pixel of a clip's first frame to guess its chroma
/// key colour. Green-screen clips are rarely pure 0x00FF00 (these are ~0x00BF4C),
/// so keying against the real colour matters.
pub fn detect_key_color(path: &str) -> Result<String, String> {
    let bin = ffmpeg_bin();
    // Sample ~30% into the clip, not frame 0: green-screen clips often fade in
    // from black, so frame 0's corner can be pure black.
    let t = probe(path)
        .map(|i| (i.duration * 0.3).clamp(0.3, 2.0))
        .unwrap_or(1.0);
    let out = Command::new(&bin)
        .args(["-v", "error", "-ss", &format!("{t}"), "-i"])
        .arg(path)
        .args([
            "-frames:v", "1",
            "-vf", "crop=2:2:0:0,scale=1:1",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
        ])
        .output()
        .map_err(|e| format!("could not run ffmpeg for key detection ({bin}): {e}"))?;
    if !out.status.success() || out.stdout.len() < 3 {
        return Err(format!(
            "could not sample key colour from {path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let (r, g, b) = (out.stdout[0], out.stdout[1], out.stdout[2]);
    Ok(format!("0x{r:02X}{g:02X}{b:02X}"))
}

/// Resolve a character's configured key colour, auto-detecting when it is
/// "auto"/empty.
pub fn resolve_key_color(ch: &Character) -> Result<String, String> {
    let k = ch.key_color.trim();
    if k.is_empty() || k.eq_ignore_ascii_case("auto") {
        let c = detect_key_color(&ch.path().to_string_lossy())?;
        log(&format!("auto key colour for {} = {}", ch.id, c));
        Ok(c)
    } else {
        Ok(k.to_string())
    }
}

/// Find the character's bounding box across the whole clip, as an ffmpeg
/// `crop=w:h:x:y` string. Keys out the background, turns the result into an
/// alpha mask, and lets cropdetect (reset=0 = union over all frames) report the
/// box that contains the character at every point in the dance.
pub fn detect_crop(path: &str, key_color: &str) -> Option<String> {
    let bin = ffmpeg_bin();
    let vf = format!(
        "fps=6,chromakey={key_color}:0.18:0.04,format=yuva420p,alphaextract,cropdetect=limit=16:round=2:reset=0"
    );
    let out = Command::new(&bin)
        .args(["-hide_banner", "-i"])
        .arg(path)
        .args(["-vf", &vf, "-an", "-f", "null", "-"])
        .output()
        .ok()?;
    let logs = String::from_utf8_lossy(&out.stderr);
    // cropdetect prints "... crop=W:H:X:Y" repeatedly; with reset=0 the last one
    // is the union over the whole clip.
    let mut last: Option<String> = None;
    for tok in logs.split_whitespace() {
        if let Some(rest) = tok.strip_prefix("crop=") {
            last = Some(rest.to_string());
        }
    }
    let s = last?;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 4 && parts.iter().all(|p| p.parse::<i64>().map(|n| n >= 0).unwrap_or(false)) {
        log(&format!("auto crop for clip = {s}"));
        Some(s)
    } else {
        None
    }
}

/// Re-encode a clip to just its non-black content range, so a looped overlay
/// never starts or wraps on fade-to/from-black frames. Modifies the file in
/// place. Returns Ok(true) if it trimmed, Ok(false) if there was nothing to do.
pub fn trim_to_content(path: &str) -> Result<bool, String> {
    let info = probe(path)?;
    let dur = info.duration;
    let bin = ffmpeg_bin();
    let out = Command::new(&bin)
        .args(["-hide_banner", "-i"])
        .arg(path)
        .args(["-vf", "blackdetect=d=0.01:pic_th=0.98", "-an", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("blackdetect run: {e}"))?;
    let logs = String::from_utf8_lossy(&out.stderr);

    let mut start = 0.0_f64;
    let mut end = dur;
    for line in logs.lines() {
        if !line.contains("black_start:") {
            continue;
        }
        let bs = parse_after(line, "black_start:");
        let be = parse_after(line, "black_end:");
        if let (Some(bs), Some(be)) = (bs, be) {
            if bs <= 0.1 && be > start {
                start = be; // leading black fade-in
            }
            if be >= dur - 0.1 && bs < end {
                end = bs; // trailing black fade-out
            }
        }
    }

    if start < 0.05 && end > dur - 0.05 {
        return Ok(false); // no meaningful black at either end
    }
    let new_dur = end - start;
    if new_dur < 0.3 {
        return Ok(false); // refuse to trim down to nothing
    }

    let tmp = format!("{path}.trim.mp4");
    let status = Command::new(&bin)
        .args(["-y", "-ss", &format!("{start}"), "-t", &format!("{new_dur}"), "-i"])
        .arg(path)
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-an", &tmp])
        .status()
        .map_err(|e| format!("trim encode: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err("trim re-encode failed".into());
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    log(&format!("trimmed {path} to {start:.2}..{end:.2}s (was 0..{dur:.2}s)"));
    Ok(true)
}

/// Parse the float immediately after `key` on a line (e.g. "black_start:0.83").
fn parse_after(line: &str, key: &str) -> Option<f64> {
    let i = line.find(key)? + key.len();
    let num: String = line[i..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse().ok()
}

fn parse_fraction(s: &str) -> f64 {
    match s.split_once('/') {
        Some((n, d)) => {
            let n: f64 = n.parse().unwrap_or(0.0);
            let d: f64 = d.parse().unwrap_or(1.0);
            if d == 0.0 {
                0.0
            } else {
                n / d
            }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

/// Where to place the overlay.
#[derive(Clone, Copy, Debug)]
pub enum Position {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
    Center,
}

impl Position {
    pub fn parse(s: &str) -> Result<Position, String> {
        Ok(match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "bottom-right" | "br" => Position::BottomRight,
            "bottom-left" | "bl" => Position::BottomLeft,
            "top-right" | "tr" => Position::TopRight,
            "top-left" | "tl" => Position::TopLeft,
            "center" | "centre" | "c" => Position::Center,
            other => return Err(format!("unknown position '{other}'")),
        })
    }

    /// overlay x:y expression for a given margin (px).
    fn expr(&self, m: u32) -> String {
        match self {
            Position::BottomRight => format!("W-w-{m}:H-h-{m}"),
            Position::BottomLeft => format!("{m}:H-h-{m}"),
            Position::TopRight => format!("W-w-{m}:{m}"),
            Position::TopLeft => format!("{m}:{m}"),
            Position::Center => "(W-w)/2:(H-h)/2".to_string(),
        }
    }
}

/// Parameters shared by preview + full render.
pub struct OverlayParams {
    pub position: Position,
    pub height_fraction: f64,
    pub margin: u32,
    pub similarity: f32,
    pub blend: f32,
    pub flip: bool,
    pub loop_overlay: bool,
}

/// Build the `-filter_complex` string. `main` is used to turn the height
/// fraction into an even pixel height for the overlay.
fn build_filter(main: &VideoInfo, key_color: &str, crop: Option<&str>, p: &OverlayParams) -> String {
    let mut target_h = (main.height as f64 * p.height_fraction).round() as i64;
    if target_h < 2 {
        target_h = 2;
    }
    if target_h % 2 != 0 {
        target_h += 1; // keep even for yuv420p
    }

    // Crop the clip down to the character's bounding box first, so corner
    // positioning actually lands the character in the corner (clips frame the
    // character small inside a big green canvas).
    let crop_prefix = match crop {
        Some(c) if !c.is_empty() => format!("crop={c},"),
        _ => String::new(),
    };

    let mut chain = format!(
        "[1:v]{crop}chromakey={color}:{sim}:{blend},scale=-2:{h}",
        crop = crop_prefix,
        color = key_color,
        sim = p.similarity,
        blend = p.blend,
        h = target_h
    );
    if p.flip {
        chain.push_str(",hflip");
    }
    chain.push_str("[ck];");

    let shortest = if p.loop_overlay { ":shortest=1" } else { "" };
    chain.push_str(&format!(
        "[0:v][ck]overlay={xy}{shortest}:format=auto[v]",
        xy = p.position.expr(p.margin),
    ));
    chain
}

/// Full render to `output` (mp4). Returns the ffmpeg argv actually run.
pub fn render_overlay(
    input: &str,
    ch: &Character,
    output: &str,
    p: &OverlayParams,
) -> Result<Vec<String>, String> {
    let main = probe(input)?;
    let key_color = resolve_key_color(ch)?;
    let filter = build_filter(&main, &key_color, ch.crop.as_deref(), p);
    let overlay_path = ch.path();
    let overlay_path = overlay_path.to_string_lossy().to_string();

    let mut args: Vec<String> = vec!["-y".into()];
    args.push("-i".into());
    args.push(input.into());
    if p.loop_overlay {
        args.push("-stream_loop".into());
        args.push("-1".into());
    }
    args.push("-i".into());
    args.push(overlay_path);
    args.push("-filter_complex".into());
    args.push(filter);
    args.push("-map".into());
    args.push("[v]".into());
    args.push("-map".into());
    args.push("0:a?".into());
    args.extend(
        [
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "copy",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(output.into());

    run_ffmpeg(&args)?;
    Ok(args)
}

/// Render a single composite frame to a PNG at timestamp `t` (seconds).
pub fn preview_frame(
    input: &str,
    ch: &Character,
    output: &str,
    t: f64,
    p: &OverlayParams,
) -> Result<Vec<String>, String> {
    let main = probe(input)?;
    let key_color = resolve_key_color(ch)?;
    let filter = build_filter(&main, &key_color, ch.crop.as_deref(), p);
    let overlay_path = ch.path().to_string_lossy().to_string();

    let mut args: Vec<String> = vec!["-y".into(), "-ss".into(), format!("{t}")];
    args.push("-i".into());
    args.push(input.into());
    if p.loop_overlay {
        args.push("-stream_loop".into());
        args.push("-1".into());
    }
    // Seek the overlay clip to the same timestamp so the preview shows a frame
    // where the character is actually on screen (clips often start on empty green).
    args.push("-ss".into());
    args.push(format!("{t}"));
    args.push("-i".into());
    args.push(overlay_path);
    args.push("-filter_complex".into());
    args.push(filter);
    args.push("-map".into());
    args.push("[v]".into());
    args.push("-frames:v".into());
    args.push("1".into());
    args.push(output.into());

    run_ffmpeg(&args)?;
    Ok(args)
}

fn run_ffmpeg(args: &[String]) -> Result<(), String> {
    let bin = ffmpeg_bin();
    log(&format!("ffmpeg {}", args.join(" ")));
    let out = Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("could not run ffmpeg ({bin}): {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // ffmpeg is verbose; keep the tail where the real error usually is.
        let tail: String = stderr.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(format!("ffmpeg failed:\n{tail}"));
    }
    Ok(())
}
