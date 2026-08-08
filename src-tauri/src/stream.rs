//! Local HTTP streaming server.
//!
//! The frontend plays `http://127.0.0.1:<port>/stream`. Depending on the source
//! this serves either a plain local file (byte ranges, instant seek) or the
//! growing fragmented-MP4 output of the in-process FFmpeg transcode worker.

use anyhow::{anyhow, Result};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tiny_http::{Header, Response, Server, StatusCode};

use crate::download::{self, DownloadResult};
use crate::probe::MediaInfo;
use crate::transcode::{self, TranscodeStats};

#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    None,
    Direct { path: String },
    Transcode { input: String },
}

#[derive(Debug, serde::Serialize)]
pub struct StreamStatus {
    pub active: bool,
    pub stage: String, // "none" | "downloading" | "direct" | "transcode"
    pub mode: String,  // "none" | "direct" | "transcode"
    pub written_bytes: u64,
    pub total_estimate: u64,
    pub downloaded_bytes: u64,
    pub download_total: u64,
    pub finished: bool,
    pub error: Option<String>,
}

/// Progress state for a running remote download.
#[derive(Default)]
pub struct DownloadState {
    pub active: AtomicBool,
    pub cancel: AtomicBool,
    pub downloaded: AtomicU64,
    pub total: AtomicU64,
    pub error: Mutex<Option<String>>,
}

pub struct StreamManager {
    pub port: u16,
    pub media: Arc<Mutex<Media>>,
    /// the file currently being served by `/stream`
    serve_path: Arc<Mutex<Option<PathBuf>>>,
    stats: TranscodeStats,
    total_estimate: Arc<AtomicU64>,
    active: Arc<AtomicBool>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// remote-download job (active while `stage == "downloading"`)
    download_job: Arc<Mutex<Option<JoinHandle<()>>>>,
    download_state: Arc<DownloadState>,
    /// result of the most recent finished remote download (if any)
    last_media: Arc<Mutex<Option<(String, MediaInfo)>>>,
    _state_lock: Arc<Mutex<()>>,
    _server_thread: JoinHandle<()>,
}

const CHUNK: u64 = 2 * 1024 * 1024;

impl StreamManager {
    pub fn start() -> anyhow::Result<StreamManager> {
        let server = Server::http("127.0.0.1:0")
            .map_err(|e| anyhow!("无法启动本地流媒体服务器: {e}"))?;
        let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);

        let serve_path: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let stats = TranscodeStats::default();
        let total_estimate = Arc::new(AtomicU64::new(0));
        let active = Arc::new(AtomicBool::new(false));

        let socket = Arc::new(server);

        let (sp2, st2) = (serve_path.clone(), stats.clone());

        let server_thread = std::thread::Builder::new()
            .name("stream-server".into())
            .spawn(move || {
                for req in socket.incoming_requests() {
                    let (sp, st) = (sp2.clone(), st2.clone());
                    std::thread::spawn(move || {
                        let _ = handle_request(req, &sp, &st);
                    });
                }
            })
            .map_err(|e| anyhow!("启动流媒体线程失败: {e}"))?;

        Ok(StreamManager {
            port,
            media: Arc::new(Mutex::new(Media::None)),
            serve_path,
            stats,
            total_estimate,
            active,
            worker: Arc::new(Mutex::new(None)),
            download_job: Arc::new(Mutex::new(None)),
            download_state: Arc::new(DownloadState::default()),
            last_media: Arc::new(Mutex::new(None)),
            _state_lock: Arc::new(Mutex::new(())),
            _server_thread: server_thread,
        })
    }

    /// Point the server at `info`'s media source. Starts SDK transcode when needed.
    pub fn set_media(&self, info: &MediaInfo) -> anyhow::Result<String> {
        let _guard = self._state_lock.lock().unwrap();

        self.stop_source();

        self.active.store(true, Ordering::Relaxed);

        if info.directly_playable && info.is_local {
            log::info!("direct serve: {}", info.uri);
            *self.media.lock().unwrap() = Media::Direct { path: info.uri.clone() };
            *self.serve_path.lock().unwrap() = Some(PathBuf::from(&info.uri));
            self.stats.finished.store(true, Ordering::Relaxed);
            self.stats.error.lock().unwrap().take();
            if let Ok(md) = std::fs::metadata(&info.uri) {
                self.stats.written_bytes.store(md.len(), Ordering::Relaxed);
                self.total_estimate.store(md.len(), Ordering::Relaxed);
            }
        } else {
            log::info!("transcode serve: {}", info.uri);
            *self.media.lock().unwrap() = Media::Transcode { input: info.uri.clone() };

            let mut tmp = std::env::temp_dir();
            tmp.push(format!(
                "vplayer_{}_{}.mp4",
                process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            ));

            *self.serve_path.lock().unwrap() = Some(tmp.clone());
            self.stats.finished.store(false, Ordering::Relaxed);
            self.stats.error.lock().unwrap().take();
            self.stats.cancel.store(false, Ordering::Relaxed);
            self.stats.written_bytes.store(0, Ordering::Relaxed);

            // rough output-size estimate: video ~2.2 Mbps + audio 160 kbps
            let dur = info.duration.unwrap_or(0.0);
            let est = (dur * (2_200_000.0 + 160_000.0) / 8.0) as u64;
            self.total_estimate.store(est.max(1), Ordering::Relaxed);

            let stats = self.stats.clone();
            let input = info.uri.clone();
            let out = tmp;
            let handle = std::thread::Builder::new()
                .name("transcode-worker".into())
                .spawn(move || {
                    let _ = transcode::run(&input, &out, &stats);
                })
                .map_err(|e| anyhow!("启动转码线程失败: {e}"))?;
            *self.worker.lock().unwrap() = Some(handle);
        }

        Ok(format!("http://127.0.0.1:{}/stream", self.port))
    }

    fn stop_source(&self) {
        if let Some(handle) = self.worker.lock().unwrap().take() {
            self.stats.cancel.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        if let Some(handle) = self.download_job.lock().unwrap().take() {
            self.download_state.cancel.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        self.download_state.active.store(false, Ordering::Relaxed);
        self.download_state.error.lock().unwrap().take();
        if let Some(p) = self.serve_path.lock().unwrap().take() {
            if matches!(*self.media.lock().unwrap(), Media::Transcode { .. }) {
                let _ = std::fs::remove_file(&p);
            }
        }
        self.active.store(false, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.stop_source();
    }

    pub fn clear_media(&self) {
        self.stop_source();
        *self.media.lock().unwrap() = Media::None;
        self.stats.error.lock().unwrap().take();
    }

    /// Result of the most recently completed remote download: `(stream_url,
    /// info)`. None while a download is still running or after `clear_media`.
    pub fn prepared_result(&self) -> Option<(String, MediaInfo)> {
        let (_local, info) = self.last_media.lock().unwrap().clone()?;
        let url = format!("http://127.0.0.1:{}/stream", self.port);
        Some((url, info))
    }

    /// Download `uri` (remote) into a temp dir in a background thread, then
    /// probe and serve it like any local media. Progress is exposed through
    /// [`StreamStatus`]. Returns the eventual `/stream` URL.
    pub fn start_remote(&self, uri: &str) -> Result<String> {
        let _guard = self._state_lock.lock().unwrap();
        self.stop_source();
        *self.last_media.lock().unwrap() = None;

        let dest_dir = std::env::temp_dir().join(format!(
            "vplayer_src_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dest_dir).map_err(|e| anyhow!("创建下载目录失败: {e}"))?;
        log::info!("download to {}", dest_dir.display());
        self.active.store(true, Ordering::Relaxed);
        self.download_state.active.store(true, Ordering::Relaxed);
        self.download_state.cancel.store(false, Ordering::Relaxed);
        self.download_state.downloaded.store(0, Ordering::Relaxed);
        self.download_state.total.store(0, Ordering::Relaxed);
        self.download_state.error.lock().unwrap().take();
        self.stats.error.lock().unwrap().take();

        let dstate = self.download_state.clone();
        let media = self.media.clone();
        let stats = self.stats.clone();
        let serve_path = self.serve_path.clone();
        let total_estimate = self.total_estimate.clone();
        let worker = self.worker.clone();
        let last_media = self.last_media.clone();
        let active = self.active.clone();
        let uri = uri.to_string();

        let handle = std::thread::Builder::new()
            .name("download-worker".into())
            .spawn(move || {
                // separate Arc clones: one for the report closure, one whose
                // `cancel` field is borrowed by download_remote
                let report_state = dstate.clone();
                let cancel_ref = &dstate.cancel;
                let result = download::download_remote(
                    &uri,
                    &dest_dir,
                    cancel_ref,
                    move |done, total| {
                        report_state.downloaded.store(done, Ordering::Relaxed);
                        if total > 0 {
                            report_state.total.store(total, Ordering::Relaxed);
                        }
                        log::trace!("download {done}/{total}");
                    },
                );
                active.store(false, Ordering::Relaxed);
                match result {
                    Ok(DownloadResult { path, is_hls: _ }) => {
                        let local = path.to_string_lossy().to_string();
                        log::info!("downloaded: {local}");
                        match crate::probe::probe(&local) {
                            Ok(info) => {
                                let ok = set_served_media(
                                    &media,
                                    &serve_path,
                                    &stats,
                                    &total_estimate,
                                    &worker,
                                    &active,
                                    &local,
                                    &info,
                                );
                                if let Err(e) = ok {
                                    *dstate.error.lock().unwrap() =
                                        Some(format!("下载后解析失败: {e}"));
                                    log::error!("probe failure: {e}");
                                } else {
                                    *last_media.lock().unwrap() =
                                        Some((local.clone(), info.clone()));
                                }
                            }
                            Err(e) => {
                                *dstate.error.lock().unwrap() =
                                    Some(format!("下载后解析失败: {e}"));
                                log::error!("probe failure: {e}");
                            }
                        }
                    }
                    Err(e) => {
                       *dstate.error.lock().unwrap() = Some(e.to_string());
                        log::error!("download failure: {e}");
                    }
                }
                dstate.active.store(false, Ordering::Relaxed);
            })
            .map_err(|e| anyhow!("启动下载线程失败: {e}"))?;
        *self.download_job.lock().unwrap() = Some(handle);

        Ok(format!("http://127.0.0.1:{}/stream", self.port))
    }

    pub fn status(&self) -> StreamStatus {
        let downloading = self.download_state.active.load(Ordering::Relaxed);
        let (mode, stage) = if downloading {
            ("download".to_string(), "downloading".to_string())
        } else {
            let mode = match *self.media.lock().unwrap() {
                Media::None => "none",
                Media::Direct { .. } => "direct",
                Media::Transcode { .. } => "transcode",
            }
            .to_string();
            let stage = if self.active.load(Ordering::Relaxed) {
                mode.clone()
            } else {
                "none".to_string()
            };
            (mode, stage)
        };

        let dl_error = self.download_state.error.lock().unwrap().clone();
        let error = dl_error.or_else(|| self.stats.error.lock().unwrap().clone());
        StreamStatus {
            active: self.active.load(Ordering::Relaxed),
            stage,
            mode,
            written_bytes: self.stats.written_bytes.load(Ordering::Relaxed),
            total_estimate: self.total_estimate.load(Ordering::Relaxed),
            downloaded_bytes: self.download_state.downloaded.load(Ordering::Relaxed),
            download_total: self.download_state.total.load(Ordering::Relaxed),
            finished: self.stats.finished.load(Ordering::Relaxed),
            error,
        }
    }
}

/// Wire up a downloaded local file the same way `set_media` wires a local path:
/// serve straight from disk when it's directly playable, otherwise run the
/// SDK transcode worker against it.
fn set_served_media(
    media: &Arc<Mutex<Media>>,
    serve_path: &Arc<Mutex<Option<PathBuf>>>,
    stats: &TranscodeStats,
    total_estimate: &Arc<AtomicU64>,
    worker: &Arc<Mutex<Option<JoinHandle<()>>>>,
    active: &Arc<AtomicBool>,
    local: &str,
    info: &MediaInfo,
) -> Result<()> {
    if info.directly_playable {
        log::info!("serving downloaded file directly: {local}");
        *media.lock().unwrap() = Media::Direct { path: local.to_string() };
        *serve_path.lock().unwrap() = Some(PathBuf::from(local));
        stats.finished.store(true, Ordering::Relaxed);
        stats.error.lock().unwrap().take();
        if let Ok(md) = std::fs::metadata(local) {
            stats.written_bytes.store(md.len(), Ordering::Relaxed);
            total_estimate.store(md.len(), Ordering::Relaxed);
        }
        active.store(true, Ordering::Relaxed);
        return Ok(());
    }

    log::info!("transcoding downloaded file: {local}");
    *media.lock().unwrap() = Media::Transcode { input: local.to_string() };

    let mut tmp = std::env::temp_dir();
    tmp.push(format!(
        "vplayer_{}_{}.mp4",
        process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    *serve_path.lock().unwrap() = Some(tmp.clone());
    stats.finished.store(false, Ordering::Relaxed);
    stats.error.lock().unwrap().take();
    stats.cancel.store(false, Ordering::Relaxed);
    stats.written_bytes.store(0, Ordering::Relaxed);

    let dur = info.duration.unwrap_or(0.0);
    let est = (dur * (2_200_000.0 + 160_000.0) / 8.0) as u64;
    total_estimate.store(est.max(1), Ordering::Relaxed);

    let stats = stats.clone();
    let input = local.to_string();
    let out = tmp;
    let handle = std::thread::Builder::new()
        .name("transcode-worker".into())
        .spawn(move || {
            let _ = transcode::run(&input, &out, &stats);
        })
        .map_err(|e| anyhow!("启动转码线程失败: {e}"))?;
    *worker.lock().unwrap() = Some(handle);
    active.store(true, Ordering::Relaxed);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct RangeSpec {
    start: u64,
    end: Option<u64>,
}

impl std::str::FromStr for RangeSpec {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let v = s.trim();
        let v = v
            .strip_prefix("bytes=")
            .ok_or_else(|| anyhow!("非 bytes 范围"))?;
        let (a, b) = v.split_once('-').ok_or_else(|| anyhow!("缺少 '-'"))?;
        if a.is_empty() {
            // suffix range: treat as from-start (rare in our flows)
            return Ok(RangeSpec {
                start: 0,
                end: None,
            });
        }
        let start = a.parse::<u64>()?;
        let end = if b.is_empty() {
            None
        } else {
            Some(b.parse::<u64>()?)
        };
        Ok(RangeSpec { start, end })
    }
}

/// Read up to `want` bytes starting `start`, blocking (up to `timeout`) until
/// the growing transcode file catches up. Returns (bytes, current_file_size).
fn read_available(
    path: &std::path::Path,
    start: u64,
    want: u64,
    finished: &AtomicBool,
) -> anyhow::Result<(Vec<u8>, u64)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let mut file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        if start < size {
            file.seek(SeekFrom::Start(start))?;
            let mut buf = vec![0u8; (want.min(size - start)) as usize];
            let mut filled = 0usize;
            while filled < buf.len() {
                match file.read(&mut buf[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => return Err(e.into()),
                }
            }
            buf.truncate(filled);
            return Ok((buf, size));
        }
        if finished.load(Ordering::Relaxed) || std::time::Instant::now() > deadline {
            return Err(anyhow!("请求的偏移超出可用数据"));
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
}

fn handle_request(
    req: tiny_http::Request,
    serve_path: &Arc<Mutex<Option<PathBuf>>>,
    stats: &TranscodeStats,
) -> anyhow::Result<()> {
    let is_head = req.method() == &tiny_http::Method::Head;
    if req.method() != &tiny_http::Method::Get && !is_head {
        let _ = req.respond(Response::empty(StatusCode(405)));
        return Ok(());
    }
    let url_path = req.url().split('?').next().unwrap_or("/").to_string();
    if url_path != "/stream" {
        let _ = req.respond(Response::empty(StatusCode(404)));
        return Ok(());
    }

    let path = match *serve_path.lock().unwrap() {
        Some(ref p) => p.clone(),
        None => {
            let _ = req.respond(Response::empty(StatusCode(404)));
            return Ok(());
        }
    };

    let range = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .and_then(|h| h.value.as_str().parse::<RangeSpec>().ok());

    let finished = stats.finished.clone();
    let (data, size, status, cr_header) = match range {
        Some(range) => {
            let mut want = range
                .end
                .map(|e| e.saturating_sub(range.start) + 1)
                .unwrap_or(CHUNK);
            if want > CHUNK * 8 {
                want = CHUNK * 8;
            }
            match read_available(&path, range.start, want, &finished) {
                Ok((data, cur)) => {
                    let content_range = format!(
                        "bytes {}-{}/{}",
                        range.start,
                        range.start + data.len() as u64 - 1,
                        cur
                    );
                    (data, cur, StatusCode(206), Some(content_range))
                }
                Err(_) => {
                    let _ = req.respond(Response::empty(StatusCode(416)));
                    return Ok(());
                }
            }
        }
        None => match read_available(&path, 0, CHUNK, &finished) {
            Ok((data, cur)) => (data, cur, StatusCode(200), None),
            Err(_) => {
                let _ = req.respond(Response::empty(StatusCode(404)));
                return Ok(());
            }
        },
    };

    let mut headers = vec![
        Header::from_bytes(&b"Content-Type"[..], b"video/mp4").unwrap(),
        Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap(),
    ];
    if let Some(cr) = cr_header {
        headers.push(Header::from_bytes(&b"Content-Range"[..], cr.as_bytes()).unwrap());
    }

    if is_head {
        let resp = Response::new(
            status,
            headers,
            std::io::Cursor::new(Vec::<u8>::new()),
            Some(size as usize),
            None,
        );
        let _ = req.respond(resp);
    } else {
        let data_len = data.len();
        let resp = Response::new(status, headers, std::io::Cursor::new(data), Some(data_len), None);
        let _ = req.respond(resp);
    }

    Ok(())
}