//! Kagura — a Model Context Protocol server (stdio, JSON-RPC 2.0) that overlays
//! green-screen anime dance clips onto videos with ffmpeg.
//!
//! The MCP stdio transport is newline-delimited JSON-RPC: one message per line
//! on stdin, one response line on stdout. stderr is free for logging.

mod assets;
mod ffmpeg;
mod util;

use assets::{Character, PRESETS};
use ffmpeg::{OverlayParams, Position};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use util::log;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "kagura";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    log("starting (stdio)");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                log(&format!("stdin read error: {e}"));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                log(&format!("bad json: {e}"));
                continue;
            }
        };

        // Notifications have no "id" and expect no response.
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        if id.is_none() {
            log(&format!("notification: {method}"));
            continue;
        }
        let id = id.unwrap();

        let response = match handle(method, params) {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(rpc) => json!({"jsonrpc":"2.0","id":id,"error":{"code":rpc.0,"message":rpc.1}}),
        };

        if let Ok(s) = serde_json::to_string(&response) {
            if writeln!(out, "{s}").is_err() || out.flush().is_err() {
                log("stdout closed; exiting");
                break;
            }
        }
    }
    log("stopped");
}

/// (code, message) for a JSON-RPC error.
type RpcError = (i64, String);

fn handle(method: &str, params: Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_specs() })),
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or((-32602, "missing tool name".to_string()))?;
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(call_tool(name, &args))
        }
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Tool specs (tools/list)
// ---------------------------------------------------------------------------

fn tool_specs() -> Value {
    json!([
        {
            "name": "list_characters",
            "description": "List installed green-screen dance characters plus the built-in presets you can install.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "install_preset",
            "description": "Download one of the built-in presets from YouTube and register it as a character. Preset ids: shigure-ui-dance, shigure-ui-9mm, evernight-chibi.",
            "inputSchema": {
                "type":"object",
                "properties": {
                    "id": {"type":"string","description":"preset id"}
                },
                "required":["id"]
            }
        },
        {
            "name": "add_character_from_youtube",
            "description": "Download any YouTube URL with yt-dlp and register it as a green-screen character.",
            "inputSchema": {
                "type":"object",
                "properties": {
                    "url": {"type":"string","description":"YouTube URL"},
                    "id": {"type":"string","description":"short id/slug to store it under"},
                    "name": {"type":"string","description":"display name (optional)"},
                    "key_color": {"type":"string","description":"chroma key colour, ffmpeg syntax, default 0x00FF00"}
                },
                "required":["url","id"]
            }
        },
        {
            "name": "inspect_video",
            "description": "Probe a video file with ffprobe (dimensions, duration, fps, codec).",
            "inputSchema": {
                "type":"object",
                "properties": { "path": {"type":"string"} },
                "required":["path"]
            }
        },
        {
            "name": "preview_overlay",
            "description": "Render a single composite frame (PNG) so you can check placement before a full render.",
            "inputSchema": { "type":"object", "properties": preview_props(), "required":["input","character"] }
        },
        {
            "name": "overlay_dance",
            "description": "Overlay a green-screen dance character onto a video and render the result to an mp4.",
            "inputSchema": { "type":"object", "properties": overlay_props(), "required":["input","character","output"] }
        }
    ])
}

fn overlay_props() -> Value {
    json!({
        "input": {"type":"string","description":"path to the source video"},
        "character": {"type":"string","description":"character id (see list_characters)"},
        "output": {"type":"string","description":"path to write the mp4"},
        "position": {"type":"string","enum":["bottom-right","bottom-left","top-right","top-left","center"],"description":"default bottom-right"},
        "height_fraction": {"type":"number","description":"character height as fraction of video height, default 0.4"},
        "margin": {"type":"integer","description":"edge margin in px, default 16"},
        "similarity": {"type":"number","description":"chromakey similarity 0..1 (override)"},
        "blend": {"type":"number","description":"chromakey blend 0..1 (override)"},
        "flip": {"type":"boolean","description":"mirror the character horizontally"},
        "loop": {"type":"boolean","description":"loop the clip to cover the whole video, default true"}
    })
}

fn preview_props() -> Value {
    let mut v = overlay_props();
    // preview uses a time and an optional output instead of a required output.
    let obj = v.as_object_mut().unwrap();
    obj.remove("output");
    obj.insert("output".into(), json!({"type":"string","description":"PNG path (optional; defaults to a temp file)"}));
    obj.insert("time".into(), json!({"type":"number","description":"timestamp in seconds to sample, default 1.0"}));
    v
}

// ---------------------------------------------------------------------------
// Tool dispatch -> MCP tools/call result
// ---------------------------------------------------------------------------

fn call_tool(name: &str, args: &Value) -> Value {
    let r = match name {
        "list_characters" => tool_list_characters(),
        "install_preset" => tool_install_preset(args),
        "add_character_from_youtube" => tool_add_youtube(args),
        "inspect_video" => tool_inspect(args),
        "preview_overlay" => tool_preview(args),
        "overlay_dance" => tool_overlay(args),
        other => Err(format!("unknown tool: {other}")),
    };
    match r {
        Ok(text) => json!({ "content": [ {"type":"text","text": text} ] }),
        Err(e) => json!({ "content": [ {"type":"text","text": format!("Error: {e}")} ], "isError": true }),
    }
}

fn tool_list_characters() -> Result<String, String> {
    let m = assets::load_manifest();
    let mut s = String::new();
    s.push_str("Installed characters:\n");
    if m.characters.is_empty() {
        s.push_str("  (none yet — install a preset or add from YouTube)\n");
    } else {
        for c in &m.characters {
            let ok = if c.exists() { "" } else { "  [FILE MISSING]" };
            s.push_str(&format!(
                "  • {id}  \"{name}\"  key={key} sim={sim} blend={blend}{ok}\n",
                id = c.id,
                name = c.name,
                key = c.key_color,
                sim = c.similarity,
                blend = c.blend,
                ok = ok
            ));
        }
    }
    s.push_str("\nBuilt-in presets (install_preset <id>):\n");
    for p in PRESETS {
        let installed = m.characters.iter().any(|c| c.id == p.id);
        let tag = if installed { " (installed)" } else { "" };
        s.push_str(&format!("  • {}  \"{}\"  {}{}\n", p.id, p.name, p.url, tag));
    }
    Ok(s)
}

fn tool_install_preset(args: &Value) -> Result<String, String> {
    let id = get_str(args, "id").ok_or("missing 'id'")?;
    let preset = assets::find_preset(&id)
        .ok_or_else(|| format!("no preset '{id}'. Known: shigure-ui-dance, shigure-ui-9mm, evernight-chibi"))?;
    let file = assets::download_youtube(preset.url, preset.id)?;
    let _ = ffmpeg::trim_to_content(&util::assets_dir().join(&file).to_string_lossy());
    let ch = Character {
        id: preset.id.to_string(),
        name: preset.name.to_string(),
        file,
        key_color: preset.key_color.to_string(),
        similarity: 0.15,
        blend: 0.08,
        source: Some(preset.url.to_string()),
        crop: None,
    };
    let ch = ensure_character_ready(ch);
    assets::upsert_character(ch.clone())?;
    Ok(format!(
        "Installed preset '{}' as character '{}' ({}).",
        preset.id,
        ch.id,
        ch.path().display()
    ))
}

fn tool_add_youtube(args: &Value) -> Result<String, String> {
    let url = get_str(args, "url").ok_or("missing 'url'")?;
    let id = get_str(args, "id").ok_or("missing 'id'")?;
    let name = get_str(args, "name").unwrap_or_else(|| id.clone());
    let key_color = get_str(args, "key_color").unwrap_or_else(|| "auto".to_string());
    let file = assets::download_youtube(&url, &id)?;
    let _ = ffmpeg::trim_to_content(&util::assets_dir().join(&file).to_string_lossy());
    let ch = Character {
        id: id.clone(),
        name,
        file,
        key_color,
        similarity: 0.15,
        blend: 0.08,
        source: Some(url),
        crop: None,
    };
    let ch = ensure_character_ready(ch);
    assets::upsert_character(ch.clone())?;
    Ok(format!(
        "Added character '{}' ({}).",
        ch.id,
        ch.path().display()
    ))
}

fn tool_inspect(args: &Value) -> Result<String, String> {
    let path = get_str(args, "path").ok_or("missing 'path'")?;
    let info = ffmpeg::probe(&path)?;
    Ok(format!(
        "{path}\n  {w}x{h}  {d:.2}s  {fps:.3} fps  codec={codec}",
        w = info.width,
        h = info.height,
        d = info.duration,
        fps = info.fps,
        codec = info.codec
    ))
}

fn tool_preview(args: &Value) -> Result<String, String> {
    let input = get_str(args, "input").ok_or("missing 'input'")?;
    let ch = cache_ready(resolve_character(args)?);
    let params = overlay_params(args, &ch);
    let time = get_f64(args, "time").unwrap_or(1.0);
    let output = get_str(args, "output").unwrap_or_else(|| {
        std::env::temp_dir()
            .join(format!("dance_preview_{}.png", std::process::id()))
            .to_string_lossy()
            .to_string()
    });
    ffmpeg::preview_frame(&input, &ch, &output, time, &params)?;
    Ok(format!("Preview frame written to {output}"))
}

fn tool_overlay(args: &Value) -> Result<String, String> {
    let input = get_str(args, "input").ok_or("missing 'input'")?;
    let output = get_str(args, "output").ok_or("missing 'output'")?;
    let ch = cache_ready(resolve_character(args)?);
    let params = overlay_params(args, &ch);
    if !ch.exists() {
        return Err(format!(
            "character file missing: {}. Install it first.",
            ch.path().display()
        ));
    }
    ffmpeg::render_overlay(&input, &ch, &output, &params)?;
    Ok(format!("Rendered {output}"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_character(args: &Value) -> Result<Character, String> {
    let id = get_str(args, "character").ok_or("missing 'character'")?;
    assets::find_character(&id)
        .ok_or_else(|| format!("no character '{id}'. Use list_characters / install_preset first."))
}

/// Make sure a character has its bounding-box crop detected (needed so corner
/// positioning lands the character, not the clip's big green canvas). Detection
/// runs once per character and is cached in the manifest. Infallible: on failure
/// the crop stays None and the overlay just uses the full frame.
fn ensure_character_ready(mut ch: Character) -> Character {
    if ch.crop.is_some() || !ch.exists() {
        return ch;
    }
    let key = match ffmpeg::resolve_key_color(&ch) {
        Ok(k) => k,
        Err(_) => return ch,
    };
    if let Some(crop) = ffmpeg::detect_crop(&ch.path().to_string_lossy(), &key) {
        ch.crop = Some(crop);
    }
    ch
}

/// ensure_character_ready + persist a newly-detected crop back to the manifest.
fn cache_ready(ch: Character) -> Character {
    let had_crop = ch.crop.is_some();
    let ready = ensure_character_ready(ch);
    if !had_crop && ready.crop.is_some() {
        let _ = assets::upsert_character(ready.clone());
    }
    ready
}

fn overlay_params(args: &Value, ch: &Character) -> OverlayParams {
    let position = get_str(args, "position")
        .and_then(|s| Position::parse(&s).ok())
        .unwrap_or(Position::BottomRight);
    OverlayParams {
        position,
        height_fraction: get_f64(args, "height_fraction").unwrap_or(0.4),
        margin: get_u64(args, "margin").unwrap_or(16) as u32,
        similarity: get_f64(args, "similarity").map(|x| x as f32).unwrap_or(ch.similarity),
        blend: get_f64(args, "blend").map(|x| x as f32).unwrap_or(ch.blend),
        flip: get_bool(args, "flip").unwrap_or(false),
        loop_overlay: get_bool(args, "loop").unwrap_or(true),
    }
}

fn get_str(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string())
}
fn get_f64(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}
fn get_u64(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64())
}
fn get_bool(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(|x| x.as_bool())
}
