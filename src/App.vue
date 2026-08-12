<script setup>
import { ref, reactive, onMounted, onBeforeUnmount } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import UrlModal from "./components/UrlModal.vue";
import SettingsModal from "./components/SettingsModal.vue";
import NetworkVideo from "./components/NetworkVideo.vue";
import Toast from "./components/Toast.vue";

const videoEl = ref(null);
const stageEl = ref(null);
const seekEl = ref(null);
const volEl = ref(null);
const timeEl = ref(null);

const emptyVisible = ref(true);
const loadingVisible = ref(false);
const loadingText = ref("正在解析媒体…");
const dropHintVisible = ref(false);
const controlsVisible = ref(false);
const tprogressVisible = ref(false);
const tprogressBarEl = ref(null);
const tprogressTextEl = ref(null);
const metaTitle = ref("未加载媒体");
const metaDetail = ref("");
const metaCompatVisible = ref(false);
const metaCompatCls = ref("ok");
const metaCompatText = ref("");

const urlModalOpen = ref(false);
const settingsOpen = ref(false);
const networkOpen = ref(false);

const playingState = ref(false);
const fsState = ref(false);
const iconVolHidden = ref(false);
const iconMuteHidden = ref(true);
const speedText = ref("1x");
const stageIdle = ref(false);

const toastVisible = ref(false);
const toastMsg = ref("");
const toastIsErr = ref(false);

const state = reactive({
  media: null,
  streamUrl: null,
  idleTimer: null,
  pollTimer: null,
  scrubbing: false,
});

const SPEEDS = [0.5, 0.75, 1, 1.25, 1.5, 2];
let speedIdx = 2;

// ---------- Utilities ----------
function fmtTime(sec) {
  if (!isFinite(sec) || sec == null) return "00:00";
  sec = Math.max(0, Math.floor(sec));
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

function fmtBytes(n) {
  if (n < 1024) return n + " B";
  if (n < 1048576) return (n / 1024).toFixed(1) + " KB";
  if (n < 1073741824) return (n / 1048576).toFixed(1) + " MB";
  return (n / 1073741824).toFixed(2) + " GB";
}

function fileNameFromUri(uri) {
  try {
    const clean = uri.replace(/^[a-z]+:\/\//i, "");
    const parts = clean.split(/[?#]/)[0].split("/").pop();
    return decodeURIComponent(parts || uri);
  } catch {
    return uri;
  }
}

function fileBase(name) {
  const clean = name.startsWith("file://") ? name.slice("file://".length) : name;
  const base = clean.split(/[\\/]/).pop() || clean;
  return decodeURIComponent(base).replace(/\.[a-zA-Z0-9]+$/, "");
}

function infoString(info) {
  const parts = [];
  const fmt = (info.format_name || "动态").split(",")[0];
  if (fmt) parts.push(fmt.toUpperCase());
  const v = info.videos[0];
  if (v) {
    if (v.width && v.height) parts.push(`${v.width}×${v.height}`);
    if (v.fps) parts.push(`${Math.round(v.fps)}fps`);
    parts.push(v.codec_name.toUpperCase());
  }
  const a = info.audios[0];
  if (a && a.codec_name) parts.push(`${a.codec_name.toUpperCase()}`);
  if (info.file_size) parts.push(fmtBytes(info.file_size));
  if (info.duration) parts.push(`时长 ${fmtTime(info.duration)}`);
  return parts.filter(Boolean).join(" · ");
}

let toastT = null;
function toast(msg, isErr = false) {
  toastMsg.value = msg;
  toastIsErr.value = isErr;
  toastVisible.value = true;
  clearTimeout(toastT);
  toastT = setTimeout(() => {
    toastVisible.value = false;
  }, 3600);
}

// ---------- Loading media ----------
async function openMedia(uri) {
  if (!uri) return;
  loadingVisible.value = true;
  loadingText.value = "正在解析媒体…";
  emptyVisible.value = false;

  try {
    const res = await invoke("open_media", { uri });
    state.media = res.info;
    state.streamUrl = res.url;
    loadStream(res.info);
  } catch (err) {
    loadingVisible.value = false;
    emptyVisible.value = true;
    toast(typeof err === "string" ? err : String(err), true);
  }
}

function loadStream(info) {
  loadingText.value = info.directly_playable
    ? info.is_local ? "正在加载…" : "正在建立直连…"
    : "FFmpeg 首次转码中，请稍候…";
  metaTitle.value =
    (info.is_local ? fileBase(info.uri) : fileNameFromUri(info.uri)) || "网络媒体";
  metaDetail.value = infoString(info);

  metaCompatVisible.value = true;
  if (info.directly_playable) {
    metaCompatText.value = info.is_local ? "● 原生播放" : "● 实时直连";
    metaCompatCls.value = "ok";
  } else {
    metaCompatText.value = "● FFmpeg 实时转码";
    metaCompatCls.value = "transcode";
  }

  if (videoEl.value.src) {
    videoEl.value.pause();
    videoEl.value.removeAttribute("src");
    videoEl.value.load();
  }
  videoEl.value.src = state.streamUrl;
  videoEl.value.play().catch(() => {});

  const needsProgress = state.streamUrl && state.streamUrl.includes("127.0.0.1");
  startProgressPolling(needsProgress);
}

function startProgressPolling(active) {
  stopProgressPolling();
  if (!active) {
    tprogressVisible.value = false;
    return;
  }
  const update = async () => {
    try {
      const st = await invoke("stream_status");
      if (!st.finished && (st.mode === "transcode" || st.mode === "relay")) {
        tprogressVisible.value = true;
        const pct = st.progress_pct || 0;
        tprogressBarEl.value.style.width = pct + "%";
        const label = state.media && state.media.directly_playable ? "正在加载" : "正在转码";
        tprogressTextEl.value.textContent = `${label} ${pct}% · ${fmtBytes(st.written_bytes)}`;
      } else {
        tprogressVisible.value = false;
      }
    } catch {}
  };
  update();
  state.pollTimer = setInterval(update, 1200);
}

function stopProgressPolling() {
  if (state.pollTimer) {
    clearInterval(state.pollTimer);
    state.pollTimer = null;
  }
}

// ---------- Video events ----------
function onLoadedMetadata() {
  loadingVisible.value = false;
  emptyVisible.value = false;
  controlsVisible.value = true;
  updateSeek();
}
function onPlaying() {
  setPlaying(true);
}
function onPause() {
  setPlaying(false);
}
function onEnded() {
  setPlaying(false);
}
function onWaiting() {
  if (state.media && !state.media.is_local) {
    loadingVisible.value = true;
    loadingText.value = "缓冲中…";
  }
}
function onCanplay() {
  loadingVisible.value = false;
}
function onError() {
  loadingVisible.value = false;
  toast("视频加载失败，请检查文件或网络地址", true);
}

function setPlaying(p) {
  playingState.value = p;
  if (p) wakeControls();
}

function updateSeek() {
  const v = videoEl.value;
  if (!v) return;
  const live = !v.duration || !isFinite(v.duration) || v.duration === Infinity;
  if (live) {
    seekEl.value.value = 0;
    timeEl.value.innerHTML = `${fmtTime(v.currentTime)}<i>/</i> 直播`;
    return;
  }
  const ratio = v.currentTime / v.duration;
  seekEl.value.value = Math.round(ratio * 1000);

  let bufferRatio = 0;
  try {
    if (v.buffered.length)
      bufferRatio = v.buffered.end(v.buffered.length - 1) / v.duration;
  } catch (err) {}
  const fill = ratio * 100;
  const buffer = Math.max(bufferRatio * 100, fill + 0.6);
  seekEl.value.style.background = `linear-gradient(90deg,
    #d8b97f 0%, #c9a25f ${fill}%,
    rgba(255,235,200,.18) ${fill}% ${buffer}%,
    rgba(255,235,200,.08) ${buffer}% 100%)`;
  timeEl.value.innerHTML = `${fmtTime(v.currentTime)}<i>/</i>${fmtTime(v.duration)}`;
}

// ---------- Controls ----------
function onSeekInput() {
  const v = videoEl.value;
  if (!v.duration || !isFinite(v.duration)) return;
  const t = (seekEl.value.value / 1000) * v.duration;
  v.currentTime = t;
}
function onSeekDown() {
  state.scrubbing = true;
}
function onSeekUp() {
  state.scrubbing = false;
}

function togglePlay() {
  const v = videoEl.value;
  if (v.paused || v.ended) v.play().catch(() => {});
  else v.pause();
}

function seekBy(delta) {
  const v = videoEl.value;
  if (!v.duration || !isFinite(v.duration)) return;
  v.currentTime = Math.max(0, Math.min(v.duration, v.currentTime + delta));
}

function onVolInput() {
  const v = volEl.value.value / 100;
  videoEl.value.volume = v;
  videoEl.value.muted = v === 0;
  updateMuteIcon();
}

function toggleMute() {
  videoEl.value.muted = !videoEl.value.muted;
  updateMuteIcon();
}

function updateMuteIcon() {
  const muted = videoEl.value.muted || videoEl.value.volume === 0;
  iconVolHidden.value = muted;
  iconMuteHidden.value = !muted;
}

function cycleSpeed() {
  speedIdx = (speedIdx + 1) % SPEEDS.length;
  videoEl.value.playbackRate = SPEEDS[speedIdx];
  speedText.value = SPEEDS[speedIdx] + "x";
}

async function toggleFullscreen() {
  state.fs = !state.fs;
  stageEl.value.classList.add("fs-mode");
  const win = getCurrentWindow();
  await win.setFullscreen(state.fs);
}

// ---------- Idle controls hiding ----------
function wakeControls() {
  stageIdle.value = false;
  if (playingState.value) {
    clearTimeout(state.idleTimer);
    state.idleTimer = setTimeout(() => (stageIdle.value = true), 2600);
  }
}

// ---------- File / URL opening ----------
async function openFileDialog() {
  try {
    const picked = await invoke("open_file_dialog");
    if (picked) openMedia(picked);
  } catch (err) {
    toast(String(err), true);
  }
}
function openUrlModal() {
  urlModalOpen.value = true;
}
function onUrlSubmit(url) {
  openMedia(url);
}

function openSettings() {
  settingsOpen.value = true;
}

function onNetworkDirect(url) {
  networkOpen.value = false;
  openMedia(url);
}

// ---------- Keyboard ----------
function onKeydown(e) {
  if (networkOpen.value) {
    if (e.key === "Escape") networkOpen.value = false;
    return;
  }
  if (urlModalOpen.value) {
    if (e.key === "Escape") urlModalOpen.value = false;
    return;
  }
  switch (e.key) {
    case " ":
      e.preventDefault();
      togglePlay();
      break;
    case "ArrowRight":
      seekBy(10);
      break;
    case "ArrowLeft":
      seekBy(-10);
      break;
    case "ArrowUp":
      e.preventDefault();
      if (videoEl.value.volume < 1) {
        videoEl.value.volume = Math.min(1, videoEl.value.volume + 0.1);
        volEl.value.value = videoEl.value.volume * 100;
      }
      break;
    case "ArrowDown":
      e.preventDefault();
      if (videoEl.value.volume > 0) {
        videoEl.value.volume = Math.max(0, videoEl.value.volume - 0.1);
        volEl.value.value = videoEl.value.volume * 100;
      }
      break;
    case "m":
    case "M":
      toggleMute();
      break;
    case "f":
    case "F":
      toggleFullscreen();
      break;
    case "Escape":
      if (state.fs) toggleFullscreen();
      break;
  }
}

// ---------- Lifecycle ----------
onMounted(() => {
  document.addEventListener("mousemove", wakeControls);
  document.addEventListener("keydown", onKeydown);
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("drop", (e) => e.preventDefault());

  const win = getCurrentWindow();
  win.onDragDropEvent(async (event) => {
    if (!event.payload) return;
    if (event.payload.type === "over" || event.payload.type === "enter")
      dropHintVisible.value = true;
    if (event.payload.type === "leave") dropHintVisible.value = false;
    if (event.payload.type === "drop") {
      dropHintVisible.value = false;
      const paths = event.payload.paths;
      if (paths && paths.length) openMedia(paths[0]);
    }
  });

  window.addEventListener("beforeunload", () => {
    invoke("stop_playback").catch(() => {});
  });
});

onBeforeUnmount(() => {
  document.removeEventListener("mousemove", wakeControls);
  document.removeEventListener("keydown", onKeydown);
  stopProgressPolling();
  invoke("stop_playback").catch(() => {});
});
</script>

<template>
  <div class="app">

    <!-- ===== Top bar ===== -->
    <header class="topbar">
      <div class="brand">
        <div class="brand-mark"><span class="play-tri"></span></div>
        <div class="brand-text">
          <span class="brand-name">流影</span>
          <span class="brand-sub">LYU YING</span>
        </div>
      </div>
      <div class="top-actions">
        <button class="tbtn ghost" @click="networkOpen = true">
          <svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Zm7 9h-3.1a15.7 15.7 0 0 0-.9-4.4A8.1 8.1 0 0 1 19 11Zm-10 0h-4a8.1 8.1 0 0 1 4-4.4 15.7 15.7 0 0 0-.9 4.4Zm0 2a15.7 15.7 0 0 0 .9 4.4 8.1 8.1 0 0 1-4-4.4h3.1Zm2 5.2A11 11 0 0 1 9.9 13h4.2a11 11 0 0 1-1.1 5.2A11 11 0 0 1 12 18.2a11 11 0 0 1-1-1ZM14.9 7a15.7 15.7 0 0 1 .9 4h3.1a8.1 8.1 0 0 0-4-4ZM12 4.1c.4.3.7.7 1 1.1a12 12 0 0 0-2 0 7 7 0 0 1 1-1.1Zm0 15.8c-.4-.3-.7-.7-1-1.1a12 12 0 0 0 2 0 7 7 0 0 1-1 1.1Z"/></svg>
          网络视频
        </button>
        <button class="tbtn ghost" @click="openUrlModal">
          <svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M10.6 13.4a1 1 0 0 1-1.4 0 4 4 0 0 1 0-5.7l4-4a4 4 0 1 1 5.7 5.7l-1.8 1.8a1 1 0 1 1-1.4-1.4l1.8-1.8a2 2 0 0 0-2.9-2.9l-4 4a2 2 0 0 0 0 2.9 1 1 0 0 1 0 1.4Z"/><path fill="currentColor" d="M13.4 10.6a1 1 0 0 1 1.4 0 4 4 0 0 1 0 5.7l-4 4a4 4 0 1 1-5.7-5.7l1.8-1.8a1 1 0 1 1 1.4 1.4l-1.8 1.8a2 2 0 0 0 2.9 2.9l4-4a2 2 0 0 0 0-2.9 1 1 0 0 1 0-1.4Z"/></svg>
          网络播放
        </button>
        <button class="tbtn primary" @click="openFileDialog">
          <svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M12 3a1 1 0 0 1 1 1v6h6a1 1 0 1 1 0 2h-6v6a1 1 0 1 1-2 0v-6H5a1 1 0 1 1 0-2h6V4a1 1 0 0 1 1-1Z"/></svg>
          打开文件
        </button>
      </div>
    </header>

    <!-- ===== Main stage ===== -->
    <main ref="stageEl" class="stage" :class="{ idle: stageIdle }">
      <video ref="videoEl" playsinline
        @loadedmetadata="onLoadedMetadata"
        @playing="onPlaying"
        @pause="onPause"
        @ended="onEnded"
        @waiting="onWaiting"
        @canplay="onCanplay"
        @timeupdate="updateSeek"
        @progress="updateSeek"
        @error="onError"></video>

      <div v-show="emptyVisible" class="empty">
        <div class="empty-icon"><span class="play-tri"></span></div>
        <h2>银屏静候 · 卷帘开卷</h2>
        <p>打开本地影片，或粘贴一曲网络清音，<br/>mp4 · mkv · flv · avi · rmvb · mov · m3u8 · rtmp 皆可相迎</p>
        <div class="empty-buttons">
          <button class="primary big" @click="openFileDialog">选择文件</button>
          <button class="ghost big" @click="openUrlModal">粘贴网络地址</button>
        </div>
      </div>

      <div v-show="loadingVisible" class="loading">
        <div class="spinner"></div>
        <p>{{ loadingText }}</p>
      </div>

      <div v-show="dropHintVisible" class="drop-hint">
        <svg viewBox="0 0 24 24" width="28" height="28"><path fill="currentColor" d="M12 3a1 1 0 0 1 1 1v6h6a1 1 0 1 1 0 2h-6v6a1 1 0 1 1-2 0v-6H5a1 1 0 1 1 0-2h6V4a1 1 0 0 1 1-1Z"/></svg>
        松卷以开
      </div>

      <!-- custom controls -->
      <div v-show="controlsVisible" class="controls">
        <input ref="seekEl" id="seek" class="seek" type="range" min="0" max="1000" value="0" step="1"
          @input="onSeekInput"
          @pointerdown="onSeekDown"
          @pointerup="onSeekUp" />
        <div class="controls-row">
          <button class="cbtn" title="后退 10 秒" @click="seekBy(-10)">
            <svg viewBox="0 0 24 24"><path fill="currentColor" d="M12 5V1L5 7l7 6V9a6 6 0 1 1-6 6h2a4 4 0 1 0 4-4V5Z"/></svg>
          </button>
          <button class="cbtn play" title="播放 / 暂停" @click="togglePlay">
            <svg v-show="!playingState" viewBox="0 0 24 24" class="ic-play"><path fill="currentColor" d="M7 4.5a1 1 0 0 1 1.6-.8l12 8.5a1 1 0 0 1 0 1.6l-12 8.5a1 1 0 0 1-1.6-.8v-17Z"/></svg>
            <svg v-show="playingState" viewBox="0 0 24 24" class="ic-pause"><path fill="currentColor" d="M6 4.5a1 1 0 0 1 2 0v15a1 1 0 1 1-2 0v-15Zm10 0a1 1 0 1 1 2 0v15a1 1 0 1 1-2 0v-15Z"/></svg>
          </button>
          <button class="cbtn" title="前进 10 秒" @click="seekBy(10)">
            <svg viewBox="0 0 24 24"><path fill="currentColor" d="M12 5V1l7 6-7 6V9a6 6 0 1 0 6 6h-2a4 4 0 1 1-4-4V5Z"/></svg>
          </button>
          <span ref="timeEl" class="time" id="time">00:00 <i>/</i> 00:00</span>
          <div class="spacer"></div>
          <span class="speed" @click="cycleSpeed">{{ speedText }}</span>
          <div class="volume">
            <button class="cbtn" title="静音" @click="toggleMute">
              <svg v-show="!iconMuteHidden" viewBox="0 0 24 24" class="ic-vol"><path fill="currentColor" d="M4 9v6h4l5 4V5L8 9H4Zm12 3a3 3 0 0 0-1.5-2.6v5.2A3 3 0 0 0 16 12Zm-1.5-6.9v2.1a5.5 5.5 0 0 1 0 9.6v2.1a7.5 7.5 0 0 0 0-13.8Z"/></svg>
              <svg v-show="iconMuteHidden" viewBox="0 0 24 24" class="ic-mute"><path fill="currentColor" d="M4 9v6h4l5 4V5L8 9H4Zm12.3-2.3 1.2 1.2-3.3 3.3 3.3 3.3-1.2 1.2-3.3-3.3-3.3 3.3-1.2-1.2 3.3-3.3-3.3-3.3 1.2-1.2 3.3 3.3 3.3-3.3Z"/></svg>
            </button>
            <input ref="volEl" id="vol" class="vol" type="range" min="0" max="100" value="80"
              @input="onVolInput" />
          </div>
          <button class="cbtn" title="全屏" @click="toggleFullscreen">
            <svg viewBox="0 0 24 24"><path fill="currentColor" d="M4 4h6v2H6v4H4V4Zm10 0h6v6h-2V6h-4V4ZM4 14h2v4h4v2H4v-6Zm14 0h2v6h-6v-2h4v-4Z"/></svg>
          </button>
        </div>
      </div>

      <!-- transcode progress -->
      <div v-show="tprogressVisible" class="tprogress">
        <div class="tprogress-track"><div ref="tprogressBarEl" class="tprogress-bar"></div></div>
        <span ref="tprogressTextEl">正在转码…</span>
      </div>
    </main>

    <!-- ===== Info bar ===== -->
    <footer class="infobar">
      <div class="meta">
        <span id="metaTitle">{{ metaTitle }}</span>
        <span id="metaDetail" class="dim">{{ metaDetail }}</span>
      </div>
      <div class="info-right">
        <span class="compat" :class="metaCompatCls" v-show="metaCompatVisible">{{ metaCompatText }}</span>
        <button class="tbtn ghost icon-only" title="设置" @click="openSettings">
          <svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M12 8.5A3.5 3.5 0 1 0 12 15.5 3.5 3.5 0 0 0 12 8.5Zm7.4 3.5c0-.6-.05-1.1-.15-1.6l2-1.55-2-3.4-2.35.95a7.4 7.4 0 0 0-2.7-1.55L14 1h-4l-.2 2.4a7.4 7.4 0 0 0-2.7 1.55l-2.35-.95-2 3.4 2 1.55c-.1.5-.15 1-.15 1.6s.05 1.1.15 1.6l-2 1.55 2 3.4 2.35-.95a7.4 7.4 0 0 0 2.7 1.4L10 24h4l.2-2.4a7.4 7.4 0 0 0 2.7-1.4l2.35.95 2-3.4-2-1.55c.1-.5.15-1.1.15-1.6Z"/></svg>
        </button>
      </div>
    </footer>

    <!-- modals & toast -->
    <NetworkVideo :open="networkOpen" @close="networkOpen = false" @direct="onNetworkDirect" />
    <UrlModal :open="urlModalOpen" @close="urlModalOpen = false" @submit="onUrlSubmit" />
    <SettingsModal :open="settingsOpen" @close="settingsOpen = false" />
    <Toast :message="toastMsg" :is-err="toastIsErr" :visible="toastVisible" />
  </div>
</template>