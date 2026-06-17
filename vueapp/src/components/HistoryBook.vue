<template>
  <div class="rcover">
    <div v-show="coverBase64.length <= 0" class="rph">
      <svg viewBox="0 0 24 24" fill="none">
        <path d="M6 2h8l4 4v16H6z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
        <path d="M14 2v4h4" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
      </svg>
    </div>

    <img
        v-if="coverBase64.length > 0"
        :src="coverBase64"
        loading="lazy"
    />
    <span class="rpage" v-if="book.progress?.page">
              {{ book.progress.page }}{{ book.progress.totalPages ? '/' + book.progress.totalPages : '' }}
            </span>
    <div class="rbar">
      <i :style="{ width: (book.progress?.percent || 0) + '%' }"></i>
    </div>
  </div>
  <div class="rname">{{ book.name }}</div>

</template>

<script setup lang="ts">
import {ref, computed} from 'vue'
import request from '@/utils/request'

const props = defineProps<{
  book: any
}>()

const emit = defineEmits(['click'])
const coverBase64 = ref('')

function loadCoverBase64() {
  request.get(`page?id=${encodeURIComponent(props.book.id)}`).then((res) => {
    coverBase64.value = res.data.base64 || ''
  })
}

loadCoverBase64()

</script>

<style scoped>


.cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

.cover .ph {
  color: #b3bccb;
}

.cover .ph svg {
  width: 46px;
  height: 46px;
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


.ritem {
  flex: 0 0 auto;
  width: 96px;
  cursor: pointer;
  scroll-snap-align: start;
  transition: transform .12s;
}

.ritem:hover {
  transform: translateY(-3px);
}

.ritem .rcover {
  width: 96px;
  height: 128px;
  border-radius: 9px;
  overflow: hidden;
  position: relative;
  background: linear-gradient(135deg, #e9eef7, #dde5f2);
  border: 1px solid var(--border);
  box-shadow: var(--shadow);
  display: flex;
  align-items: center;
  justify-content: center;
}

.ritem .rcover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

.ritem .rcover .rph {
  color: #b3bccb;
}

.ritem .rcover .rph svg {
  width: 34px;
  height: 34px;
}

.ritem .rbar {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  height: 3px;
  background: rgba(0, 0, 0, .12);
}

.ritem .rbar > i {
  display: block;
  height: 100%;
  background: var(--accent);
}

.ritem .rpage {
  position: absolute;
  right: 5px;
  top: 5px;
  background: rgba(47, 111, 237, .92);
  color: #fff;
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 999px;
}

.ritem .rname {
  font-size: 11px;
  line-height: 1.3;
  margin-top: 6px;
  color: var(--text);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  height: 29px;
}

</style>