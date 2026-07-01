<template>
  <div class="card" @click="emit('click')">
    <div class="cover">
      <!-- 封面图 -->
      <img
          :src="`api/page?id=${encodeURIComponent(props.book.id)}&dpi=80`"
          loading="lazy"
      />

      <!-- 占位 -->
      <div class="ph">
        <svg viewBox="0 0 24 24" fill="none">
          <path
              d="M6 2h8l4 4v16H6z"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linejoin="round"
          />
          <path
              d="M14 2v4h4"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linejoin="round"
          />
        </svg>
      </div>

      <!-- 阅读进度徽标 -->
      <span v-if="progressText" class="badge">
        {{ progressText }}
      </span>
    </div>

    <div class="meta">
      <p class="title" :title="book.name">{{ book.name }}</p>
      <div class="sub">
        {{ fmtSize(book.size) }}
        <span v-if="percent">　·　{{ percent }}%</span>
      </div>
      <div v-if="percent" class="progressbar">
        <i :style="{ width: percent + '%' }"></i>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import {ref, computed} from 'vue'

const props = defineProps<{
  book: any
}>()

const emit = defineEmits(['click'])

const percent = computed(() => {
  return props.book.progress?.percent ?? 0
})

const progressText = computed(() => {
  const p = props.book.progress
  if (!p || !p.page) return ''
  return `读到 ${p.page + 1}${p.totalPages ? '/' + p.totalPages : ''} 页`
})

function fmtSize(n: number) {
  if (n < 1024) return n + ' B'
  if (n < 1048576) return (n / 1024).toFixed(0) + ' KB'
  return (n / 1048576).toFixed(1) + ' MB'
}

</script>

<style scoped>
.card {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 12px;
  overflow: hidden;
  cursor: pointer;
  box-shadow: var(--shadow);
  transition: transform .12s, box-shadow .12s;
}

.card:hover {
  transform: translateY(-3px);
  box-shadow: 0 6px 26px rgba(0, 0, 0, .10);
}

.cover {
  aspect-ratio: 3 / 4;
  background: linear-gradient(135deg, #e9eef7, #dde5f2);
  display: flex;
  align-items: center;
  justify-content: center;
  position: relative;
  overflow: hidden;
}

.cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
  position: absolute; /* 封面图：绝对定位 */
  z-index: 2; /* 在上层 */
}

.cover .ph {
  color: #b3bccb;
  position: absolute; /* 封面图：绝对定位 */
  z-index: 1; /* 在上层 */
}

.cover .ph svg {
  width: 46px;
  height: 46px;
}

.badge {
  position: absolute;
  left: 8px;
  bottom: 8px;
  background: rgba(47, 111, 237, .92);
  color: #fff;
  font-size: 11px;
  padding: 2px 7px;
  border-radius: 999px;
}

.card .meta {
  padding: 10px 11px 12px;
}

.card .title {
  font-size: 13px;
  font-weight: 500;
  line-height: 1.35;
  margin: 0 0 4px;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  min-height: 35px;
}

.card .sub {
  font-size: 11px;
  color: var(--sub);
}

.progressbar {
  height: 3px;
  background: #eef0f3;
  border-radius: 2px;
  margin-top: 7px;
  overflow: hidden;
}

.progressbar > i {
  display: block;
  height: 100%;
  background: var(--accent);
}
</style>