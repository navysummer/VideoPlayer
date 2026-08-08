<script setup>
import { ref, watch, nextTick } from "vue";

const props = defineProps({
  open: { type: Boolean, default: false },
});
const emit = defineEmits(["close", "submit"]);

const input = ref("");

watch(
  () => props.open,
  (o) => {
    if (o) {
      input.value = "";
      nextTick(() => inputEl.value?.focus());
    }
  }
);

const inputEl = ref(null);

function submit() {
  const v = input.value.trim();
  emit("close");
  if (v) emit("submit", v);
}
</script>

<template>
  <div v-show="open" id="urlModal" class="modal" @click.self="emit('close')">
    <div class="modal-card">
      <h3>播放网络地址</h3>
      <p class="modal-hint">支持 http(s) 视频、m3u8 / HLS 直播流、RTMP 推流地址等</p>
      <input
        ref="inputEl"
        id="urlInput"
        v-model="input"
        type="text"
        spellcheck="false"
        placeholder="https://… /  http://… / rtmp://… / m3u8 地址"
        @keydown.enter="submit"
      />
      <div class="modal-actions">
        <button class="ghost" @click="emit('close')">取消</button>
        <button class="primary" @click="submit">开始播放</button>
      </div>
    </div>
  </div>
</template>