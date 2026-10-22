//! Small helpers: locating external binaries and logging to stderr.

use std::path::{Path, PathBuf};

/// Resolve an external binary. Order: explicit env var, Homebrew path, bare name
/// (relying on PATH). MCP servers are often launched with a minimal PATH, so the
/// Homebrew fallback matters on this machine.
pub fn tool_bin(name: &str, env_var: &str) -> String {
    if let Ok(v) = std::env::var(env_var) {
        if !v.is_empty() {
            return v;
        }
    }
    let brew = format!("/opt/homebrew/bin/{name}");
    if Path::new(&brew).exists() {
        return brew;
    }
    name.to_string()
}

pub fn ffmpeg_bin() -> String {
    tool_bin("ffmpeg", "FFMPEG_BIN")
}
pub fn ffprobe_bin() -> String {
    tool_bin("ffprobe", "FFPROBE_BIN")
}
pub fn ytdlp_bin() -> String {
    tool_bin("yt-dlp", "YT_DLP_BIN")
}

/// Directory holding the character clips + characters.json manifest.
/// Overridable with DANCE_ASSETS_DIR; defaults to <crate>/assets.
pub fn assets_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DANCE_ASSETS_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))
}

/// Log a line to stderr (stdout is reserved for the JSON-RPC protocol).
pub fn log(msg: &str) {
    eprintln!("[kagura] {msg}");
}
