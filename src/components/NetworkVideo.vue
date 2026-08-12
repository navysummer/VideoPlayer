<script setup>
import { ref, computed, watch, nextTick } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";

const props = defineProps({
  open: { type: Boolean, default: false },
});
const emit = defineEmits(["close", "direct"]);

const DEFAULT_SOURCES = [
  { id: 1, name: "海军VIP解析", url: "https://z1.im1907.top/?jx=" },
];

const sources = ref(DEFAULT_SOURCES.map((s) => ({ ...s })));
const fetchFailed = ref(false);

const videoUrl = ref("");
const selectedSource = ref(sources.value[0].url);
const inputRef = ref(null);

const isIframeVisible = ref(false);
const isDirectVisible = ref(false);
const isCollapsed = ref(false);
const sideCollapsed = ref(false);
const loading = ref(false);
const islandText = ref("播放中");
const lastDirect = ref("");

const showHistory = ref(false);
const history = ref([]);
const HISTORY_KEY = "liuyingNetworkHistory";

// ---------- Local parse sources ----------
async function refreshRemoteSources() {
  try {
    const json = await import("../network-sources.json");
    const list = Array.isArray(json.default)
      ? json.default
      : Array.isArray(json)
        ? json
        : [];
    const clean = list
      .filter(
        (s) =>
          s &&
          s.name &&
          (s.url.startsWith("http://") || s.url.startsWith("https://"))
      )
      .map((s, i) => ({ id: i + 1, name: s.name.trim(), url: s.url.trim() }));
    if (clean.length) {
      sources.value = clean;
      if (!clean.some((s) => s.url === selectedSource.value)) {
        selectedSource.value = clean[0].url;
      }
      fetchFailed.value = false;
      return;
    }
    throw new Error("no sources parsed");
  } catch {
    sources.value = DEFAULT_SOURCES.map((s) => ({ ...s }));
    selectedSource.value = DEFAULT_SOURCES[0].url;
    fetchFailed.value = true;
  }
}

watch(
  () => props.open,
  (o) => {
    if (o) {
      isIframeVisible.value = false;
      isDirectVisible.value = false;
      loading.value = false;
      showHistory.value = false;
      loadHistory();
      refreshRemoteSources();
      nextTick(() => inputRef.value?.focus());
    } else {
      clearTimeout(loadTimer);
      loading.value = false;
    }
  }
);

const playerUrl = computed(() => {
  const v = (videoUrl.value || "").trim();
  return v ? selectedSource.value + v : "";
});

function play() {
  const v = (videoUrl.value || "").trim();
  if (!v) return;
  isIframeVisible.value = true;
  isDirectVisible.value = false;
  isCollapsed.value = false;
  loading.value = true;
  islandText.value = "播放中 · 收起";
  addHistory(v);
  clearTimeout(loadTimer);
  loadTimer = setTimeout(() => {
    loading.value = false;
  }, 4000);
}

let loadTimer = null;

function onFrameLoad() {
  loading.value = false;
  clearTimeout(loadTimer);
}

function directPlay() {
  const v = (videoUrl.value || "").trim();
  if (!v) {
    return;
  }
  addHistory(v);
  lastDirect.value = v;
  emit("direct", v);
}

function toggleIsland() {
  if (!isIframeVisible.value && !isDirectVisible.value) return;
  isCollapsed.value = !isCollapsed.value;
  islandText.value = isCollapsed.value ? "已收起 · 展开" : "播放中 · 收起";
}

function toggleFullscreen() {
  if (isCollapsed.value || !isIframeVisible.value) {
    isCollapsed.value = false;
    islandText.value = isIframeVisible.value ? "播放中 · 收起" : "播放中";
  }
  const win = getCurrentWindow();
  win.isFullscreen().then((fs) => win.setFullscreen(!fs)).catch(() => {});
}

// ---------- History ----------
function loadHistory() {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    history.value = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(history.value)) history.value = [];
  } catch {
    history.value = [];
  }
}

function addHistory(u) {
  if (!u) return;
  const list = [u, ...history.value.filter((h) => h !== u)];
  history.value = list.slice(0, 50);
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history.value));
  } catch {}
}

function clearHistory() {
  history.value = [];
  try {
    localStorage.removeItem(HISTORY_KEY);
  } catch {}
}

function toggleHistory() {
  showHistory.value = !showHistory.value;
}

function toggleSide() {
  sideCollapsed.value = !sideCollapsed.value;
  if (!sideCollapsed.value) {
    nextTick(() => inputRef.value?.focus());
  }
}

function playHistory(u) {
  showHistory.value = false;
  videoUrl.value = u;
  play();
}
</script>

<template>
  <div v-show="props.open" class="net-overlay">
    <div class="net-page">
      <header class="net-head">
        <h2 class="net-title">网络视频</h2>
        <span class="net-sub">解析播放 · 搜片观影</span>
        <div class="spacer"></div>
        <button class="tbtn ghost" @click="emit('close')">返回播放器</button>
      </header>

      <div class="net-body">
        <aside class="net-side" :class="{ collapsed: sideCollapsed }">
          <template v-if="!sideCollapsed">
            <div class="net-side-head">
              <span>搜索配置</span>
              <button class="net-fold" title="收起面板" @click="toggleSide">
                <svg viewBox="0 0 24 24" width="15" height="15"><path fill="currentColor" d="M14.7 5.3a1 1 0 0 0-1.4 0l-5 5a1 1 0 0 0 0 1.4l5 5a1 1 0 0 0 1.4-1.4L10.4 11l4.3-4.3a1 1 0 0 0 0-1.4Z"/></svg>
              </button>
            </div>
            <div class="net-field">
              <label for="net-input">视频地址 / 片名</label>
              <input
                id="net-input"
                ref="inputRef"
                v-model.trim="videoUrl"
                type="text"
                spellcheck="false"
                placeholder="粘贴视频页地址，或直接输入片名"
                @keydown.enter="play"
              />
            </div>

            <div class="net-field">
              <label for="net-source">解析接口</label>
              <select id="net-source" v-model="selectedSource">
                <option v-for="s in sources" :key="s.id" :value="s.url">
                  {{ s.name }}
                </option>
              </select>
            </div>

            <div class="net-btns">
              <button class="tbtn primary" @click="play">▶ 播放</button>
              <button class="tbtn ghost" @click="directPlay">本机直连</button>
              <button class="tbtn ghost" @click="toggleFullscreen">全屏</button>
              <button class="tbtn ghost" @click="toggleHistory">历史</button>
            </div>

            <p class="net-hint">
              · 「播放」接入第三方解析接口，支持视频页地址或片名搜索；<br />
              · 「本机直连」调用内置引擎直接播放视频直链（http/https/m3u8）；<br />
              · 接口为共享公共解析，仅供参考学习使用。
            </p>
          </template>
          <template v-else>
            <div class="net-side-pin"><span class="play-tri"></span></div>
            <button class="tbtn ghost net-expand" title="展开搜索面板" @click="toggleSide">
              展开
            </button>
          </template>
        </aside>

        <main class="net-main">
          <iframe
            v-show="isIframeVisible && !isCollapsed"
            class="net-frame"
            :src="playerUrl"
            allowfullscreen
            @load="onFrameLoad"
          ></iframe>

          <div
            v-show="(!isIframeVisible && !isDirectVisible) || (isIframeVisible && isCollapsed)"
            class="net-placeholder"
          >
            <div class="net-placeholder-icon"><span class="play-tri"></span></div>
            <p>输入视频地址或片名，点「播放」开始</p>
            <div v-if="isDirectVisible" class="net-direct-tip">
              已调用内置引擎播放：<span>{{ lastDirect }}</span>
            </div>
          </div>

          <div v-if="loading && isIframeVisible" class="net-loading">
            <div class="spinner"></div>
            <span>正在加载解析…</span>
          </div>

          <div
            v-show="isIframeVisible || isDirectVisible"
            class="net-island"
            :class="{ collapsed: isCollapsed }"
            @click="toggleIsland"
          >
            <svg
              v-if="!isCollapsed"
              viewBox="0 0 24 24"
              width="14"
              height="14"
            ><path
              fill="currentColor"
              d="M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Zm6.7 7.5v3.8a1 1 0 0 0 1.6.8l2.7-1.9a1 1 0 0 0 0-1.6l-2.7-1.9a1 1 0 0 0-1.6.8Z"
            /></svg>
            <span>{{ islandText }}</span>
          </div>
        </main>
      </div>
    </div>

    <div v-if="showHistory" class="modal hist-overlay" @click.self="toggleHistory">
      <div class="modal-card history-card">
        <h3>播放历史</h3>
        <ul v-if="history.length" class="history-list">
          <li v-for="(h, i) in history" :key="i">
            <button class="history-item" :title="h" @click="playHistory(h)">{{ h }}</button>
          </li>
        </ul>
        <p v-else class="history-empty">暂无记录</p>
        <div class="modal-actions">
          <button class="ghost" @click="clearHistory">清空历史</button>
          <button class="primary" @click="toggleHistory">关闭</button>
        </div>
      </div>
    </div>
  </div>
</template>