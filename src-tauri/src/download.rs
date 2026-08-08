//! Network download layer.
//!
//! The embedded FFmpeg is built without a TLS backend on purpose (it only ever
//! opens local files). All remote I/O (`http://`, `https://`, HLS playlists)
//! happens here with ureq + rustls, and the result is materialised as local
//! files which can then be probed / transcoded by the FFmpeg SDK.

use anyhow::{anyhow, Context as AContext, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Where a remote source landed on disk after downloading.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub path: PathBuf,
    pub is_hls: bool,
}

pub struct Downloader<'a> {
    client: ureq::Agent,
    report: Report,
    cancel: &'a AtomicBool,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(30))
        .redirects(10)
        .user_agent("video-player/0.1")
        .build()
}

/// True when `uri` points at a remote resource our downloader can fetch.
pub fn is_remote(uri: &str) -> bool {
    uri.starts_with("http://") || uri.starts_with("https://")
}

pub fn looks_like_hls(uri: &str) -> bool {
    let lower = uri.to_lowercase();
    lower.ends_with(".m3u8") || lower.ends_with(".m3u")
}

pub fn looks_like_dash(uri: &str) -> bool {
    let lower = uri.to_lowercase();
    lower.ends_with(".mpd") || lower.contains(".mpd?") || lower.contains(".mpd#")
}

/// Fetch `uri` into `dest_dir` and return the local path. Blocks; reports
/// `(downloaded, total)` as it goes.
pub fn download_remote(
    uri: &str,
    dest_dir: &Path,
    cancel: &AtomicBool,
    report: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<DownloadResult> {
    let d = Downloader {
        client: agent(),
        cancel,
        report: Report::new(report),
    };
    d.download(uri, dest_dir)
}

#[derive(Clone)]
pub struct Report(Arc<Box<dyn Fn(u64, u64) + Send + Sync>>);

impl Report {
    pub fn new(f: impl Fn(u64, u64) + Send + Sync + 'static) -> Self {
        Self(Arc::new(Box::new(f)))
    }
    pub fn call(&self, down: u64, total: u64) {
        (self.0)(down, total);
    }
}

impl<'a> Downloader<'a> {
    pub fn new(client: ureq::Agent, cancel: &'a AtomicBool) -> Self {
        Self { client, cancel, report: Report::new(|_, _| {}) }
    }

    pub fn with_report(mut self, r: impl Fn(u64, u64) + Send + Sync + 'static) -> Self {
        self.report = Report::new(r);
        self
    }

    fn report(&self, down: u64, total: u64) {
        self.report.call(down, total);
    }

    /// Download `uri` into `dest_dir`. Returns the local path.
    pub fn download(&self, uri: &str, dest_dir: &Path) -> Result<DownloadResult> {
        std::fs::create_dir_all(dest_dir).context("创建下载目录失败")?;

        if looks_like_hls(uri) {
            let path = self.download_hls(uri, dest_dir)?;
            log::info!("hls materialised at {}", path.display());
            return Ok(DownloadResult { path, is_hls: true });
        }
        if looks_like_dash(uri) {
            let path = self.download_dash(uri, dest_dir)?;
            log::info!("dash materialised at {}", path.display());
            return Ok(DownloadResult { path, is_hls: false });
        }

        let name = file_name_from_uri(uri).unwrap_or_else(|| "download.bin".to_string());
        let dest = dest_dir.join(name);
        self.download_to(uri, &dest)?;
        Ok(DownloadResult { path: dest, is_hls: false })
    }

    // -----------------------------------------------------------------------
    // Plain single-file download
    // -----------------------------------------------------------------------

    /// Returns `Ok(None)` when the server answers 404 (used to find the tail
    /// of a template-based DASH stream).
    fn download_to_optional(&self, uri: &str, dest: &Path) -> Result<Option<u64>> {
        self.check_cancel()?;
        match self.client.get(uri).call() {
            Ok(resp) => self.write_response(resp, dest).map(Some),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(anyhow!("网络请求失败: {e}")),
        }
    }

    fn download_to(&self, uri: &str, dest: &Path) -> Result<u64> {
        self.check_cancel()?;
        let resp = self
            .client
            .get(uri)
            .call()
            .map_err(|e| anyhow!("网络请求失败: {e}"))?;
        self.write_response(resp, dest)
    }

    fn write_response(&self, resp: ureq::Response, dest: &Path) -> Result<u64> {
        let total = resp
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        self.report(0, total);

        let mut reader = resp.into_reader();
        let mut file = std::fs::File::create(dest)?;
        let mut written: u64 = 0;
        let mut buf = [0u8; 64 * 1024];
        loop {
            self.check_cancel()?;
            let n = std::io::Read::read(&mut reader, &mut buf)
                .map_err(|e| anyhow!("读取响应失败: {e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            written += n as u64;
            self.report(written, total);
        }
        file.flush()?;
        self.report(written, total);
        Ok(written)
    }

    fn get_bytes(&self, uri: &str) -> Result<Vec<u8>> {
        self.check_cancel()?;
        let resp = self
            .client
            .get(uri)
            .call()
            .map_err(|e| anyhow!("网络请求失败: {e}"))?;
        let mut buf = Vec::with_capacity(64 * 1024);
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)
            .map_err(|e| anyhow!("读取响应失败: {e}"))?;
        Ok(buf)
    }

    // -----------------------------------------------------------------------
    // HLS materialisation
    // -----------------------------------------------------------------------

    /// Follow the master playlist down to a media playlist, download every
    /// segment into `dest_dir/seg`, and rewrite a local playlist that FFmpeg
    /// can open by absolute path. Returns the rewritten playlist path.
    fn download_hls(&self, master_uri: &str, dest_dir: &Path) -> Result<PathBuf> {
        self.check_cancel()?;

        let media_uri = self.resolve_media_playlist(master_uri)?;
        let media_text = String::from_utf8(self.get_bytes(&media_uri)?)
            .context("解析 m3u8 失败（非文本）")?;

        if media_text.contains("#EXT-X-KEY") {
            anyhow::bail!("暂不支持加密的 HLS 流（EXT-X-KEY）");
        }
        self.check_cancel()?;

        let seg_dir = dest_dir.join("seg");
        std::fs::create_dir_all(&seg_dir).context("创建分段目录失败")?;

        // Parse every literal URI line (segment references).
        let mut segments: Vec<String> = Vec::new();
        for line in media_text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let abs = resolve_url(&media_uri, line)?;
            segments.push(abs);
        }
        if segments.is_empty() {
            anyhow::bail!("playlist 中没有可下载的分段");
        }

        // Download each segment into seg/seg_00001.ts …
        for (i, seg_uri) in segments.iter().enumerate() {
            self.check_cancel()?;
            let fname = format!("seg_{:05}", i + 1);
            let ext = segment_ext(seg_uri)?;
            let local = seg_dir.join(format!("{fname}{ext}"));
            let _ = self.download_to(seg_uri, &local)?;
        }

        // Rewrite the media playlist with absolute local segment paths.
        let mut out = String::with_capacity(media_text.len() + 512);
        let mut seg_index = 0usize;
        for line in media_text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            // A segment URI line: replace with the absolute local copy.
            let fname = format!("seg_{:05}", seg_index + 1);
            let ext = segment_ext(&segments[seg_index])?;
            let local = seg_dir.join(format!("{fname}{ext}"));
            out.push_str(&local.to_string_lossy());
            out.push('\n');
            seg_index += 1;
        }

        let playlist = dest_dir.join("index.m3u8");
        std::fs::write(&playlist, out).context("写入本地播放列表失败")?;
        Ok(playlist)
    }

    /// If `uri` points at a master playlist (contains EXT-X-STREAM-INF), follow
    /// its first variant to the real media playlist. Otherwise return `uri`.
    fn resolve_media_playlist(&self, uri: &str) -> Result<String> {
        let text = String::from_utf8(self.get_bytes(uri)?).context("解析 m3u8 失败")?;

        // Master playlist has variant streams; pick the first declared one.
        let mut variants: Vec<String> = Vec::new();
        let mut next_is_variant = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("#EXT-X-STREAM-INF") {
                next_is_variant = true;
                continue;
            }
            if next_is_variant && !t.is_empty() && !t.starts_with('#') {
                variants.push(resolve_url(uri, t)?);
                next_is_variant = false;
            }
        }
        if variants.is_empty() {
            return Ok(uri.to_string());
        }
        log::info!("master playlist: {} variant(s), using first", variants.len());
        Ok(variants.into_iter().next().unwrap())
    }

    // -----------------------------------------------------------------------
    // MPEG-DASH (DASH-IF `.mpd`) materialisation
    // -----------------------------------------------------------------------

    /// Download a simple DASH manifest: pick a video representation, download
    /// its init segment + all media segments into `dest_dir/seg`, then rewrite
    /// a self-contained local `.mpd` (with `SegmentList` + `Initialization`)
    /// that FFmpeg can open from disk.
    ///
    /// Supported: flat `<SegmentTemplate>` with `$Number$`/`$RepresentationID$`/
    /// `$Bandwidth$` and an optional `$BaseURL$`, or an explicit `<SegmentList>`.
    /// Unsupported (`$Time$`-based or long-format streams, encrypted streams)
    /// produce a clear error instead of a silent failure.
    fn download_dash(&self, mpd_uri: &str, dest_dir: &Path) -> Result<PathBuf> {
        self.check_cancel()?;
        let text = String::from_utf8(self.get_bytes(mpd_uri)?).context("读取 .mpd 失败")?;
        let plan = parse_mpd(&text, mpd_uri).context("解析 .mpd 失败")?;
        if plan.segments.is_empty() {
            anyhow::bail!("未在 .mpd 中找到可下载的分段");
        }

        let seg_dir = dest_dir.join("seg");
        std::fs::create_dir_all(&seg_dir).context("创建分段目录失败")?;

        // Download init segment (if declared).
        if let Some(init) = &plan.init {
            self.check_cancel()?;
            let local_init = seg_dir.join("init.mp4");
            let _ = self.download_to(init, &local_init)?;
        } else {
            std::fs::write(seg_dir.join("init.mp4"), &[][..]).ok();
        }

        // Download every media segment.
        let mut local_segments: Vec<PathBuf> = Vec::new();
        for (i, seg_uri) in plan.segments.iter().enumerate() {
            self.check_cancel()?;
            let fname = format!("seg_{:05}", i + 1);
            let ext = segment_ext(seg_uri)?;
            let local = seg_dir.join(format!("{fname}{ext}"));
            match self.download_to_optional(seg_uri, &local)? {
                Some(_) => local_segments.push(local),
                None => {
                    // template-based stream ended here
                    if plan.ends_on_404 {
                        break;
                    }
                    anyhow::bail!("分段不存在: {seg_uri}");
                }
            }
        }

        // Rewrite a minimal DASH manifest the FFmpeg dash demuxer understands.
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        out.push_str("<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" ");
        out.push_str("type=\"static\" mediaPresentationDuration=\"");
        out.push_str(&plan.duration_iso);
        out.push_str("\" profiles=\"urn:mpeg:dash:profile:isoff-on-demand:2011\">\n");
        out.push_str("  <Period>\n");
        out.push_str("    <AdaptationSet mimeType=\"");
        out.push_str(&plan.mime_type);
        out.push_str("\" contentType=\"");
        out.push_str(&plan.content_type);
        out.push_str("\">\n");
        out.push_str("      <Representation id=\"");
        out.push_str(&plan.rep_id);
        out.push_str("\" bandwidth=\"");
        out.push_str(&plan.bandwidth.to_string());
        out.push_str("\">\n");
        out.push_str("        <Initialization sourceURL=\"init.mp4\"/>");
        out.push('\n');
        out.push_str("        <SegmentList>\n");
        for i in 0..local_segments.len() {
            out.push_str("          <SegmentURL media=\"seg/seg_");
            out.push_str(&format!("{:05}", i + 1));
            out.push_str(&segment_ext0(&plan.segments[i]));
            out.push_str("\"/>\n");
        }
        out.push_str("        </SegmentList>\n");
        out.push_str("      </Representation>\n");
        out.push_str("    </AdaptationSet>\n");
        out.push_str("  </Period>\n");
        out.push_str("</MPD>\n");

        let local_mpd = dest_dir.join("index.mpd");
        std::fs::write(&local_mpd, out).context("写入本地 .mpd 失败")?;
        Ok(local_mpd)
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("下载已取消");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

fn resolve_url(base: &str, rel: &str) -> Result<String> {
    let base_url = url::Url::parse(base).context("无效的基础地址")?;
    let resolved = base_url.join(rel).context("无法解析相对地址")?;
    Ok(resolved.to_string())
}

/// Extension for a downloaded segment, defaulting to `.ts`.
fn segment_ext(uri: &str) -> Result<String> {
    let path = uri.split('?').next().unwrap_or(uri);
    match path.rsplit('.').next() {
        Some(ext) if !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.len() <= 5 => {
            Ok(format!(".{ext}"))
        }
        _ => Ok(".ts".to_string()),
    }
}

/// Stable local extension for a DASH segment (`.m4s` when unknown).
fn segment_ext0(uri: &str) -> String {
    segment_ext(uri).unwrap_or_else(|_| ".m4s".to_string())
}

/// Derive a sane local filename from a remote URL.
fn file_name_from_uri(uri: &str) -> Option<String> {
    let path = uri.split('?').next().unwrap_or(uri);
    let base = path.rsplit('/').next().unwrap_or("").trim();
    if base.is_empty() {
        return None;
    }
    Some(base.to_string())
}

// ---------------------------------------------------------------------------
// MPEG-DASH (DASH-IF `.mpd`) parsing / materialisation
// ---------------------------------------------------------------------------

/// Collects the information we need to actually download a DASH stream:
/// one init segment (optional) plus the full, expanded list of media segments.
struct DashPlan {
    init: Option<String>,
    segments: Vec<String>,
    mime_type: String,
    content_type: String,
    rep_id: String,
    bandwidth: u64,
    duration_iso: String,
    /// template-derived plans probe for the end of the stream (stop at 404)
    ends_on_404: bool,
}

/// A single representation gathered while scanning the manifest.
#[derive(Clone)]
struct Rep {
    id: String,
    bandwidth: u64,
    mime_type: String,
    content_type: String,
    init_template: Option<String>,
    media_template: Option<String>,
    /// explicit segment list (from `<SegmentList><SegmentURL/></SegmentList>`)
    segments: Vec<String>,
    base_url: Option<String>,
    /// true when the media template uses `$Time$` (unsupported)
    has_time_template: bool,
}

impl Default for Rep {
    fn default() -> Self {
        Self {
            id: "rep".into(),
            bandwidth: 0,
            mime_type: "video/mp4".into(),
            content_type: "video".into(),
            init_template: None,
            media_template: None,
            segments: Vec::new(),
            base_url: None,
            has_time_template: false,
        }
    }
}

fn resolve_seg(base: &str, rel: &str) -> Result<String> {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Ok(rel.to_string());
    }
    resolve_url(base, rel)
}

/// Base directory of the manifest (for relative segment URLs).
fn mpd_dir(uri: &str) -> String {
    match uri.rfind('/') {
        Some(idx) => uri[..=idx].to_string(),
        None => uri.to_string(),
    }
}

/// Extract numeric `mediaPresentationDuration` from an ISO-8601 duration like
/// `PT1H2M3.4S`; returns seconds as f64.
fn iso_duration_secs(s: &str) -> f64 {
    let mut secs = 0.0;
    let mut num = String::new();
    let mut mult = 0.0;
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            if !num.is_empty() {
                let v: f64 = num.parse().unwrap_or(0.0);
                match c {
                    'H' => mult = 3600.0,
                    'M' => mult = 60.0,
                    'S' => mult = 1.0,
                    _ => mult = 0.0,
                }
                secs += v * mult;
            }
            num.clear();
        }
    }
    secs
}

/// Parse a `.mpd` manifest into a downloadable plan.
fn parse_mpd(text: &str, mpd_uri: &str) -> Result<DashPlan> {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    let mut media_duration: Option<String> = None;
    let mut adaptation_content_type: Option<String> = None;
    let mut adaptation_mime: Option<String> = None;
    let mut current: Option<Rep> = None;
    let mut reps: Vec<Rep> = Vec::new();

    use quick_xml::events::Event;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                stack.push(tag.clone());
                match tag.as_str() {
                    "MPD" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"mediaPresentationDuration" {
                                media_duration = Some(String::from_utf8_lossy(&a.value).into_owned());
                            }
                        }
                    }
                    "AdaptationSet" => {
                        // flushing happens on Representation end / new adaptation
                        adaptation_content_type = None;
                        adaptation_mime = None;
                        for a in e.attributes().flatten() {
                            match a.key.as_ref() {
                                b"contentType" => {
                                    adaptation_content_type =
                                        Some(String::from_utf8_lossy(&a.value).into_owned())
                                }
                                b"mimeType" => {
                                    adaptation_mime = Some(String::from_utf8_lossy(&a.value).into_owned())
                                }
                                _ => {}
                            }
                        }
                    }
                    "Representation" => {
                        let mut r = Rep::default();
                        for a in e.attributes().flatten() {
                            match a.key.as_ref() {
                                b"id" => r.id = String::from_utf8_lossy(&a.value).into_owned(),
                                b"bandwidth" => {
                                    r.bandwidth = String::from_utf8_lossy(&a.value).parse().unwrap_or(0)
                                }
                                b"mimeType" => r.mime_type = String::from_utf8_lossy(&a.value).into_owned(),
                                b"contentType" => {
                                    r.content_type = String::from_utf8_lossy(&a.value).into_owned()
                                }
                                b"codecs" => {}
                                _ => {}
                            }
                        }
                        if let Some(ct) = &adaptation_content_type {
                            r.content_type = ct.clone();
                        }
                        if let Some(mm) = &adaptation_mime {
                            r.mime_type = mm.clone();
                        }
                        current = Some(r);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match tag.as_str() {
                    "SegmentTemplate" => {
                        if let Some(r) = current.as_mut() {
                            for a in e.attributes().flatten() {
                                match a.key.as_ref() {
                                    b"initialization" => {
                                        r.init_template =
                                            Some(String::from_utf8_lossy(&a.value).into_owned())
                                    }
                                    b"media" => {
                                        let v = String::from_utf8_lossy(&a.value);
                                        if v.contains("$Time$") {
                                            r.has_time_template = true;
                                        }
                                        r.media_template = Some(v.into_owned());
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "SegmentURL" => {
                        if let Some(r) = current.as_mut() {
                            for a in e.attributes().flatten() {
                                if a.key.as_ref() == b"media" {
                                    r.segments.push(String::from_utf8_lossy(&a.value).into_owned());
                                }
                            }
                        }
                    }
                    "BaseURL" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"url" && current.is_some() {
                                current.as_mut().unwrap().base_url =
                                    Some(String::from_utf8_lossy(&a.value).into_owned());
                            }
                        }
                    }
                    "Initialization" => {
                        if let Some(r) = current.as_mut() {
                            for a in e.attributes().flatten() {
                                if a.key.as_ref() == b"sourceURL" {
                                    r.init_template = Some(String::from_utf8_lossy(&a.value).into_owned());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if stack.last().map(String::as_str) == Some("BaseURL") {
                    let s = t.decode()?.to_string();
                    if !s.trim().is_empty() && current.is_some() {
                        current.as_mut().unwrap().base_url = Some(s.trim().to_string());
                    }
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if tag == "Representation" {
                    if let Some(r) = current.take() {
                        reps.push(r);
                    }
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("解析 .mpd 失败: {e}"),
            _ => {}
        }
        buf.clear();
    }
    if let Some(r) = current.take() {
        reps.push(r);
    }

    // Prefer the first usable video representation.
    let rep = reps
        .iter()
        .find(|r| !r.segments.is_empty() || (r.media_template.is_some() && !r.has_time_template))
        .or_else(|| reps.first())
        .cloned()
        .ok_or_else(|| anyhow!("未找到可下载的 DASH 表示"))?;

    if rep.has_time_template {
        anyhow::bail!("暂不支持 $Time$ 时间模板的 DASH 流");
    }

    let base = mpd_dir(mpd_uri);
    let rep_base = rep
        .base_url
        .as_deref()
        .map(|b| relative_to(&base, b))
        .unwrap_or(base);

    let (init, segments, ends_on_404) = if !rep.segments.is_empty() {
        let init = rep
            .init_template
            .as_ref()
            .map(|i| resolve_url(&rep_base, i))
            .transpose()?;
        let segs = rep
            .segments
            .iter()
            .map(|s| resolve_seg(&rep_base, s))
            .collect::<Result<Vec<_>>>()?;
        (init, segs, false)
    } else if let Some(tmpl) = &rep.media_template {
        let init = rep
            .init_template
            .as_ref()
            .map(|i| expand_template(i, &rep, None, &rep_base))
            .transpose()?;
        let segs = segments_from_template(tmpl, &rep, &rep_base)?;
        (init, segs, true)
    } else {
        (None, Vec::new(), false)
    };

    if segments.is_empty() {
        anyhow::bail!("未在 .mpd 中找到可下载的分段");
    }

    Ok(DashPlan {
        init,
        segments,
        mime_type: rep.mime_type,
        content_type: rep.content_type,
        rep_id: rep.id,
        bandwidth: rep.bandwidth,
        duration_iso: media_duration.unwrap_or_else(|| "PT0S".to_string()),
        ends_on_404,
    })
}

fn relative_to(dir: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    format!("{dir}{rel}")
}

/// Expand `$RepresentationID$`, `$Bandwidth$` and `$Number$` in a DASH URL.
fn expand_template(tmpl: &str, rep: &Rep, number: Option<u64>, base: &str) -> Result<String> {
    let mut url = tmpl.to_string();
    url = url.replace("$RepresentationID$", &rep.id);
    url = url.replace("$Bandwidth$", &rep.bandwidth.to_string());
    if let Some(n) = number {
        url = url.replace("$number$", &n.to_string());
        url = url.replace("$Number$", &n.to_string());
    }
    resolve_seg(base, &url)
}

/// Derive the complete segment URL list from a Number-based SegmentTemplate.
///
/// A static MPD template (`seg_$Number%05d$.m4s`) has no explicit count and no
/// SegmentTimeline. We therefore probe from `startNumber=1` upward, stopping at
/// the first missing segment (HTTP 404). To stay bounded and responsive we cap
/// the probe at `DASH_TEMPLATE_CAP` segments.
fn segments_from_template(tmpl: &str, rep: &Rep, base: &str) -> Result<Vec<String>> {
    // Quick validity check without touching the network: the template must be
    // number-based for us to expand it.
    if !tmpl.contains("$Number$") && !tmpl.contains("$number$") {
        anyhow::bail!("暂不支持非数字模板的 DASH 分段（需要 $Number$）");
    }
    let mut urls = Vec::with_capacity(64);
    let start: u64 = 1;
    let cap = DASH_TEMPLATE_CAP as u64;
    for n in start..=start + cap {
        // build URL but do NOT verify existence here; the downloader probes 404
        urls.push(expand_template(tmpl, rep, Some(n), base)?);
    }
    Ok(urls)
}

const DASH_TEMPLATE_CAP: u64 = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_url_absolute_and_relative() {
        let base = "https://example.com/a/b/index.m3u8";
        assert_eq!(
            resolve_url(base, "https://cdn.example.com/x.ts").unwrap(),
            "https://cdn.example.com/x.ts"
        );
        assert_eq!(
            resolve_url(base, "seg/1.ts").unwrap(),
            "https://example.com/a/b/seg/1.ts"
        );
        assert_eq!(
            resolve_url(base, "../other/2.ts").unwrap(),
            "https://example.com/a/other/2.ts"
        );
    }

    #[test]
    fn looks_like_hls_detects_playlists() {
        assert!(looks_like_hls("https://x/y/index.m3u8"));
        assert!(looks_like_hls("https://x/y/index.m3u8?token=abc"));
        assert!(!looks_like_hls("https://x/y/video.mp4"));
    }
}