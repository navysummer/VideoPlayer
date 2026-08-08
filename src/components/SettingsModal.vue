<script setup>
import { invoke } from "@tauri-apps/api/core";

defineProps({
  open: { type: Boolean, default: false },
});
const emit = defineEmits(["close"]);

const links = [
  { icon: "G", label: "GitHub", href: "https://github.com/navysummer", sub: "github.com/navysummer" },
  { icon: "博", label: "博客", href: "https://www.cnblogs.com/navysummer", sub: "cnblogs.com/navysummer" },
];

async function openExternal(url) {
  try {
    await invoke("open_external", { url });
  } catch {
    window.open(url, "_blank");
  }
}
</script>

<template>
  <div v-show="open" id="settingsModal" class="modal" @click.self="emit('close')">
    <div class="modal-card settings-card">
      <h3>设置</h3>
      <div class="settings-tabs">
        <button class="tab active">关于</button>
      </div>

      <div class="settings-panel">
        <div class="about-head">
          <div class="about-mark"><span class="play-tri"></span></div>
          <div class="about-title">
            <span class="about-name">流影</span>
            <span class="about-sub">LY·YING　全格式视频播放器</span>
          </div>
        </div>

        <div class="about-section">
          <h4>书卷雅意 · 相迎万相</h4>
          <ul class="feature-list">
            <li>千里屏幕收一匣：现代视觉风格、雕花纹饰，声色俱佳。</li>
            <li>一卷在手，万格式可读：本地影片、在线视频、直播流随取随看，今日即开即赏。</li>
            <li>尚无阻格，跨平台同游：一款可伴 Windows、macOS、Linux 世代传抄。</li>
            <li>静谧省心：即点即放，无需额外安装任何环境。</li>
            <li>体贴细节：倍速、音量、进度、浮窗、全屏、拖曳任意快件。</li>
          </ul>
        </div>

        <div class="about-divider"></div>

        <div class="about-section">
          <h4>作者</h4>
          <ul class="author-list">
            <li>
              <span class="author-icon">名</span>
              <span class="author-main">
                <span class="author-name">navysummer</span>
                <span class="author-role">独立创作者</span>
              </span>
            </li>
            <li v-for="l in links" :key="l.href">
              <span class="author-icon">{{ l.icon }}</span>
              <span class="author-main">
                <button class="link" @click="openExternal(l.href)">{{ l.label }}</button>
                <span class="author-role">{{ l.sub }}</span>
              </span>
            </li>
            <li>
              <span class="author-icon">微</span>
              <span class="author-main">
                <span class="author-name">navysummer1001</span>
                <span class="author-role">微信 ID</span>
              </span>
            </li>
          </ul>
          <p class="about-foot">愿此卷录君心 · 谢谢使用流影</p>
        </div>
      </div>

      <div class="modal-actions">
        <button class="ghost" @click="emit('close')">关闭</button>
      </div>
    </div>
  </div>
</template>