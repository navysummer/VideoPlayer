#![allow(linker_messages)]

mod probe;
mod stream;
mod transcode;

/// Exported only for the `hls_diag` diagnostic binary.
#[doc(hidden)]
pub fn probe(uri: &str) -> anyhow::Result<probe::MediaInfo> {
    probe::probe(uri)
}

/// Exported only for the `hls_diag` diagnostic binary.
#[doc(hidden)]
pub fn transcode_run(
    input: &str,
    output: &std::path::Path,
    stats: &transcode::TranscodeStats,
) -> anyhow::Result<()> {
    transcode::run(input, output, stats)
}

/// Exported only for the `hls_diag` diagnostic binary.
#[doc(hidden)]
pub use transcode::TranscodeStats;

use probe::MediaInfo;
use serde::Serialize;
use stream::{StreamManager, StreamStatus};
use tauri::{Manager, RunEvent, State};

pub struct AppState {
    pub stream: StreamManager,
}

#[derive(Serialize)]
pub struct OpenMediaResult {
    pub url: String,
    pub info: MediaInfo,
}

/// Open the native "choose file" dialog and return the picked path, if any.
#[tauri::command]
async fn open_file_dialog() -> Result<Option<String>, String> {
    #[cfg(feature = "file-dialog")]
    {
        tauri::async_runtime::spawn_blocking(|| {
            let picked = rfd::FileDialog::new()
                .set_title("打开视频文件")
                .add_filter(
                    "所有视频",
                    &[
                        "mp4", "mkv", "mov", "avi", "flv", "wmv", "webm", "m4v", "mpg", "mpeg",
                        "ts", "m2ts", "mts", "3gp", "3g2", "ogv", "rm", "rmvb", "rmv", "asf",
                        "mp4v", "m4a", "aac", "mp3", "flac", "wav", "mka", "ogg", "opus", "amr",
                    ],
                )
                .add_filter("所有文件", &["*"])
                .pick_file();
            Ok(picked.map(|p| p.to_string_lossy().to_string()))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "file-dialog"))]
    {
        Err("当前平台不支持文件选择对话框".into())
    }
}

/// True for remote URLs whose container is directly playable by the webview
/// (HLS on WebKit, plus plain mp4/webm/audio). The browser streams these in
/// real time — no download, no transcode.
///
/// HTTPS sources are handed straight to the webview. HTTP sources are relayed
/// through our local HTTP server (macOS ATS blocks cleartext media loads in
/// WKWebView, so we proxy them ourselves).
fn native_playable_remote(uri: &str) -> bool {
    if !(uri.starts_with("http://") || uri.starts_with("https://")) {
        return false;
    }
    let path = uri.split(['?', '#']).next().unwrap_or(uri).to_lowercase();
    for ext in [
        ".m3u8", ".m3u", ".mp4", ".m4v", ".m4a", ".mov", ".webm", ".aac", ".mp3", ".wav",
        ".ogg", ".oga", ".opus", ".flac", ".mp2",
    ] {
        if path.ends_with(ext) {
            return true;
        }
    }
    false
}

/// Quick playlist sniff: does this look like an HLS or DASH manifest URL?
fn looks_like_manifest(uri: &str) -> bool {
    let lower = uri.to_lowercase();
    lower.ends_with(".m3u8") || lower.ends_with(".m3u") || lower.ends_with(".mpd")
}

/// Probe a media source (local path or URL) and start streaming it.
/// Returns the stream URL + media info.
#[tauri::command]
async fn open_media(uri: String, state: State<'_, AppState>) -> Result<OpenMediaResult, String> {
    let uri = uri.trim().to_string();
    if uri.is_empty() {
        return Err("请输入有效的地址或路径".into());
    }
    if !is_playable_uri(&uri) {
        return Err("暂不支持该协议/地址".into());
    }

    // Remote sources the webview can play natively — hand straight to <video>.
    // The browser handles HTTP/HTTPS Range requests on its own.
    if native_playable_remote(&uri) {
        let mut info = MediaInfo::default();
        info.uri = uri.clone();
        info.is_local = false;
        info.directly_playable = true;
        info.compatible_reason = "浏览器原生播放".into();
        if looks_like_manifest(&uri) {
            info.format_name = Some(if uri.to_lowercase().ends_with(".mpd") {
                "dash".into()
            } else {
                "hls".into()
            });
        }
        return Ok(OpenMediaResult { url: uri, info });
    }

    // Non-playable remote formats go through FFmpeg copy/transcode via local server.
    let info = tauri::async_runtime::spawn_blocking({
        let uri = uri.clone();
        move || probe::probe(&uri)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    if info.videos.is_empty() && info.audios.is_empty() {
        return Err("未识别到可播放的音视频流".into());
    }

    let url = state.stream.set_media(&info).map_err(|e| e.to_string())?;
    Ok(OpenMediaResult { url, info })
}

#[tauri::command]
fn stream_status(state: State<'_, AppState>) -> StreamStatus {
    state.stream.status()
}

#[tauri::command]
async fn stop_playback(state: State<'_, AppState>) -> Result<(), String> {
    state.stream.stop();
    state.stream.clear_media();
    Ok(())
}

/// Open an external URL in the default browser (used by the About dialog).
#[tauri::command]
async fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    app.opener()
        .open_url(url, None::<String>)
        .map_err(|e| format!("无法打开链接: {e}"))?;
    Ok(())
}

fn is_playable_uri(uri: &str) -> bool {
    if std::path::Path::new(uri).exists() {
        return true;
    }
    let lower = uri.to_lowercase();
    const SCHEMES: &[&str] = &[
        "http://", "https://", "rtmp://", "rtsp://", "rtmps://", "mms://", "mmsh://",
        "ftp://", "file://", "srt://", "udp://", "tcp://", "blob:", "magnet:",
    ];
    SCHEMES.iter().any(|s| lower.starts_with(s) || uri.starts_with(s))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let stream = StreamManager::start().expect("无法启动流媒体服务");
    let state = AppState { stream };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            open_file_dialog,
            open_media,
            stream_status,
            stop_playback,
            open_external,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let RunEvent::ExitRequested { .. } = event {
            if let Some(state) = app_handle.try_state::<AppState>() {
                state.stream.stop();
            }
        }
    });
}