#![allow(linker_messages)]

mod download;
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
    /// true when a remote source is being downloaded in the background; the
    /// final result is obtained with `open_media_result`.
    pub loading: bool,
}

/// Open the native "choose file" dialog and return the picked path, if any.
#[tauri::command]
async fn open_file_dialog() -> Result<Option<String>, String> {
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

/// Probe a media source (local path or URL) and start streaming it.
/// Returns the local stream URL + media info.
#[tauri::command]
async fn open_media(uri: String, state: State<'_, AppState>) -> Result<OpenMediaResult, String> {
    let uri = uri.trim().to_string();
    if uri.is_empty() {
        return Err("请输入有效的地址或路径".into());
    }
    if !is_playable_uri(&uri) {
        return Err("暂不支持该协议/地址".into());
    }

    // Remote sources are downloaded via ureq+rustls in a background thread;
    // the FFmpeg SDK never touches the network directly. Return immediately
    // with `loading = true`; the frontend polls `stream_status` and then
    // calls `open_media_result` once the download stage has finished.
    if download::is_remote(&uri) {
        let url = state
            .stream
            .start_remote(&uri)
            .map_err(|e| e.to_string())?;
        return Ok(OpenMediaResult {
            url,
            info: MediaInfo::default(),
            loading: true,
        });
    }

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
    Ok(OpenMediaResult {
        url,
        info,
        loading: false,
    })
}

/// Return the completed media result for a remote download that has finished
/// staging. Errors if the download failed or a remote source is still running.
#[tauri::command]
async fn open_media_result(state: State<'_, AppState>) -> Result<Option<OpenMediaResult>, String> {
    let prepared = state.stream.prepared_result();
    Ok(prepared.map(|(url, info)| OpenMediaResult {
        url,
        info,
        loading: false,
    }))
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
            open_media_result,
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