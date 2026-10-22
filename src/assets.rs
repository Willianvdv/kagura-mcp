//! Character library: the on-disk manifest, the built-in presets, and
//! downloading new clips from YouTube with yt-dlp.

use crate::util::{assets_dir, log, ytdlp_bin};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// One usable overlay clip on disk.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Character {
    pub id: String,
    pub name: String,
    /// File name relative to the assets dir.
    pub file: String,
    /// Chroma key colour, ffmpeg syntax, e.g. "0x00FF00".
    #[serde(default = "default_key_color")]
    pub key_color: String,
    /// chromakey similarity (0..1). Higher removes more of the key colour.
    #[serde(default = "default_similarity")]
    pub similarity: f32,
    /// chromakey blend (0..1). Softens the edge.
    #[serde(default = "default_blend")]
    pub blend: f32,
    /// Where it came from (URL / credit), for attribution.
    #[serde(default)]
    pub source: Option<String>,
    /// Cached character bounding box as ffmpeg "w:h:x:y", auto-detected on first
    /// use so corner positioning works. None = not detected yet.
    #[serde(default)]
    pub crop: Option<String>,
}

fn default_key_color() -> String {
    "auto".to_string()
}
fn default_similarity() -> f32 {
    0.15
}
fn default_blend() -> f32 {
    0.08
}

impl Character {
    pub fn path(&self) -> PathBuf {
        assets_dir().join(&self.file)
    }
    pub fn exists(&self) -> bool {
        self.path().exists()
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub characters: Vec<Character>,
}

fn manifest_path() -> PathBuf {
    assets_dir().join("characters.json")
}

pub fn load_manifest() -> Manifest {
    let p = manifest_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            log(&format!("manifest parse error ({e}); starting empty"));
            Manifest::default()
        }),
        Err(_) => Manifest::default(),
    }
}

pub fn save_manifest(m: &Manifest) -> Result<(), String> {
    std::fs::create_dir_all(assets_dir()).map_err(|e| e.to_string())?;
    let s = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    std::fs::write(manifest_path(), s).map_err(|e| e.to_string())
}

pub fn find_character(id: &str) -> Option<Character> {
    load_manifest().characters.into_iter().find(|c| c.id == id)
}

/// Insert or replace a character by id, then persist.
pub fn upsert_character(c: Character) -> Result<(), String> {
    let mut m = load_manifest();
    m.characters.retain(|x| x.id != c.id);
    m.characters.push(c);
    save_manifest(&m)
}

// ---------------------------------------------------------------------------
// Built-in presets
// ---------------------------------------------------------------------------

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub key_color: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "shigure-ui-dance",
        name: "Shigure Ui – Loli Dance",
        url: "https://www.youtube.com/watch?v=cNqZPeiWrSs",
        key_color: "auto",
    },
    Preset {
        id: "shigure-ui-9mm",
        name: "Shigure Ui – 9mm Goes Bang (shadow)",
        url: "https://www.youtube.com/watch?v=AqVQ7Gmi7qs",
        key_color: "auto",
    },
    Preset {
        id: "evernight-chibi",
        name: "Evernight (HSR) Chibi Dance – Free to Use 4K",
        url: "https://www.youtube.com/watch?v=lewgIOYrjy4",
        key_color: "auto",
    },
];

pub fn find_preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

// ---------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------

/// Download `url` into the assets dir as `<id>.mp4` using yt-dlp, returning the
/// resulting file name (relative to the assets dir).
pub fn download_youtube(url: &str, id: &str) -> Result<String, String> {
    std::fs::create_dir_all(assets_dir()).map_err(|e| e.to_string())?;
    let out_tmpl = assets_dir().join(format!("{id}.%(ext)s"));
    let bin = ytdlp_bin();

    log(&format!("yt-dlp downloading {url} -> {id}.mp4"));
    let output = Command::new(&bin)
        .arg("--no-playlist")
        .arg("-f")
        .arg("bv*+ba/b")
        .arg("--merge-output-format")
        .arg("mp4")
        .arg("-o")
        .arg(&out_tmpl)
        .arg(url)
        .output()
        .map_err(|e| {
            format!(
                "could not run yt-dlp ({bin}): {e}. Install it with `brew install yt-dlp` \
                 or set YT_DLP_BIN."
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "yt-dlp failed (exit {}):\n{}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    let file = format!("{id}.mp4");
    if !assets_dir().join(&file).exists() {
        // yt-dlp may have produced a non-mp4 container despite the merge flag.
        return Err(format!(
            "download finished but {file} was not found in the assets dir — \
             check the yt-dlp output format"
        ));
    }
    Ok(file)
}
