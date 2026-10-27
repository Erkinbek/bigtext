//! Updates from the project's GitHub releases: finding newer versions and their release notes,
//! downloading a release file (size and SHA-256 checked) and installing it. A portable copy
//! swaps its executable in place; a copy installed with the Windows setup program runs the new
//! installer once BigText has exited. Everything here blocks, so it runs on worker threads.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::jobs::{CancelToken, Progress};

/// GitHub API listing of the releases, newest first.
const RELEASES_API: &str = "https://api.github.com/repos/Erkinbek/bigtext/releases?per_page=30";
pub const RELEASES_PAGE: &str = "https://github.com/Erkinbek/bigtext/releases/latest";

const EXE_NAME: &str = if cfg!(windows) { "bigtext.exe" } else { "bigtext" };
const INSTALLER_ASSET: &str = "bigtext-setup-windows-x64.exe";
/// The release archive with this platform's build (names as CI publishes them).
const ARCHIVE_ASSET: Option<&str> = if cfg!(all(windows, target_arch = "x86_64")) {
    Some("bigtext-windows-x64.zip")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("bigtext-linux-x64.tar.gz")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("bigtext-macos-arm64.tar.gz")
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some("bigtext-macos-x64.tar.gz")
} else {
    None
};

/// A release version (major.minor.patch). Pre-releases are never offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// This build's version; a pre-release build (`0.2.0-beta.1`) counts as its release.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION").split(['-', '+']).next().unwrap_or("")).unwrap_or(Version(0, 0, 0))
    }

    /// Parses `1.2.3` or a tag such as `v1.2.3`; `1.2` means `1.2.0`. Pre-release versions
    /// (`1.2.3-rc.1`) give `None`.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let mut parts = s.split('.').map(|p| p.parse::<u64>().ok());
        let v = Version(parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?);
        parts.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    /// Release notes (Markdown).
    pub notes: String,
    /// The release page on GitHub.
    pub page: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex SHA-256 that GitHub computed on upload, when known.
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// The stable releases newer than `current` in a GitHub "list releases" answer, newest first.
pub fn parse_newer(json: &str, current: Version) -> Result<Vec<Release>, String> {
    let list: Vec<ApiRelease> = serde_json::from_str(json).map_err(|e| format!("Unexpected answer from GitHub: {e}"))?;
    let mut out: Vec<Release> = list
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = Version::parse(&r.tag_name).filter(|v| *v > current)?;
            let assets = r
                .assets
                .into_iter()
                .map(|a| Asset {
                    sha256: a.digest.and_then(|d| d.strip_prefix("sha256:").map(|h| h.to_ascii_lowercase())),
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                })
                .collect();
            Some(Release { version, notes: r.body.unwrap_or_default(), page: r.html_url, assets })
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.version));
    out.dedup_by_key(|r| r.version);
    Ok(out)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(format!("BigText/{} (+https://github.com/Erkinbek/bigtext)", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        // A stalled connection fails instead of hanging; a slow download that moves is fine.
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}

fn net_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(403 | 429) => "GitHub refused the request (rate limit). Try again later.".into(),
        ureq::Error::StatusCode(code) => format!("GitHub answered with HTTP status {code}."),
        e => format!("Could not reach GitHub: {e}"),
    }
}

/// Asks GitHub for the releases newer than `current`, newest first.
pub fn check(current: Version) -> Result<Vec<Release>, String> {
    let mut resp = agent()
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(net_error)?;
    let json = resp.body_mut().read_to_string().map_err(net_error)?;
    parse_newer(&json, current)
}

/// How this copy of BigText updates itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    /// Unpack the release archive and swap the executable `exe` for the new one.
    Replace { asset: Asset, exe: PathBuf },
    /// Installed with the Windows setup program: run the new installer after BigText exits, so
    /// the version in "Installed apps", the shortcuts and the uninstaller stay right.
    Installer { asset: Asset },
    /// Installed with `cargo install`; the user runs it again.
    Cargo,
    /// The release has no build for this platform.
    Manual,
}

/// Path of the running executable; on Unix with symlinks resolved, so the real file is replaced.
pub fn current_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(if cfg!(windows) { exe } else { fs::canonicalize(&exe).unwrap_or(exe) })
}

/// Picks the update method for the executable at `exe` (reads the file system).
pub fn method(release: &Release, exe: &Path) -> Method {
    let dir = exe.parent().unwrap_or(Path::new(""));
    // Inno Setup puts its uninstaller next to the installed program.
    if cfg!(windows) && dir.join("unins000.exe").is_file() {
        return release.asset(INSTALLER_ASSET).map_or(Method::Manual, |a| Method::Installer { asset: a.clone() });
    }
    if is_cargo_bin(dir) {
        return Method::Cargo;
    }
    match ARCHIVE_ASSET.and_then(|name| release.asset(name)) {
        Some(a) => Method::Replace { asset: a.clone(), exe: exe.to_path_buf() },
        None => Method::Manual,
    }
}

fn is_cargo_bin(dir: &Path) -> bool {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| PathBuf::from(h).join(".cargo"));
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from).or(home);
    match (cargo_home.and_then(|h| fs::canonicalize(h.join("bin")).ok()), fs::canonicalize(dir)) {
        (Some(bin), Ok(dir)) => bin == dir,
        _ => false,
    }
}

/// `path` with `suffix` appended to its file name (`bigtext.exe` → `bigtext.exe.old`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Where the Windows installer is downloaded to.
pub fn download_dir() -> PathBuf {
    std::env::temp_dir().join("bigtext-update")
}

/// Downloads `asset` to `dest` and checks its size and SHA-256; on any failure, or when
/// `cancel` fires, nothing is left at `dest`.
pub fn download(asset: &Asset, dest: &Path, progress: &Progress, cancel: &CancelToken) -> Result<(), String> {
    let part = sibling(dest, ".part");
    let dir = dest.parent().unwrap_or(Path::new("."));
    // Created before the request, so an unwritable folder fails without a download.
    let mut file = fs::create_dir_all(dir)
        .and_then(|()| File::create(&part))
        .map_err(|e| format!("BigText cannot write to {}: {e}", dir.display()))?;
    progress.set_total(asset.size);
    progress.set_done(0);
    let result = (|| {
        let resp = agent().get(&asset.url).call().map_err(net_error)?;
        let mut body = resp.into_body().into_reader();
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut got = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err("Cancelled.".to_string());
            }
            let n = body.read(&mut buf).map_err(|e| format!("The download failed: {e}"))?;
            if n == 0 {
                break;
            }
            got += n as u64;
            if got > asset.size {
                return Err("The download is larger than GitHub reported.".into());
            }
            hash.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", part.display()))?;
            progress.set_done(got);
        }
        if got != asset.size {
            return Err(format!("The download stopped early ({got} of {} bytes).", asset.size));
        }
        if let Some(want) = &asset.sha256 {
            if hex(&hash.finalize()) != *want {
                return Err("The downloaded file is damaged (its SHA-256 does not match the release).".into());
            }
        }
        file.sync_all().map_err(|e| format!("{}: {e}", part.display()))
    })();
    drop(file);
    match result.and_then(|()| fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display()))) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads the release archive next to `exe`, unpacks the new executable and swaps it in.
/// The running program keeps working; the new version starts next time.
pub fn update_exe(asset: &Asset, exe: &Path, progress: &Progress, cancel: &CancelToken) -> Result<(), String> {
    let archive = sibling(exe, if asset.name.ends_with(".zip") { ".update.zip" } else { ".update.tar.gz" });
    download(asset, &archive, progress, cancel)?;
    let staged = sibling(exe, ".new");
    let result = extract_exe(&archive, &staged).and_then(|()| install_exe(&staged, exe));
    let _ = fs::remove_file(&archive);
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

/// Writes the BigText executable found in a release archive (`.zip` or `.tar.gz`) to `out`.
pub fn extract_exe(archive: &Path, out: &Path) -> Result<(), String> {
    let bad = |e: &dyn fmt::Display| format!("Could not unpack {}: {e}", archive.display());
    let file = File::open(archive).map_err(|e| bad(&e))?;
    let found = if archive.to_string_lossy().ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
        let mut found = false;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| bad(&e))?;
            if entry.is_file() && entry.name().is_ok_and(|n| n.rsplit(['/', '\\']).next() == Some(EXE_NAME)) {
                write_file(&mut entry, out)?;
                found = true;
                break;
            }
        }
        found
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut found = false;
        for entry in tar.entries().map_err(|e| bad(&e))? {
            let mut entry = entry.map_err(|e| bad(&e))?;
            if entry.header().entry_type().is_file() && entry.path().is_ok_and(|p| p.file_name().is_some_and(|n| n == EXE_NAME)) {
                write_file(&mut entry, out)?;
                found = true;
                break;
            }
        }
        found
    };
    if found {
        Ok(())
    } else {
        Err(format!("The release archive does not contain {EXE_NAME}."))
    }
}

fn write_file(r: &mut impl Read, out: &Path) -> Result<(), String> {
    let err = |e: io::Error| format!("{}: {e}", out.display());
    let mut f = File::create(out).map_err(err)?;
    io::copy(r, &mut f).map_err(err)?;
    f.sync_all().map_err(err)
}

/// Moves the new executable `staged` to `exe`. Windows cannot overwrite a running executable
/// but can rename it, so the old one is moved aside to `<exe>.old` first (removed by `cleanup`).
fn install_exe(staged: &Path, exe: &Path) -> Result<(), String> {
    let err = |e: io::Error| format!("Could not replace {}: {e}", exe.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(exe).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o755);
        fs::set_permissions(staged, fs::Permissions::from_mode(mode)).map_err(err)?;
    }
    if cfg!(windows) {
        let old = sibling(exe, ".old");
        let _ = fs::remove_file(&old);
        fs::rename(exe, &old).map_err(err)?;
        if let Err(e) = fs::rename(staged, exe) {
            let _ = fs::rename(&old, exe);
            return Err(err(e));
        }
        Ok(())
    } else {
        fs::rename(staged, exe).map_err(err)
    }
}

/// Removes what an earlier update left behind: the previous executable (Windows keeps it until
/// that process has exited), half-done downloads and the downloaded installer.
pub fn cleanup(exe: &Path) {
    for suffix in [".old", ".new", ".update.zip", ".update.zip.part", ".update.tar.gz", ".update.tar.gz.part"] {
        let _ = fs::remove_file(sibling(exe, suffix));
    }
    if cfg!(windows) {
        let _ = fs::remove_dir_all(download_dir());
    }
}

/// Starts `exe` as a new process (the updated BigText).
pub fn relaunch(exe: &Path) -> io::Result<()> {
    Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(drop)
}

/// Starts the downloaded Windows installer without its wizard pages (it keeps the previous
/// folder, install mode and tasks). With `relaunch` it opens BigText again when it is done;
/// see the `[Run]` section of installer/bigtext.iss.
pub fn run_installer(setup: &Path, relaunch: bool) -> io::Result<()> {
    let mut cmd = Command::new(setup);
    cmd.args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/NORESTARTAPPLICATIONS"]);
    if relaunch {
        cmd.arg("/relaunch=1");
    }
    cmd.spawn().map(drop)
}

// ---------------------------------------------------------------------------------------------
// Release notes: the Markdown subset GitHub release notes use, parsed for the update dialog.

/// The part of a release's notes for the update dialog: its "What's new" section when it has
/// one (the notes also list the downloads and other text that is the same every release),
/// otherwise all of them.
pub fn whats_new(notes: &str) -> &str {
    let mut start: Option<(usize, usize)> = None;
    let mut pos = 0;
    for line in notes.split_inclusive('\n') {
        if let Some((level, title)) = heading(line.trim()) {
            match start {
                None => {
                    let t = title.to_lowercase().replace('’', "'");
                    if t.starts_with("what's new") || t.starts_with("what's changed") || t == "changes" || t == "changelog" {
                        start = Some((pos + line.len(), level));
                    }
                }
                Some((s, l)) if level <= l => return notes[s..pos].trim(),
                Some(_) => {}
            }
        }
        pos += line.len();
    }
    match start {
        Some((s, _)) => notes[s..].trim(),
        None => notes.trim(),
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    let title = line[level..].strip_prefix(' ')?;
    (1..=6).contains(&level).then(|| (level, title.trim().trim_end_matches('#').trim()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(Vec<Span>),
    /// A list item or table row, with its nesting depth.
    Item(usize, Vec<Span>),
    Paragraph(Vec<Span>),
    Code(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(String),
    Strong(String),
    Code(String),
    /// Label and URL.
    Link(String, String),
}

/// Parses release notes into blocks. Covers what release notes use: headings, lists, tables
/// (one row per item, the header row dropped), code blocks, **bold**, `code` and links;
/// images are left out.
pub fn parse_notes(md: &str) -> Vec<Block> {
    enum Raw {
        Heading(String),
        Item(usize, String),
        Para(String),
        Code(String),
    }
    let mut raw: Vec<Raw> = Vec::new();
    let mut code: Option<String> = None;
    // A line right after a list item or paragraph line continues it.
    let mut open = false;
    let mut table_row = false;
    for line in md.lines() {
        if let Some(c) = &mut code {
            if line.trim_start().starts_with("```") {
                raw.push(Raw::Code(std::mem::take(c)));
                code = None;
            } else {
                c.push_str(line);
                c.push('\n');
            }
            continue;
        }
        let t = line.trim();
        let depth = (line.len() - line.trim_start().len()) / 2;
        let was_table_row = std::mem::replace(&mut table_row, false);
        if t.is_empty() || matches!(t, "---" | "***" | "___") {
            open = false;
        } else if t.starts_with("```") {
            code = Some(String::new());
            open = false;
        } else if let Some((_, title)) = heading(t) {
            raw.push(Raw::Heading(title.to_string()));
            open = false;
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
            raw.push(Raw::Item(depth, rest.trim().to_string()));
            open = true;
        } else if t.split_once(". ").is_some_and(|(n, _)| (1..=3).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())) {
            // Numbered items keep their number.
            raw.push(Raw::Item(depth, t.to_string()));
            open = true;
        } else if t.starts_with('|') {
            let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
            if cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))) {
                // The separator under the header row: the row before it was the header.
                if was_table_row {
                    raw.pop();
                }
            } else {
                raw.push(Raw::Item(0, cells.join(" · ")));
                table_row = true;
            }
            open = false;
        } else {
            let t = t.trim_start_matches('>').trim();
            match raw.last_mut() {
                Some(Raw::Item(_, s) | Raw::Para(s)) if open => {
                    s.push(' ');
                    s.push_str(t);
                }
                _ => raw.push(Raw::Para(t.to_string())),
            }
            open = true;
        }
    }
    if let Some(c) = code {
        raw.push(Raw::Code(c));
    }
    raw.into_iter()
        .filter_map(|r| match r {
            Raw::Heading(s) => Some(Block::Heading(inline(&s))),
            Raw::Item(d, s) => Some(Block::Item(d, inline(&s))),
            // A paragraph that was only an image has nothing left.
            Raw::Para(s) => Some(inline(&s)).filter(|v| !v.is_empty()).map(Block::Paragraph),
            Raw::Code(s) => Some(Block::Code(s)),
        })
        .collect()
}

/// Inline Markdown: `**bold**`, `` `code` ``, `[label](url)`, `<url>` and bare URLs.
fn inline(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut strong = false;
    let flush = |text: &mut String, out: &mut Vec<Span>, strong: bool| {
        if !text.is_empty() {
            let t = std::mem::take(text);
            out.push(if strong { Span::Strong(t) } else { Span::Text(t) });
        }
    };
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(c) = rest.strip_prefix('\\').and_then(|r| r.chars().next()).filter(char::is_ascii_punctuation) {
            text.push(c);
            i += 2;
        } else if rest.starts_with("**") {
            flush(&mut text, &mut out, strong);
            strong = !strong;
            i += 2;
        } else if let Some(end) = rest.strip_prefix('`').and_then(|r| r.find('`')) {
            flush(&mut text, &mut out, strong);
            out.push(Span::Code(rest[1..1 + end].to_string()));
            i += end + 2;
        } else if let Some((_, _, len)) = rest.strip_prefix('!').and_then(link_at) {
            i += 1 + len;
        } else if let Some((label, url, len)) = link_at(rest) {
            flush(&mut text, &mut out, strong);
            out.push(Span::Link(label.replace("**", "").replace('`', ""), url.to_string()));
            i += len;
        } else if let Some(end) = rest.strip_prefix('<').filter(|r| r.starts_with("https://") || r.starts_with("http://")).and_then(|r| r.find('>')) {
            flush(&mut text, &mut out, strong);
            let url = &rest[1..1 + end];
            out.push(Span::Link(url.to_string(), url.to_string()));
            i += end + 2;
        } else if rest.starts_with("https://") || rest.starts_with("http://") {
            let end = rest.find(|c: char| c.is_whitespace() || matches!(c, ')' | '>' | '<')).unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            flush(&mut text, &mut out, strong);
            out.push(Span::Link(url.to_string(), url.to_string()));
            i += url.len();
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            text.push(c);
            i += c.len_utf8();
        }
    }
    flush(&mut text, &mut out, strong);
    out
}

/// A `[label](url)` link at the start of `s`: its label, URL and length.
fn link_at(s: &str) -> Option<(&str, &str, usize)> {
    let close = s.strip_prefix('[')?.find(']')? + 1;
    let url_len = s[close..].strip_prefix("](")?.find(')')?;
    let url = s[close + 2..close + 2 + url_len].split_whitespace().next()?;
    Some((&s[1..close], url, close + 2 + url_len + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(Version::parse("v0.1.2"), Some(Version(0, 1, 2)));
        assert_eq!(Version::parse("1.10"), Some(Version(1, 10, 0)));
        assert_eq!(Version::parse(" 2 "), Some(Version(2, 0, 0)));
        assert_eq!(Version::parse("1.2.3-rc.1"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("nightly"), None);
        assert_eq!(Version::parse(""), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
        assert!(env!("CARGO_PKG_VERSION").starts_with(&Version::current().to_string()));
    }

    const RELEASES: &str = r###"[
        {"tag_name":"v0.3.0-rc.1","html_url":"https://x/rc","prerelease":true,"assets":[]},
        {"tag_name":"v0.2.1","html_url":"https://x/0.2.1","draft":true,"assets":[]},
        {"tag_name":"v0.1.10","html_url":"https://x/0.1.10","body":"## What's new in 0.1.10\n- B","assets":[
            {"name":"bigtext-windows-x64.zip","browser_download_url":"https://x/a.zip","size":10,"digest":"sha256:ABCD"},
            {"name":"bigtext-setup-windows-x64.exe","browser_download_url":"https://x/s.exe","size":20,"digest":null}]},
        {"tag_name":"v0.1.2","html_url":"https://x/0.1.2","body":null,"assets":[]},
        {"tag_name":"v0.1.1","html_url":"https://x/0.1.1","body":"old","assets":[]},
        {"tag_name":"latest","html_url":"https://x/latest","assets":[]}
    ]"###;

    #[test]
    fn newer_releases() {
        let r = parse_newer(RELEASES, Version(0, 1, 1)).unwrap();
        let v: Vec<Version> = r.iter().map(|r| r.version).collect();
        assert_eq!(v, [Version(0, 1, 10), Version(0, 1, 2)]);
        assert_eq!(r[0].notes, "## What's new in 0.1.10\n- B");
        assert_eq!(r[1].notes, "");
        let zip = r[0].asset("bigtext-windows-x64.zip").unwrap();
        assert_eq!((zip.url.as_str(), zip.size, zip.sha256.as_deref()), ("https://x/a.zip", 10, Some("abcd")));
        assert_eq!(r[0].asset("bigtext-setup-windows-x64.exe").unwrap().sha256, None);
        assert!(parse_newer(RELEASES, Version(0, 1, 10)).unwrap().is_empty());
        assert!(parse_newer("{\"message\":\"Not Found\"}", Version(0, 1, 1)).is_err());
    }

    fn release(assets: &[&str]) -> Release {
        let assets = assets.iter().map(|n| Asset { name: n.to_string(), url: String::new(), size: 1, sha256: None }).collect();
        Release { version: Version(9, 0, 0), notes: String::new(), page: String::new(), assets }
    }

    #[test]
    fn update_method() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(EXE_NAME);
        let all = release(&["bigtext-setup-windows-x64.exe", "bigtext-windows-x64.zip", "bigtext-linux-x64.tar.gz", "bigtext-macos-arm64.tar.gz", "bigtext-macos-x64.tar.gz"]);
        match (method(&all, &exe), ARCHIVE_ASSET) {
            (Method::Replace { asset, exe: e }, Some(name)) => assert_eq!((asset.name.as_str(), e), (name, exe.clone())),
            (Method::Manual, None) => {}
            (m, _) => panic!("unexpected {m:?}"),
        }
        assert_eq!(method(&release(&[]), &exe), Method::Manual);
        if cfg!(windows) {
            fs::write(dir.path().join("unins000.exe"), b"").unwrap();
            assert!(matches!(method(&all, &exe), Method::Installer { asset } if asset.name == INSTALLER_ASSET));
            assert_eq!(method(&release(&["bigtext-windows-x64.zip"]), &exe), Method::Manual);
        }
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn tar_gz_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let bench = format!("bigtext-x/bigtext-bench{}", if cfg!(windows) { ".exe" } else { "" });
        let main = format!("bigtext-x/{EXE_NAME}");
        let entries: [(&str, &[u8]); 3] = [("bigtext-x/README.md", b"readme"), (&bench, b"bench"), (&main, b"the new exe")];
        for (name, data) in [("a.zip", zip_with(&entries)), ("a.tar.gz", tar_gz_with(&entries))] {
            let archive = dir.path().join(name);
            fs::write(&archive, data).unwrap();
            extract_exe(&archive, &out).unwrap();
            assert_eq!(fs::read(&out).unwrap(), b"the new exe", "{name}");
        }
        let archive = dir.path().join("b.zip");
        fs::write(&archive, zip_with(&entries[..2])).unwrap();
        assert!(extract_exe(&archive, &out).unwrap_err().contains("does not contain"));
        fs::write(&archive, b"not a zip").unwrap();
        assert!(extract_exe(&archive, &out).unwrap_err().contains("Could not unpack"));
    }

    #[test]
    fn swaps_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(EXE_NAME);
        let staged = sibling(&exe, ".new");
        fs::write(&exe, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        install_exe(&staged, &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert!(!staged.exists());
        assert_eq!(fs::read(sibling(&exe, ".old")).is_ok(), cfg!(windows));
        cleanup(&exe);
        assert!(!sibling(&exe, ".old").exists() && exe.exists());
    }

    #[test]
    fn whats_new_section() {
        let notes = "Intro\n\n## What’s new in 0.2.0\n\n- A\n### Detail\n- B\n\n## Which file do I need?\n| x |\n";
        assert_eq!(whats_new(notes), "- A\n### Detail\n- B");
        assert_eq!(whats_new("## What's Changed\n* C\n"), "* C");
        assert_eq!(whats_new("Just text\n"), "Just text");
    }

    #[test]
    fn notes_markdown() {
        let md = "## Fixes\n\n- **Icon** in `bigtext.exe`, see [the docs](https://x/d \"t\").\n  continued\n  - nested\n\
                  1. first\n\nSome text\nmore text <https://a/b>.\n\n![shot](https://x/i.png)\n\n\
                  | Platform | File |\n| --- | :---: |\n| Linux | [`a.tar.gz`](https://x/a) |\n\n```\ncode\n```\nhttps://x/y.";
        let t = |s: &str| Span::Text(s.into());
        assert_eq!(
            parse_notes(md),
            vec![
                Block::Heading(vec![t("Fixes")]),
                Block::Item(0, vec![Span::Strong("Icon".into()), t(" in "), Span::Code("bigtext.exe".into()), t(", see "), Span::Link("the docs".into(), "https://x/d".into()), t(". continued")]),
                Block::Item(1, vec![t("nested")]),
                Block::Item(0, vec![t("1. first")]),
                Block::Paragraph(vec![t("Some text more text "), Span::Link("https://a/b".into(), "https://a/b".into()), t(".")]),
                Block::Item(0, vec![t("Linux · "), Span::Link("a.tar.gz".into(), "https://x/a".into())]),
                Block::Code("code\n".into()),
                Block::Paragraph(vec![Span::Link("https://x/y".into(), "https://x/y".into()), t(".")]),
            ]
        );
    }

    /// Downloads the real latest release for this platform and unpacks it. Needs the network:
    /// `cargo test --lib update -- --ignored`.
    #[test]
    #[ignore]
    fn live_download() {
        let releases = check(Version(0, 0, 0)).unwrap();
        let latest = &releases[0];
        println!("latest {} with {} assets", latest.version, latest.assets.len());
        let asset = latest.asset(ARCHIVE_ASSET.unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(EXE_NAME);
        fs::write(&exe, b"old").unwrap();
        update_exe(asset, &exe, &Progress::default(), &CancelToken::new()).unwrap();
        assert!(fs::metadata(&exe).unwrap().len() > 1 << 20);
        assert!(!sibling(&exe, ".update.zip").exists() && !sibling(&exe, ".update.tar.gz").exists());
    }
}
