//! External converters, found at run time like tesseract and never required:
//! `pandoc`, `antiword` / `catdoc`, `soffice` (`LibreOffice`), `ffprobe` /
//! `ffmpeg` and a local `whisper` / `whisper-cli`, plus the user's own
//! `[extract.converters]` hooks. Every tool is spawned from an argv array
//! (never a shell), in a private temp directory, with a timeout and an output
//! cap, and with credential environment variables removed.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::detect::Kind;

/// A user converter from `[extract.converters]`: `"<ext or mime>" = ["cmd",
/// "{in}"]`, stdout is Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    /// Lowercased extension (no dot) or MIME type.
    pub key: String,
    pub argv: Vec<String>,
}

/// Where converters are looked up and how hard they are limited.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    pub path_dirs: Vec<PathBuf>,
    pub hooks: Vec<Hook>,
    pub timeout: Duration,
    /// Audio / video transcription may legitimately run long.
    pub media_timeout: Duration,
    pub max_output: usize,
    /// `whisper` model name (the openai CLI downloads it on first use).
    pub whisper_model: String,
    /// A ggml model file for `whisper-cli` (whisper.cpp), if the user has one.
    pub whisper_cpp_model: Option<PathBuf>,
}

impl ExtractConfig {
    /// No external tools and no hooks: only the built-in converters run.
    pub fn none() -> Self {
        Self {
            path_dirs: Vec::new(),
            hooks: Vec::new(),
            timeout: Duration::from_secs(120),
            media_timeout: Duration::from_mins(20),
            max_output: 16 * 1024 * 1024,
            whisper_model: "base".to_string(),
            whisper_cpp_model: None,
        }
    }

    /// The real environment: `PATH` (plus the usual package-manager and
    /// `LibreOffice` locations) and the config file's hooks. Under `cargo
    /// test` this is [`Self::none`], so tests never touch the machine's
    /// tools or config.
    #[cfg(not(test))]
    pub fn load() -> Self {
        let mut cfg = Self::none();
        if let Some(path) = std::env::var_os("PATH") {
            cfg.path_dirs = std::env::split_paths(&path).collect();
        }
        for extra in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/Applications/LibreOffice.app/Contents/MacOS",
        ] {
            let p = PathBuf::from(extra);
            if !cfg.path_dirs.contains(&p) {
                cfg.path_dirs.push(p);
            }
        }
        cfg.hooks = crate::config::load_extract_converters()
            .into_iter()
            .map(|(key, argv)| Hook { key, argv })
            .collect();
        if let Ok(model) = std::env::var("NEXUS_WHISPER_MODEL")
            && !model.trim().is_empty()
        {
            cfg.whisper_model = model.trim().to_string();
        }
        cfg.whisper_cpp_model = std::env::var_os("NEXUS_WHISPER_CPP_MODEL")
            .map(PathBuf::from)
            .filter(|p| p.is_file());
        cfg
    }

    #[cfg(test)]
    pub fn load() -> Self {
        Self::none()
    }

    /// An executable named `program` (a path with a separator is used as is).
    pub fn find(&self, program: &str) -> Option<PathBuf> {
        let direct = Path::new(program);
        if direct.components().count() > 1 {
            return is_executable(direct).then(|| direct.to_path_buf());
        }
        self.path_dirs
            .iter()
            .map(|d| d.join(program))
            .find(|p| is_executable(p))
    }

    /// The user hook for an extension or MIME type.
    pub fn hook_for(&self, ext: &str, mime: &str) -> Option<&Hook> {
        self.hooks
            .iter()
            .find(|h| (!ext.is_empty() && h.key == ext) || h.key == mime)
    }

    /// Whether scanned PDFs can be OCR'd locally: `pdftoppm` (poppler) renders
    /// the pages and `tesseract` reads them.
    pub fn ocr_tools_installed(&self) -> bool {
        self.find("tesseract").is_some() && self.find("pdftoppm").is_some()
    }

    fn has_any(&self, programs: &[&str]) -> bool {
        programs.iter().any(|p| self.find(p).is_some())
    }
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Why running a program failed.
#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    Missing,
    Timeout,
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "not installed"),
            Self::Timeout => write!(f, "timed out"),
            Self::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// What a finished program printed (capped).
pub struct RunOutput {
    pub stdout: Vec<u8>,
    pub truncated: bool,
}

/// Run `program` with `args` in `cwd`, killing it after `timeout` or once it
/// has printed more than `cap` bytes. Stdin is closed; stderr is reduced to
/// a short message on failure.
pub fn run_capped(
    program: &Path,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    cap: usize,
) -> Result<RunOutput, RunError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::config::scrub_secret_env(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RunError::Missing
        } else {
            RunError::Failed(e.to_string())
        }
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout {
            let _ = s.take(cap as u64 + 1).read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr {
            let _ = (&mut s).take(2048).read_to_end(&mut buf);
            let _ = std::io::copy(&mut s, &mut std::io::sink());
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut over_cap = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => return Err(RunError::Failed(e.to_string())),
        }
        if out_reader.is_finished() {
            // The reader only stops early once the cap is exceeded.
            over_cap = true;
        }
        if over_cap || Instant::now() >= deadline {
            timed_out = !over_cap;
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if timed_out {
        return Err(RunError::Timeout);
    }
    let truncated = stdout.len() > cap;
    stdout.truncate(cap);
    if let Some(status) = status
        && !status.success()
        && !truncated
    {
        let msg = String::from_utf8_lossy(&stderr);
        let first = msg.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(RunError::Failed(if first.is_empty() {
            format!("exited with {status}")
        } else {
            first.trim().chars().take(160).collect()
        }));
    }
    Ok(RunOutput { stdout, truncated })
}

/// A converted file: the text and how it was produced.
#[derive(Debug)]
pub struct Converted {
    pub text: String,
    pub via: String,
}

/// A private scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!("nexus-convert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Which external attempt [`convert`] would make for `kind`, judged from the
/// installed tools and hooks alone (no process is started). `base_empty` says
/// the built-in converter produced nothing.
pub fn plan(kind: Kind, ext: &str, base_empty: bool, cfg: &ExtractConfig) -> Option<&'static str> {
    if cfg.hook_for(ext, kind.mime()).is_some() {
        return Some("hook");
    }
    match kind {
        Kind::Office(l) => {
            if l == "doc" && cfg.find("antiword").is_some() {
                Some("antiword")
            } else if l == "doc" && cfg.find("catdoc").is_some() {
                Some("catdoc")
            } else if cfg.find("soffice").is_some() || cfg.find("libreoffice").is_some() {
                Some("soffice")
            } else {
                None
            }
        }
        Kind::Docx | Kind::Pptx | Kind::Odt | Kind::Ods | Kind::Odp | Kind::Epub | Kind::Rtf
            if base_empty =>
        {
            if cfg.find("pandoc").is_some() && pandoc_format(kind).is_some() {
                Some("pandoc")
            } else if cfg.has_any(&["soffice", "libreoffice"]) {
                Some("soffice")
            } else {
                None
            }
        }
        Kind::Xlsx if base_empty && cfg.has_any(&["soffice", "libreoffice"]) => Some("soffice"),
        Kind::Markup(_) if cfg.find("pandoc").is_some() => Some("pandoc"),
        Kind::Audio(_) | Kind::Video(_) => {
            if cfg.has_any(&["whisper", "whisper-cli"]) && cfg.find("ffmpeg").is_some()
                || cfg.find("whisper").is_some()
            {
                Some("whisper")
            } else if cfg.find("ffprobe").is_some() {
                Some("ffprobe")
            } else {
                None
            }
        }
        _ => None,
    }
}

fn pandoc_format(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::Docx => "docx",
        Kind::Pptx => "pptx",
        Kind::Odt => "odt",
        Kind::Epub => "epub",
        Kind::Rtf => "rtf",
        Kind::Markup(fmt) => fmt,
        _ => return None,
    })
}

/// Run the external converter for `kind` on `path`.
pub fn convert(path: &Path, kind: Kind, cfg: &ExtractConfig) -> Result<Converted, String> {
    let ext = super::detect::extension(&path.to_string_lossy());
    let scratch = Scratch::new()?;
    let tmp = scratch.0.as_path();
    let cap = cfg.max_output;
    let text = |out: RunOutput| String::from_utf8_lossy(&out.stdout).trim().to_string();
    let note = |msg: String| -> String { msg };

    if let Some(hook) = cfg.hook_for(&ext, kind.mime()) {
        let exe = cfg
            .find(&hook.argv[0])
            .ok_or_else(|| format!("converter hook: {} is not installed", hook.argv[0]))?;
        let input = path.to_string_lossy().to_string();
        let mut args: Vec<String> = hook.argv[1..]
            .iter()
            .map(|a| a.replace("{in}", &input))
            .collect();
        if !hook.argv[1..].iter().any(|a| a.contains("{in}")) {
            args.push(input);
        }
        let out = run_capped(&exe, &args, tmp, cfg.timeout, cap)
            .map_err(|e| format!("converter hook {}: {e}", hook.argv[0]))?;
        let t = text(out);
        return if t.is_empty() {
            Err(format!("converter hook {} printed nothing", hook.argv[0]))
        } else {
            Ok(Converted {
                text: t,
                via: "hook".into(),
            })
        };
    }

    match kind {
        Kind::Office(label) => {
            if label == "doc" {
                for (tool, args) in [
                    ("antiword", vec!["-w".to_string(), "0".to_string()]),
                    (
                        "catdoc",
                        vec!["-w".to_string(), "-d".to_string(), "utf-8".to_string()],
                    ),
                ] {
                    if let Some(exe) = cfg.find(tool) {
                        let mut a = args;
                        a.push(path.to_string_lossy().to_string());
                        if let Ok(out) = run_capped(&exe, &a, tmp, cfg.timeout, cap) {
                            let t = text(out);
                            if !t.is_empty() {
                                return Ok(Converted {
                                    text: t,
                                    via: tool.into(),
                                });
                            }
                        }
                    }
                }
            }
            soffice_text(path, tmp, cfg)
        }
        Kind::Docx
        | Kind::Pptx
        | Kind::Odt
        | Kind::Ods
        | Kind::Odp
        | Kind::Epub
        | Kind::Rtf
        | Kind::Xlsx
        | Kind::Markup(_) => {
            if let (Some(fmt), Some(exe)) = (pandoc_format(kind), cfg.find("pandoc")) {
                match pandoc(&exe, fmt, path, tmp, cfg) {
                    Ok(t) if !t.is_empty() => {
                        return Ok(Converted {
                            text: t,
                            via: "pandoc".into(),
                        });
                    }
                    Ok(_) => {}
                    Err(e)
                        if cfg.has_any(&["soffice", "libreoffice"])
                            && !matches!(kind, Kind::Markup(_)) =>
                    {
                        let _ = note(e);
                    }
                    Err(e) => return Err(format!("pandoc: {e}")),
                }
            }
            if matches!(kind, Kind::Markup(_)) {
                return Err("pandoc is not installed".to_string());
            }
            soffice_text(path, tmp, cfg)
        }
        Kind::Audio(_) | Kind::Video(_) => media_text(path, kind, tmp, cfg),
        _ => Err("no converter for this file type".to_string()),
    }
}

fn pandoc(
    exe: &Path,
    fmt: &str,
    input: &Path,
    tmp: &Path,
    cfg: &ExtractConfig,
) -> Result<String, String> {
    let base = |sandbox: bool| {
        let mut a = vec![
            "--from".to_string(),
            fmt.to_string(),
            "--to".to_string(),
            "gfm".to_string(),
            "--wrap=none".to_string(),
        ];
        if sandbox {
            a.push("--sandbox".to_string());
        }
        a.push(input.to_string_lossy().to_string());
        a
    };
    // `--sandbox` keeps pandoc from reading other files; older versions lack it.
    let out = match run_capped(exe, &base(true), tmp, cfg.timeout, cfg.max_output) {
        Err(RunError::Failed(m)) if m.to_lowercase().contains("option") => {
            run_capped(exe, &base(false), tmp, cfg.timeout, cfg.max_output)
        }
        other => other,
    }
    .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn soffice_text(path: &Path, tmp: &Path, cfg: &ExtractConfig) -> Result<Converted, String> {
    let exe = cfg
        .find("soffice")
        .or_else(|| cfg.find("libreoffice"))
        .ok_or_else(|| "LibreOffice (soffice) is not installed".to_string())?;
    let out_dir = tmp.join("out");
    let profile = tmp.join("profile");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let args = vec![
        "--headless".to_string(),
        "--norestore".to_string(),
        format!("-env:UserInstallation=file://{}", profile.display()),
        "--convert-to".to_string(),
        "pdf".to_string(),
        "--outdir".to_string(),
        out_dir.to_string_lossy().to_string(),
        path.to_string_lossy().to_string(),
    ];
    run_capped(&exe, &args, tmp, cfg.timeout, 1 << 20).map_err(|e| format!("soffice: {e}"))?;
    let pdf = std::fs::read_dir(&out_dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "pdf"))
        .ok_or_else(|| "soffice produced no pdf".to_string())?;
    let bytes = std::fs::read(pdf).map_err(|e| e.to_string())?;
    let text = super::pdf_text(&bytes).map_err(|e| format!("soffice pdf: {e}"))?;
    if text.trim().is_empty() {
        return Err("soffice converted the file but found no text".to_string());
    }
    Ok(Converted {
        text,
        via: "soffice → pdf".to_string(),
    })
}

/// Duration `1:02:03` / `3:25` from seconds.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn clock(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `ffprobe` metadata as text.
fn probe_text(exe: &Path, path: &Path, tmp: &Path, cfg: &ExtractConfig) -> Result<String, String> {
    let args = vec![
        "-v".to_string(),
        "error".to_string(),
        "-print_format".to_string(),
        "json".to_string(),
        "-show_format".to_string(),
        "-show_streams".to_string(),
        path.to_string_lossy().to_string(),
    ];
    let out =
        run_capped(exe, &args, tmp, cfg.timeout, 1 << 20).map_err(|e| format!("ffprobe: {e}"))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "ffprobe printed no metadata".to_string())?;
    let s = |p: &str| v.pointer(p).and_then(serde_json::Value::as_str);
    let mut lines = Vec::new();
    if let Some(d) = s("/format/duration").and_then(|d| d.parse::<f64>().ok()) {
        lines.push(format!("duration: {}", clock(d)));
    }
    if let Some(f) = s("/format/format_long_name") {
        lines.push(format!("format: {f}"));
    }
    if let Some(tags) = v
        .pointer("/format/tags")
        .and_then(serde_json::Value::as_object)
    {
        for key in ["title", "artist", "album", "date", "genre", "comment"] {
            if let Some(val) = tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .and_then(|(_, v)| v.as_str())
            {
                lines.push(format!("{key}: {val}"));
            }
        }
    }
    if let Some(streams) = v.get("streams").and_then(serde_json::Value::as_array) {
        for (i, st) in streams.iter().enumerate() {
            let g = |k: &str| st.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
            let n = |k: &str| st.get(k).and_then(serde_json::Value::as_u64);
            let mut line = format!("stream {i}: {} {}", g("codec_type"), g("codec_name"));
            if let (Some(w), Some(h)) = (n("width"), n("height")) {
                let _ = write!(line, " {w}x{h}");
            }
            if let Some(sr) = g("sample_rate").parse::<u32>().ok().filter(|s| *s > 0) {
                let _ = write!(line, " {sr} Hz");
            }
            if let Some(ch) = n("channels") {
                let _ = write!(line, " {ch} ch");
            }
            lines.push(line);
        }
    }
    Ok(lines.join("\n"))
}

fn media_text(
    path: &Path,
    kind: Kind,
    tmp: &Path,
    cfg: &ExtractConfig,
) -> Result<Converted, String> {
    let mut header = format!(
        "media: {} ({})",
        if matches!(kind, Kind::Video(_)) {
            "video"
        } else {
            "audio"
        },
        kind.label()
    );
    if let Some(exe) = cfg.find("ffprobe")
        && let Ok(meta) = probe_text(&exe, path, tmp, cfg)
        && !meta.is_empty()
    {
        header.push('\n');
        header.push_str(&meta);
    }
    let transcript = transcribe(path, tmp, cfg);
    match transcript {
        Ok((t, tool)) if !t.is_empty() => Ok(Converted {
            text: format!("{header}\n\n[transcript]\n{t}"),
            via: tool.to_string(),
        }),
        Ok(_) | Err(None) => {
            if header.contains('\n') {
                Ok(Converted {
                    text: header,
                    via: "ffprobe".to_string(),
                })
            } else {
                Err("no transcriber or ffprobe installed".to_string())
            }
        }
        Err(Some(e)) => {
            if header.contains('\n') {
                Ok(Converted {
                    text: format!("{header}\n(transcription failed: {e})"),
                    via: "ffprobe".to_string(),
                })
            } else {
                Err(e)
            }
        }
    }
}

/// A local transcript: `whisper` (openai CLI) or `whisper-cli` (whisper.cpp,
/// needs `NEXUS_WHISPER_CPP_MODEL`). `Err(None)` = no transcriber installed.
fn transcribe(
    path: &Path,
    tmp: &Path,
    cfg: &ExtractConfig,
) -> Result<(String, &'static str), Option<String>> {
    if let Some(exe) = cfg.find("whisper") {
        let out_dir = tmp.join("whisper");
        std::fs::create_dir_all(&out_dir).map_err(|e| Some(e.to_string()))?;
        let args = vec![
            path.to_string_lossy().to_string(),
            "--model".to_string(),
            cfg.whisper_model.clone(),
            "--output_format".to_string(),
            "txt".to_string(),
            "--output_dir".to_string(),
            out_dir.to_string_lossy().to_string(),
            "--fp16".to_string(),
            "False".to_string(),
        ];
        run_capped(&exe, &args, tmp, cfg.media_timeout, 1 << 20)
            .map_err(|e| Some(format!("whisper: {e}")))?;
        let transcript_file = std::fs::read_dir(&out_dir)
            .map_err(|e| Some(e.to_string()))?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "txt"));
        let text = transcript_file
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        return Ok((text.trim().to_string(), "whisper"));
    }
    if let (Some(exe), Some(model), Some(ffmpeg)) = (
        cfg.find("whisper-cli"),
        cfg.whisper_cpp_model.as_ref(),
        cfg.find("ffmpeg"),
    ) {
        let wav = tmp.join("audio.wav");
        let args = vec![
            "-nostdin".to_string(),
            "-v".to_string(),
            "error".to_string(),
            "-i".to_string(),
            path.to_string_lossy().to_string(),
            "-ar".to_string(),
            "16000".to_string(),
            "-ac".to_string(),
            "1".to_string(),
            "-c:a".to_string(),
            "pcm_s16le".to_string(),
            "-y".to_string(),
            wav.to_string_lossy().to_string(),
        ];
        run_capped(&ffmpeg, &args, tmp, cfg.media_timeout, 1 << 20)
            .map_err(|e| Some(format!("ffmpeg: {e}")))?;
        let args = vec![
            "-m".to_string(),
            model.to_string_lossy().to_string(),
            "-f".to_string(),
            wav.to_string_lossy().to_string(),
            "-nt".to_string(),
            "-np".to_string(),
        ];
        let out = run_capped(&exe, &args, tmp, cfg.media_timeout, cfg.max_output)
            .map_err(|e| Some(format!("whisper-cli: {e}")))?;
        return Ok((
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            "whisper-cli",
        ));
    }
    Err(None)
}

/// Fake tools (shell scripts in a temp dir acting as `PATH`) for tests.
#[cfg(all(test, unix))]
pub(crate) mod fake_tools {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A temp dir holding fake tools (shell scripts) and acting as `PATH`.
    pub(crate) struct Bin {
        pub dir: PathBuf,
    }

    impl Bin {
        pub fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("nexus-bin-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        pub fn tool(&self, name: &str, script: &str) -> &Self {
            let p = self.dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            self
        }

        pub fn cfg(&self) -> ExtractConfig {
            ExtractConfig {
                path_dirs: vec![self.dir.clone()],
                timeout: Duration::from_secs(10),
                media_timeout: Duration::from_secs(10),
                ..ExtractConfig::none()
            }
        }
    }

    impl Drop for Bin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::fake_tools::Bin;
    use super::*;

    #[test]
    fn run_capped_captures_stdout_and_reports_failures() {
        let bin = Bin::new();
        bin.tool("ok", "echo hello")
            .tool("bad", "echo oops >&2; exit 3");
        let cfg = bin.cfg();
        let tmp = std::env::temp_dir();
        let out = run_capped(&cfg.find("ok").unwrap(), &[], &tmp, cfg.timeout, 1024).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
        match run_capped(&cfg.find("bad").unwrap(), &[], &tmp, cfg.timeout, 1024) {
            Err(RunError::Failed(m)) => assert_eq!(m, "oops"),
            other => panic!("expected a failure, got {:?}", other.err()),
        }
        assert_eq!(
            run_capped(Path::new("/definitely/not/here"), &[], &tmp, cfg.timeout, 1).err(),
            Some(RunError::Missing)
        );
    }

    #[test]
    fn run_capped_enforces_timeout_and_output_cap() {
        let bin = Bin::new();
        bin.tool("slow", "sleep 30").tool(
            "flood",
            "while true; do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; done",
        );
        let cfg = bin.cfg();
        let tmp = std::env::temp_dir();
        let started = Instant::now();
        let r = run_capped(
            &cfg.find("slow").unwrap(),
            &[],
            &tmp,
            Duration::from_millis(300),
            1024,
        );
        assert_eq!(r.err(), Some(RunError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(10));
        let r = run_capped(&cfg.find("flood").unwrap(), &[], &tmp, cfg.timeout, 200).unwrap();
        assert!(r.truncated && r.stdout.len() == 200);
    }

    #[test]
    fn find_ignores_non_executables_and_missing_tools() {
        let bin = Bin::new();
        bin.tool("real", "true");
        std::fs::write(bin.dir.join("plain"), "not a program").unwrap();
        let cfg = bin.cfg();
        assert!(cfg.find("real").is_some());
        assert!(cfg.find("plain").is_none());
        assert!(cfg.find("nope").is_none());
    }

    #[test]
    fn hooks_win_and_plan_reports_who_would_run() {
        let bin = Bin::new();
        bin.tool("pandoc", "echo converted");
        let mut cfg = bin.cfg();
        assert_eq!(
            plan(Kind::Markup("rst"), "rst", false, &cfg),
            Some("pandoc")
        );
        assert_eq!(plan(Kind::Docx, "docx", false, &cfg), None);
        assert_eq!(plan(Kind::Docx, "docx", true, &cfg), Some("pandoc"));
        assert_eq!(plan(Kind::Office("doc"), "doc", true, &cfg), None);
        assert_eq!(plan(Kind::Audio("mp3"), "mp3", true, &cfg), None);
        cfg.hooks.push(Hook {
            key: "docx".into(),
            argv: vec!["x".into()],
        });
        assert_eq!(plan(Kind::Docx, "docx", false, &cfg), Some("hook"));
        // A hook keyed by MIME type works for any extension.
        cfg.hooks.push(Hook {
            key: "application/rtf".into(),
            argv: vec!["x".into()],
        });
        assert_eq!(plan(Kind::Rtf, "bin", false, &cfg), Some("hook"));
    }

    #[test]
    fn pandoc_runs_with_argv_not_a_shell() {
        let bin = Bin::new();
        // Echo the arguments so the test can see exactly what was passed.
        bin.tool("pandoc", "echo \"args: $*\"");
        let cfg = bin.cfg();
        let dir = std::env::temp_dir().join(format!("nexus-pandoc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("odd name; rm -rf x.rst");
        std::fs::write(&input, "Title\n=====").unwrap();
        let got = convert(&input, Kind::Markup("rst"), &cfg).unwrap();
        assert_eq!(got.via, "pandoc");
        assert!(
            got.text
                .starts_with("args: --from rst --to gfm --wrap=none --sandbox "),
            "{}",
            got.text
        );
        assert!(got.text.ends_with("odd name; rm -rf x.rst"), "{}", got.text);
        assert!(input.exists(), "the input must not be touched");
    }

    #[test]
    fn user_hook_prints_markdown_and_failures_are_reported() {
        let bin = Bin::new();
        bin.tool("tomd", "echo \"# from hook: $1\"")
            .tool("broken", "exit 2");
        let dir = std::env::temp_dir().join(format!("nexus-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("a.rtf");
        std::fs::write(&input, "{\\rtf1 hi}").unwrap();
        let mut cfg = bin.cfg();
        cfg.hooks.push(Hook {
            key: "rtf".into(),
            argv: vec!["tomd".into(), "{in}".into()],
        });
        let got = convert(&input, Kind::Rtf, &cfg).unwrap();
        assert_eq!(got.via, "hook");
        assert_eq!(got.text, format!("# from hook: {}", input.display()));
        cfg.hooks[0].argv = vec!["broken".into()];
        let err = convert(&input, Kind::Rtf, &cfg).unwrap_err();
        assert!(err.contains("converter hook broken"), "{err}");
        cfg.hooks[0].argv = vec!["not-installed-anywhere".into()];
        let err = convert(&input, Kind::Rtf, &cfg).unwrap_err();
        assert!(err.contains("not installed"), "{err}");
    }

    #[test]
    fn legacy_doc_prefers_antiword_then_catdoc_then_soffice() {
        let bin = Bin::new();
        bin.tool("antiword", "exit 1")
            .tool("catdoc", "echo legacy text");
        let dir = std::env::temp_dir().join(format!("nexus-doc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("old.doc");
        std::fs::write(&input, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]).unwrap();
        let got = convert(&input, Kind::Office("doc"), &bin.cfg()).unwrap();
        assert_eq!(
            (got.text.as_str(), got.via.as_str()),
            ("legacy text", "catdoc")
        );
        let err = convert(&input, Kind::Office("doc"), &ExtractConfig::none()).unwrap_err();
        assert!(err.contains("not installed"), "{err}");
    }

    #[test]
    fn media_reports_ffprobe_metadata_and_a_transcript_when_available() {
        let bin = Bin::new();
        bin.tool(
            "ffprobe",
            r#"echo '{"format":{"duration":"205.4","format_long_name":"MP3","tags":{"TITLE":"Song"}},"streams":[{"codec_type":"audio","codec_name":"mp3","sample_rate":"44100","channels":2}]}'"#,
        );
        let dir = std::env::temp_dir().join(format!("nexus-media-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("a.mp3");
        std::fs::write(&input, b"ID3").unwrap();
        let got = convert(&input, Kind::Audio("mp3"), &bin.cfg()).unwrap();
        assert_eq!(got.via, "ffprobe");
        assert!(
            got.text.contains("duration: 3:25") && got.text.contains("title: Song"),
            "{}",
            got.text
        );
        assert!(
            got.text.contains("stream 0: audio mp3 44100 Hz 2 ch"),
            "{}",
            got.text
        );
        // With a transcriber, the transcript follows the metadata.
        bin.tool("whisper", r#"out=""; while [ $# -gt 0 ]; do [ "$1" = "--output_dir" ] && out="$2"; shift; done; echo "hello transcript" > "$out/a.txt""#);
        let got = convert(&input, Kind::Audio("mp3"), &bin.cfg()).unwrap();
        assert_eq!(got.via, "whisper");
        assert!(
            got.text.contains("[transcript]\nhello transcript"),
            "{}",
            got.text
        );
        // Nothing installed: a clear error, not a panic.
        assert!(convert(&input, Kind::Audio("mp3"), &ExtractConfig::none()).is_err());
    }

    #[test]
    fn soffice_output_is_read_through_the_pdf_path() {
        let bin = Bin::new();
        // Fake soffice: write a one-page PDF into --outdir.
        let pdf = crate::extract::pdf_with_pages(&["LEGACY SHEET"]);
        let pdf_path = bin.dir.join("fixture.pdf");
        std::fs::write(&pdf_path, pdf).unwrap();
        bin.tool(
            "soffice",
            &format!(
                r#"out=""; while [ $# -gt 0 ]; do [ "$1" = "--outdir" ] && out="$2"; shift; done; cp {} "$out/converted.pdf""#,
                pdf_path.display()
            ),
        );
        let dir = std::env::temp_dir().join(format!("nexus-soffice-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("old.xls");
        std::fs::write(&input, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]).unwrap();
        let got = convert(&input, Kind::Office("xls"), &bin.cfg()).unwrap();
        assert_eq!(got.via, "soffice → pdf");
        assert!(got.text.contains("LEGACY SHEET"), "{}", got.text);
    }
}
