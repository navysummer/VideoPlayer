//! In-process transcoding using the FFmpeg SDK directly (libav*, no external binary).
//!
//! Any input (local file, http(s), m3u8 / HLS, rtmp, …) is decoded and re-encoded
//! into a seekable, progressively-servable fragmented H.264/AAC MP4 file.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ffmpeg::channel_layout::ChannelLayout;
use ffmpeg::codec::context::Context;
use ffmpeg::codec::{decoder, encoder};
use ffmpeg::filter;
use ffmpeg::format::{self, context::Output};
use ffmpeg::frame;
use ffmpeg::media;
use ffmpeg::picture;
use ffmpeg::software::scaling;
use ffmpeg::util::rational::Rational;
use ffmpeg::{codec as fcodec, Dictionary, Packet};

const MOVFLAGS: &str = "+frag_keyframe+empty_moov+default_base_moof\0";

/// Shared progress state between the transcode worker and the HTTP server.
#[derive(Clone, Default)]
pub struct TranscodeStats {
    pub written_bytes: Arc<AtomicU64>,
    pub finished: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
    pub cancel: Arc<AtomicBool>,
}

// ---------------------------------------------------------------------------
// Video: decode -> (scale to yuv420p, even dims) -> libx264 encode
// ---------------------------------------------------------------------------

struct VideoTranscoder {
    ist_index: usize,
    ost_index: usize,
    decoder: decoder::Video,
    input_time_base: Rational,
    encoder: encoder::Video,
    scaler: Option<scaling::Context>,
}

impl VideoTranscoder {
    fn new(ist: &ffmpeg::format::stream::Stream, octx: &mut Output) -> Result<Self> {
        let context =
            Context::from_parameters(ist.parameters()).map_err(|e| anyhow!("打开视频解码器失败: {e}"))?;
        let decoder = context.decoder().video()?;

        let codec = encoder::find(fcodec::Id::H264);
        let mut ost = octx.add_stream(codec)?;

        let mut encoder = Context::new_with_codec(codec.ok_or(anyhow!("缺少 H.264 编码器（libx264）"))?)
            .encoder()
            .video()?;

        ost.set_parameters(&encoder);

        let in_w = decoder.width();
        let in_h = decoder.height();
        let out_w = if in_w % 2 == 0 { in_w } else { in_w.saturating_sub(1) };
        let out_h = if in_h % 2 == 0 { in_h } else { in_h.saturating_sub(1) };

        encoder.set_width(out_w);
        encoder.set_height(out_h);
        encoder.set_aspect_ratio(decoder.aspect_ratio());
        encoder.set_format(ffmpeg::util::format::Pixel::YUV420P);
        encoder.set_frame_rate(decoder.frame_rate());
        encoder.set_time_base(ist.time_base());
        encoder.set_bit_rate(2_000_000);

        let mut opts = Dictionary::new();
        opts.set("preset", "veryfast");
        opts.set("crf", "22");

        let opened = encoder
            .open_with(opts)
            .map_err(|e| anyhow!("打开 libx264 编码器失败: {e}"))?;
        ost.set_parameters(&opened);

        let scaler = if decoder.format() != ffmpeg::util::format::Pixel::YUV420P
            || in_w != out_w
            || in_h != out_h
        {
            Some(
                scaling::Context::get(
                    decoder.format(),
                    in_w,
                    in_h,
                    ffmpeg::util::format::Pixel::YUV420P,
                    out_w,
                    out_h,
                    scaling::Flags::BILINEAR,
                )
                .map_err(|e| anyhow!("初始化视频缩放失败: {e}"))?,
            )
        } else {
            None
        };

        Ok(Self {
            ist_index: ist.index(),
            ost_index: ost.index(),
            decoder,
            input_time_base: ist.time_base(),
            encoder: opened,
            scaler,
        })
    }

    fn decode_packet(&mut self, packet: &Packet, octx: &mut Output) -> Result<()> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| anyhow!("视频解码发送失败: {e}"))?;

        let mut raw = frame::Video::empty();
        while self.decoder.receive_frame(&mut raw).is_ok() {
            let timestamp = raw.timestamp();
            let mut scaled = frame::Video::empty();

            match &mut self.scaler {
                Some(sws) => {
                    sws.run(&raw, &mut scaled)?;
                    scaled.set_pts(timestamp);
                    scaled.set_kind(picture::Type::None);
                    self.encoder.send_frame(&scaled)?;
                }
                None => {
                    raw.set_kind(picture::Type::None);
                    self.encoder.send_frame(&raw)?;
                }
            }
            self.drain_encoder_packets(octx)?;
        }
        Ok(())
    }

    fn drain_encoder_packets(&mut self, octx: &mut Output) -> Result<()> {
        let ost_time_base = octx
            .stream(self.ost_index)
            .ok_or_else(|| anyhow!("找不到输出流 {}", self.ost_index))?
            .time_base();
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.ost_index);
            encoded.rescale_ts(self.input_time_base, ost_time_base);
            encoded.write_interleaved(octx)?;
        }
        Ok(())
    }

    fn flush(&mut self, octx: &mut Output) -> Result<()> {
        self.decoder
            .send_eof()
            .map_err(|e| anyhow!("视频解码结束失败: {e}"))?;
        let mut raw = frame::Video::empty();
        while self.decoder.receive_frame(&mut raw).is_ok() {
            let timestamp = raw.timestamp();
            let mut scaled = frame::Video::empty();
            match &mut self.scaler {
                Some(sws) => {
                    sws.run(&raw, &mut scaled)?;
                    scaled.set_pts(timestamp);
                    scaled.set_kind(picture::Type::None);
                    self.encoder.send_frame(&scaled)?;
                }
                None => {
                    raw.set_kind(picture::Type::None);
                    self.encoder.send_frame(&raw)?;
                }
            }
            self.drain_encoder_packets(octx)?;
        }
        self.encoder
            .send_eof()
            .map_err(|e| anyhow!("视频编码结束失败: {e}"))?;
        self.drain_encoder_packets(octx)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Audio: decode -> filter graph (resample) -> aac encode
// ---------------------------------------------------------------------------

struct AudioTranscoder {
    ist_index: usize,
    ost_index: usize,
    filter: filter::Graph,
    decoder: decoder::Audio,
    encoder: encoder::Audio,
    in_time_base: Rational,
    out_time_base: Rational,
}

fn audio_filter(
    decoder: &decoder::Audio,
    encoder: &encoder::Audio,
) -> Result<filter::Graph, ffmpeg::Error> {
    let mut graph = filter::Graph::new();

    let args = format!(
        "time_base={}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        decoder.time_base(),
        decoder.rate(),
        decoder.format().name(),
        decoder.channel_layout().bits()
    );
    graph.add(&filter::find("abuffer").unwrap(), "in", &args)?;
    graph.add(&filter::find("abuffersink").unwrap(), "out", "")?;

    {
        let mut out = graph.get("out").unwrap();
        out.set_sample_format(encoder.format());
        out.set_channel_layout(encoder.channel_layout());
        out.set_sample_rate(encoder.rate());
    }

    graph.output("in", 0)?.input("out", 0)?.parse("anull")?;
    graph.validate()?;

    if let Some(codec) = encoder.codec() {
        if !codec
            .capabilities()
            .contains(ffmpeg::codec::capabilities::Capabilities::VARIABLE_FRAME_SIZE)
        {
            graph
                .get("out")
                .unwrap()
                .sink()
                .set_frame_size(encoder.frame_size());
        }
    }
    Ok(graph)
}

impl AudioTranscoder {
    fn new(ist: &ffmpeg::format::stream::Stream, octx: &mut Output) -> Result<Self> {
        let context =
            Context::from_parameters(ist.parameters()).map_err(|e| anyhow!("打开音频解码器失败: {e}"))?;
        let mut decoder = context.decoder().audio()?;
        decoder
            .set_parameters(ist.parameters())
            .map_err(|e| anyhow!("设置音频解码参数失败: {e}"))?;

        let codec_id = octx.format().codec(Path::new("stream.mp4"), media::Type::Audio);
        let codec = encoder::find(codec_id).ok_or(anyhow!("缺少 AAC 音频编码器"))?;
        let codec_audio = codec.audio()?;
        let global = octx
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER);

        let mut output = octx.add_stream(codec)?;
        let enc_ctx = Context::from_parameters(output.parameters())?;
        let mut encoder = enc_ctx.encoder().audio()?;

        let channel_layout = codec_audio
            .channel_layouts()
            .map(|cls| cls.best(decoder.channel_layout().channels()))
            .unwrap_or(ChannelLayout::STEREO);

        if global {
            encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }

        encoder.set_rate(decoder.rate() as i32);
        encoder.set_channel_layout(channel_layout);
        if let Some(sample_fmt) = codec_audio.formats().and_then(|mut f| f.next()) {
            encoder.set_format(sample_fmt);
        }
        encoder.set_bit_rate(decoder.bit_rate());
        encoder.set_max_bit_rate(decoder.max_bit_rate());

        let tb = (1, decoder.rate() as i32);
        encoder.set_time_base(tb);
        output.set_time_base(tb);

        let opened = encoder.open_as(codec)?;
        output.set_parameters(&opened);

        let graph = audio_filter(&decoder, &opened)
            .map_err(|e| anyhow!("初始化音频滤波器失败: {e}"))?;

        let in_time_base = decoder.time_base();
        let out_time_base = output.time_base();

        Ok(Self {
            ist_index: ist.index(),
            ost_index: output.index(),
            filter: graph,
            decoder,
            encoder: opened,
            in_time_base,
            out_time_base,
        })
    }

    fn decode_packet(&mut self, packet: &Packet, octx: &mut Output) -> Result<()> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| anyhow!("音频解码发送失败: {e}"))?;
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let timestamp = decoded.timestamp();
            decoded.set_pts(timestamp);
            self.filter
                .get("in")
                .unwrap()
                .source()
                .add(&decoded)
                .map_err(|e| anyhow!("音频送入滤波器失败: {e}"))?;
            self.drain_filter_frames(octx)?;
        }
        Ok(())
    }

    fn drain_filter_frames(&mut self, octx: &mut Output) -> Result<()> {
        let mut filtered = frame::Audio::empty();
        while self
            .filter
            .get("out")
            .unwrap()
            .sink()
            .frame(&mut filtered)
            .is_ok()
        {
            self.encoder
                .send_frame(&filtered)
                .map_err(|e| anyhow!("音频编码发送失败: {e}"))?;
            self.drain_encoder_packets(octx)?;
        }
        Ok(())
    }

    fn drain_encoder_packets(&mut self, octx: &mut Output) -> Result<()> {
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.ost_index);
            encoded.rescale_ts(self.in_time_base, self.out_time_base);
            encoded.write_interleaved(octx)?;
        }
        Ok(())
    }

    fn flush(&mut self, octx: &mut Output) -> Result<()> {
        self.decoder
            .send_eof()
            .map_err(|e| anyhow!("音频解码结束失败: {e}"))?;
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            self.filter
                .get("in")
                .unwrap()
                .source()
                .add(&decoded)
                .map_err(|e| anyhow!("音频送入滤波器失败: {e}"))?;
            self.drain_filter_frames(octx)?;
        }
        self.filter
            .get("in")
            .unwrap()
            .source()
            .flush()
            .map_err(|e| anyhow!("音频滤波器刷新失败: {e}"))?;
        self.drain_filter_frames(octx)?;
        self.encoder
            .send_eof()
            .map_err(|e| anyhow!("音频编码结束失败: {e}"))?;
        self.drain_encoder_packets(octx)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Top-level driver
// ---------------------------------------------------------------------------

fn enable_fragmented_mp4(octx: &mut Output) {
    unsafe {
        let priv_data = (*octx.as_mut_ptr()).priv_data;
        if !priv_data.is_null() {
            let key = b"movflags\0".as_ptr() as *const std::ffi::c_char;
            let val = MOVFLAGS.as_ptr() as *const std::ffi::c_char;
            ffmpeg::ffi::av_opt_set(priv_data, key, val, 0);
        }
    }
}

/// Transcode `input` (path or URL) into a fragmented H.264/AAC MP4 at `output`.
pub fn run(input: &str, output: &Path, stats: &TranscodeStats) -> Result<()> {
    let result = run_inner(input, output, stats);
    if let Err(e) = &result {
        *stats.error.lock().unwrap() = Some(e.to_string());
        log::error!("转码失败: {e}");
    }
    stats.finished.store(true, Ordering::Relaxed);
    result
}

fn run_inner(input: &str, output: &Path, stats: &TranscodeStats) -> Result<()> {
    let mut ictx = format::input(input)?;
    let mut octx = format::output(output)?;
    enable_fragmented_mp4(&mut octx);

    let video_idx = ictx.streams().best(media::Type::Video).map(|s| s.index());
    let audio_idx = ictx.streams().best(media::Type::Audio).map(|s| s.index());

    let mut video: Option<VideoTranscoder> = None;
    let mut audio: Option<AudioTranscoder> = None;

    for (index, stream) in ictx.streams().enumerate() {
        if video.is_none() && Some(index) == video_idx {
            let t = VideoTranscoder::new(&stream, &mut octx)
                .map_err(|e| anyhow!("视频流初始化失败: {e}"))?;
            video = Some(t);
        } else if audio.is_none() && Some(index) == audio_idx {
            let t = AudioTranscoder::new(&stream, &mut octx)
                .map_err(|e| anyhow!("音频流初始化失败: {e}"))?;
            audio = Some(t);
        }
    }

    octx.write_header()?;

    let mut count = 0u64;
    for (stream, packet) in ictx.packets() {
        if stats.cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let index = stream.index();
        if let Some(v) = &mut video {
            if v.ist_index == index {
                v.decode_packet(&packet, &mut octx)?;
            }
        }
        if let Some(a) = &mut audio {
            if a.ist_index == index {
                a.decode_packet(&packet, &mut octx)?;
            }
        }

        count += 1;
        if count % 240 == 0 {
            if let Ok(md) = std::fs::metadata(output) {
                stats.written_bytes.store(md.len(), Ordering::Relaxed);
            }
        }
    }

    if let Some(mut v) = video {
        v.flush(&mut octx)?;
    }
    if let Some(mut a) = audio {
        a.flush(&mut octx)?;
    }

    octx.write_trailer()?;

    if let Ok(md) = std::fs::metadata(output) {
        stats.written_bytes.store(md.len(), Ordering::Relaxed);
    }
    Ok(())
}