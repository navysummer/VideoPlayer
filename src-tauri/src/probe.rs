//! Media probing done entirely through the FFmpeg SDK (no external CLI).

use anyhow::Context as AContext;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct StreamInfo {
    pub index: usize,
    pub codec_type: String,
    pub codec_name: String,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub sample_rate: Option<u64>,
    pub channels: Option<u64>,
    pub bit_rate: Option<String>,
}

impl StreamInfo {
    pub fn is_video(&self) -> bool {
        self.codec_type == "video"
    }
    pub fn is_audio(&self) -> bool {
        self.codec_type == "audio"
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MediaInfo {
    pub uri: String,
    pub is_local: bool,
    pub file_size: Option<u64>,
    pub duration: Option<f64>,
    pub format_name: Option<String>,
    pub streams: Vec<StreamInfo>,
    pub videos: Vec<StreamInfo>,
    pub audios: Vec<StreamInfo>,
    /// is the media directly playable by the browser `video` element
    pub directly_playable: bool,
    pub compatible_reason: String,
}

fn codec_name_of(id: ffmpeg::codec::Id) -> String {
    format!("{:?}", id)
        .trim_start_matches("CodecId::")
        .to_lowercase()
}

/// Probe media at `uri` (local path or network URL) using the FFmpeg SDK.
pub fn probe(uri: &str) -> anyhow::Result<MediaInfo> {
    let is_local = !(uri.starts_with("http://")
        || uri.starts_with("https://")
        || uri.starts_with("rtmp://")
        || uri.starts_with("rtsp://")
        || uri.starts_with("rtmps://")
        || uri.starts_with("mms://")
        || uri.starts_with("mmsh://")
        || uri.starts_with("srt://")
        || uri.starts_with("ftp://")
        || uri.starts_with("udp://")
        || uri.starts_with("tcp://")
        || uri.starts_with("blob:")
        || uri.starts_with("magnet:"));

    let mut info = MediaInfo {
        uri: uri.to_string(),
        is_local,
        file_size: None,
        duration: None,
        format_name: None,
        streams: Vec::new(),
        videos: Vec::new(),
        audios: Vec::new(),
        directly_playable: false,
        compatible_reason: String::new(),
    };

    let ictx = ffmpeg::format::input(uri).with_context(|| format!("无法打开媒体：{uri}"))?;

    let duration_us = ictx.duration();
    info.duration = (duration_us > 0).then(|| duration_us as f64 / 1_000_000.0);

    let fmt = ictx.format();
    info.format_name = Some(fmt.name().trim_end_matches('\0').to_string());

    if is_local {
        if let Ok(md) = std::fs::metadata(uri) {
            info.file_size = Some(md.len());
        }
    }

    for stream in ictx.streams() {
        let params = stream.parameters();
        let medium = params.medium();

        let mut st = StreamInfo {
            index: stream.index(),
            codec_type: match medium {
                ffmpeg::media::Type::Video => "video".into(),
                ffmpeg::media::Type::Audio => "audio".into(),
                _ => "other".into(),
            },
            codec_name: codec_name_of(params.id()),
            profile: profile_of(&params),
            width: None,
            height: None,
            fps: None,
            sample_rate: None,
            channels: None,
            bit_rate: None,
        };

        let raw = unsafe { params.as_ptr() };
        unsafe {
            let raw_ref = &*raw;
            if medium == ffmpeg::media::Type::Video {
                st.width = (raw_ref.width > 0).then_some(raw_ref.width as u32);
                st.height = (raw_ref.height > 0).then_some(raw_ref.height as u32);
            }
            if medium == ffmpeg::media::Type::Audio {
                st.sample_rate = (raw_ref.sample_rate > 0).then_some(raw_ref.sample_rate as u64);
                st.channels = (raw_ref.ch_layout.nb_channels > 0)
                    .then_some(raw_ref.ch_layout.nb_channels as u64);
            }
            if raw_ref.bit_rate > 0 {
                st.bit_rate = Some(raw_ref.bit_rate.to_string());
            }
        }

        // average frame rate from the stream
        let afr = stream.avg_frame_rate();
        let (num, den) = (afr.numerator(), afr.denominator());
        if num > 0 && den > 0 {
            st.fps = Some(num as f64 / den as f64);
        }

        info.streams.push(st.clone());
        if st.is_video() {
            info.videos.push(st);
        } else if st.is_audio() {
            info.audios.push(st);
        }
    }

    info.directly_playable = is_directly_playable(&mut info);
    Ok(info)
}

fn profile_of(params: &ffmpeg::codec::Parameters) -> Option<String> {
    unsafe {
        let codec_id = (*params.as_ptr()).codec_id;
        let profile_id = (*params.as_ptr()).profile;
        let name = ffmpeg::ffi::avcodec_profile_name(codec_id, profile_id);
        if name.is_null() {
            None
        } else {
            Some(std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned())
        }
    }
}

/// Browsers can natively play a restricted set of containers/codecs.
/// This check is only an optimisation: when true we stream the file straight
/// from disk (no transcode). Everything else goes through FFmpeg's SDK.
fn is_directly_playable(info: &mut MediaInfo) -> bool {
    let container = info.format_name.as_deref().unwrap_or("").to_lowercase();

    let vcodec = match info.videos.first() {
        Some(v) => v.codec_name.to_lowercase(),
        None => String::new(),
    };
    let acodec = match info.audios.first() {
        Some(a) => a.codec_name.to_lowercase(),
        None => String::new(),
    };

    let container_mp4 = container.contains("mov,mp4,m4a,3gp")
        || container.starts_with("mov,")
        || container.starts_with("mp4")
        || container.starts_with("3gp");
    let container_webm = container.contains("webm");

    let mp4_video_ok = matches!(vcodec.as_str(), "h264" | "hevc" | "" | "none");
    let webm_video_ok = matches!(vcodec.as_str(), "vp8" | "vp9" | "av1");
    let audio_ok = matches!(
        acodec.as_str(),
        "aac" | "mp3" | "opus" | "vorbis" | "pcm_s16le" | "" | "none"
    );

    if container_mp4 && mp4_video_ok && audio_ok {
        info.compatible_reason = "MP4/MOV 原生兼容，直接播放".into();
        return true;
    }
    if container_webm && webm_video_ok && audio_ok {
        info.compatible_reason = "WebM 原生支持，直接播放".into();
        return true;
    }
    if vcodec.is_empty() && !acodec.is_empty() {
        info.compatible_reason = "纯音频，直接播放".into();
        return true;
    }

    info.compatible_reason = format!("FFmpeg 实时转码（{}/{}）", container, vcodec);
    false
}