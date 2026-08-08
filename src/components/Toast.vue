<script setup>
defineProps({
  message: { type: String, default: "" },
  isErr: { type: Boolean, default: false },
  visible: { type: Boolean, default: false },
});
</script>

<template>
  <div id="toast" class="toast" :class="{ hidden: !visible, err: isErr }">{{ message }}</div>
</template>