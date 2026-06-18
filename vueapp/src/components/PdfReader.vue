<template>
  <div class="reader">
    <!-- 工具栏 -->
    <div class="reader-toolbar">
      <button class="btn" @click="close">← 返回</button>
      <span class="doc-title">{{ bookName }}</span>
      <div class="spacer"/>
      <span class="pageinfo">
        {{ historyPage + 1 }} / {{ total }}
      </span>
      <button class="btn" @click="zoomOut">－</button>
      <span class="zoom">{{ zoomLabel }}</span>
      <button class="btn" @click="zoomIn">＋</button>
    </div>
    <!-- 增加 ref，方便滚动定位 -->
    <PdfViewer
        ref="viewerRef"
        :pages="bookPages"
        :historyPage="historyPage"
        :historyFrac="historyFrac"
        :scale="scale"
        @scrollPageChange="scrollPageChange"
    />
  </div>
</template>
<script setup lang="ts">
import {ref, onMounted, computed, reactive} from 'vue'
import {useRoute, useRouter} from 'vue-router'
import PdfViewer from '@/components/PdfViewer.vue'
import type {PdfPageData} from '@/composables/DataBean'
import request from '@/utils/request'
import {throttle, debounce} from '@/utils/UIUtils'

if (typeof (Promise as any).withResolvers !== 'function') {
  (Promise as any).withResolvers = function <T>() {
    let resolve!: (value: T | PromiseLike<T>) => void
    let reject!: (reason?: any) => void
    const promise = new Promise<T>((res, rej) => {
      resolve = res
      reject = rej
    })
    return { promise, resolve, reject }
  }
}

import * as pdfjsLib from 'pdfjs-dist'

// Vite 原生标准写法，无需 ?url
const workerSrc = new URL('pdfjs-dist/build/pdf.worker.min.mjs', import.meta.url).href
pdfjsLib.GlobalWorkerOptions.workerSrc = workerSrc

const route = useRoute()
const router = useRouter()
const viewerRef = ref<InstanceType<typeof PdfViewer>>()
const bookId = ref<string>()
const bookName = ref('')
const bookPages = ref<PdfPageData[]>([])
const total = ref(0)
const historyPage = ref(0)
const historyFrac = ref(0)

// 缩放配置
const zoomLevels = [0.5, 0.75, 1, 1.25, 1.5, 2]
const zoomIdx = ref(2)
const scale = computed(() => zoomLevels[zoomIdx.value])
const zoomLabel = computed(() => `${Math.round(scale.value * 100)}%`)
const screenW = computed(() => {
  return viewerRef.value?.$el.clientWidth || 680
})

onMounted(async () => {
  bookId.value = route.params.bookId as string
  await loadMeta()
})

async function loadMeta() {
  const data = await request.get(`meta?id=${bookId.value}`)
  total.value = data.data.pageCount
  bookName.value = data.data.name
  historyPage.value = data.data.progress.page || 0
  historyFrac.value = data.data.progress.frac || 0
  zoomIdx.value = zoomLevels.indexOf(data.data.progress.scale || 1)

  const bookPagesTmp: (PdfPageData)[] = []
  for (let i = 0; i < data.data.pages.length; i++) {
    const pageSize = data.data.pages[i]
    bookPagesTmp.push({
      pageNum: i,
      width: pageSize.w,
      height: pageSize.h,
      page: async (pageNum) => {
        const url = `pagepdf?id=${bookId.value}&page=${pageNum}&size=1`
        const pdfBuffer = await request.get(url, {
          responseType: 'arraybuffer',
          timeout: 15000,
        })
        const pdf = await pdfjsLib.getDocument({data: pdfBuffer.data}).promise
        return pdf.getPage(1)
      },
      render: async (canvas, scale, page) => {
        const rate = Math.max(1, (screenW.value || 0) / (pageSize.w || 100000))
        const vp = page.getViewport({scale: scale * rate})
        canvas.width = vp.width
        canvas.height = vp.height
        const ctx = canvas.getContext('2d', {alpha: false})!
        // 避免画布残留内容导致重影/闪烁
        ctx.clearRect(0, 0, canvas.width, canvas.height)
        await page.render({canvasContext: ctx, viewport: vp}).promise
      }
    })
  }
  bookPages.value = bookPagesTmp
  console.log(bookPages)
}

const saveProgress = debounce(async (pageNum: number, fraction: number) => {
  if (historyPage.value == pageNum && historyFrac.value == fraction && zoomIdx.value == zoomLevels.indexOf(scale.value)) {
    return
  }
  historyPage.value = pageNum
  historyFrac.value = fraction
  zoomIdx.value = zoomLevels.indexOf(scale.value)
  await request.post(`progress?id=${bookId.value}`, {
    'page': pageNum,
    'frac': fraction,
    'name': bookName.value,
    'scale': scale.value,
    'totalPages': total.value,
    'percent': (pageNum / total.value * 100).toFixed(2),
  }, {
    headers: {"Content-Type": "application/json"}
  })
}, 500)

function scrollPageChange(param: any) {
  // pageNum: number, fraction: number
  console.log('scrollPageChange param:', param)
  saveProgress(param.pageNum, param.fraction)
}


async function zoomIn() {
  if (zoomIdx.value < zoomLevels.length - 1) zoomIdx.value++
}

function zoomOut() {
  if (zoomIdx.value > 0) zoomIdx.value--
}

function close() {
  router.back()
}
</script>

<style scoped>
.reader {
  height: 100vh;
  display: flex;
  flex-direction: column;
}

.reader-toolbar {
  flex: 0 0 46px;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 14px;
  background: var(--panel);
  border-bottom: 1px solid var(--border);
  overflow-x: auto;
  overflow-y: hidden;
  -webkit-overflow-scrolling: touch;
  scrollbar-width: none;
  -ms-overflow-style: none;
}

.reader-toolbar::-webkit-scrollbar {
  display: none;
  height: 0;
}

/* 横滑布局：所有控件不收缩，挤不下时整条工具栏可横向滑动 */
.reader-toolbar > * {
  flex: 0 0 auto;
}

.reader-toolbar .doc-title {
  font-size: 13px;
  font-weight: 500;
  max-width: 32vw;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.spacer {
  flex: 1;
}

.pageinfo {
  font-size: 14px;
}

.zoom {
  min-width: 40px;
  text-align: center;
}

.btn {
  border: none;
  background: none;
  font-size: 18px;
}
</style>