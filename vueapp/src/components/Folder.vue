<template>
  <div class="folder" @click="handleClick">
    <div class="ficon">
      <svg viewBox="0 0 24 24" fill="none">
        <path
            d="M3 6.5C3 5.7 3.7 5 4.5 5H9l2 2h8.5c.8 0 1.5.7 1.5 1.5v9c0 .8-.7 1.5-1.5 1.5h-15C3.7 19 3 18.3 3 17.5v-11z"
            fill="#fbd277"
            stroke="#f0b53d"
            stroke-width="1"
        />
        <path
            d="M3 9h18v8.5c0 .8-.7 1.5-1.5 1.5h-15C3.7 19 3 18.3 3 17.5V9z"
            fill="#fdd98a"
        />
      </svg>
    </div>

    <div class="fname">{{ folder.name }}</div>
    <div class="fsub">{{ folder.size }} 个文件</div>
  </div>
</template>

<script setup lang="ts">
defineProps<{
  folder: any
}>()

const emit = defineEmits(['click'])

const handleClick = (event: Event) => {
  console.log('Folder组件点击事件触发')
  emit('click')
}
</script>

<style scoped>
.folder {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 12px;
  cursor: pointer;
  box-shadow: var(--shadow);
  padding: 16px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  transition: transform .12s, box-shadow .12s;
}

.folder:hover {
  transform: translateY(-3px);
  box-shadow: 0 6px 26px rgba(0, 0, 0, .10);
}

.folder .ficon svg {
  width: 46px;
  height: 46px;
  display: block;
}

.folder .fname {
  font-size: 13px;
  font-weight: 500;
  line-height: 1.35;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.folder .fsub {
  font-size: 11px;
  color: var(--sub);
}
</style>