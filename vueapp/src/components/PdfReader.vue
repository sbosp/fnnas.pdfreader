<template>
  <div class="reader">
    <!-- 工具栏 -->
    <div class="reader-toolbar">
      <button class="btn" @click="close">← 返回</button>
      <span class="doc-title">{{ bookName }}</span>
      <div class="spacer"/>
      <span class="pageinfo">{{ currentPage + 1 }} / {{ total }}</span>
      <button class="btn" @click="zoomOut">－</button>
      <span class="zoom">{{ zoomLabel }}</span>
      <button class="btn" @click="zoomIn">＋</button>
    </div>

    <!-- 视口 -->
    <div class="image-viewport" ref="viewportRef" @scroll.passive="handleScroll">
      <div
          v-for="page in pages"
          :key="page.pageNum"
          class="image-page"
          :data-page-num="page.pageNum"
          :style="{
            width: page.displayWidth + 'px',
            height: page.displayHeight + 'px',
          }"
      >
        <!-- 图片：只有 src 被赋值后才真正请求 -->
        <img
            v-if="page.src"
            :src="page.src"
            :alt="'第' + (page.pageNum + 1) + '页'"
            class="page-img"
            @load="onImageLoad(page.pageNum)"
            @error="onImageError(page.pageNum)"
        />
        <!-- 占位/加载/失败 -->
        <div v-if="!page.loaded && !page.error" class="ph-overlay">
          <div class="ph-icon">
            <svg viewBox="0 0 24 24" fill="none">
              <path d="M6 2h8l4 4v16H6z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
              <path d="M14 2v4h4" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
            </svg>
          </div>
          <div class="ph-text">
            {{ page.src ? `加载中 ${page.pageNum + 1}...` : `第 ${page.pageNum + 1} 页` }}
          </div>
        </div>
        <div v-if="page.error" class="err-overlay" @click="retryPage(page.pageNum)">
          <div>加载失败，点击重试</div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import {ref, onMounted, onUnmounted, computed, reactive, nextTick} from 'vue'
import {useRoute, useRouter} from 'vue-router'
import {request} from '@/utils/request'
import {debounce} from '@/utils/UIUtils'

const route = useRoute()
const router = useRouter()
const viewportRef = ref<HTMLElement>()

// ============ 基础状态 ============
const bookId = ref<string>('')
const bookName = ref('')
const total = ref(0)
const currentPage = ref(0)

// ============ 缩放 ============
const zoomLevels = [0.5, 0.75, 1, 1.25, 1.5, 2]
const zoomIdx = ref(2)
const scale = computed(() => zoomLevels[zoomIdx.value])
const zoomLabel = computed(() => `${Math.round(scale.value * 100)}%`)

// ============ 常量配置 ============
const DPI = 300              // 图片渲染 DPI
const MAX_CONCURRENT = 3     // 最大并发加载数
const PRELOAD_AHEAD = 2      // 向下预加载页数
const PRELOAD_BEHIND = 1     // 向上预加载页数
const OBSERVER_ROOT_MARGIN = '400px 0px' // 提前触发加载的缓冲距离
const DEFAULT_PAGE_WIDTH = 595   // A4 默认宽（pt）
const DEFAULT_PAGE_HEIGHT = 842  // A4 默认高（pt）
const VIEWPORT_WIDTH_LIMIT = 900 // 图片显示的最大宽度（保持阅读舒适）

// ============ 页面数据 ============
interface PageItem {
  pageNum: number
  // 原始 pt 尺寸（来自 meta）
  origWidth: number
  origHeight: number
  // 当前显示尺寸（受 scale 影响）
  displayWidth: number
  displayHeight: number
  // 图片 src（未加载时为空）
  src: string
  loaded: boolean
  error: boolean
}

const pages = reactive<PageItem[]>([])

// ============ 并发调度器 ============
class ImageLoader {
  private queue: number[] = []          // 待加载的 pageNum 队列（按优先级）
  private running = new Set<number>()   // 正在加载的 pageNum
  private aborted = new Set<number>()   // 已被跳过的 pageNum

  // 请求加载某一页；priority=true 会插到队首
  request(pageNum: number, priority = false) {
    if (pageNum < 0 || pageNum >= pages.length) return
    const p = pages[pageNum]
    if (!p || p.src || p.loaded) return
    if (this.running.has(pageNum)) return

    this.aborted.delete(pageNum)

    // 已在队列
    const idx = this.queue.indexOf(pageNum)
    if (idx >= 0) {
      if (priority) {
        this.queue.splice(idx, 1)
        this.queue.unshift(pageNum)
      }
      return
    }

    if (priority) this.queue.unshift(pageNum)
    else this.queue.push(pageNum)

    this.tick()
  }

  // 取消尚未开始的请求（滚动出可视范围时清理低优先级任务）
  cancel(pageNum: number) {
    const idx = this.queue.indexOf(pageNum)
    if (idx >= 0) {
      this.queue.splice(idx, 1)
      this.aborted.add(pageNum)
    }
  }

  // 只保留在候选集合内的排队任务，其他的取消掉
  keepOnly(keepSet: Set<number>) {
    this.queue = this.queue.filter(pn => {
      if (keepSet.has(pn)) return true
      this.aborted.add(pn)
      return false
    })
  }

  private tick() {
    while (this.running.size < MAX_CONCURRENT && this.queue.length > 0) {
      const pageNum = this.queue.shift()!
      if (this.aborted.has(pageNum)) continue
      const p = pages[pageNum]
      if (!p || p.loaded || p.src) continue

      this.running.add(pageNum)
      // 直接给 img.src 赋值，交给浏览器加载
      // 加时间戳？不需要，浏览器缓存对同 URL 有利
      p.src = buildPageUrl(pageNum)
      // 注意：加载完成/失败会通过 @load / @error 回调 markDone / markError
    }
  }

  markDone(pageNum: number) {
    this.running.delete(pageNum)
    this.tick()
  }

  markError(pageNum: number) {
    this.running.delete(pageNum)
    this.tick()
  }

  clear() {
    this.queue = []
    this.running.clear()
    this.aborted.clear()
  }
}

const loader = new ImageLoader()

// ============ URL 构建 ============
function buildPageUrl(pageNum: number): string {
  // 使用 request 实例的 baseURL：/app/fnnas-pdfreader/api/
  // 这里直接构造完整路径给浏览器加载，避免走 axios（浏览器原生处理更快）
  const base = (request.defaults.baseURL || '').replace(/\/+$/, '/')
  // pageNum 后端从 0 开始
  return `${base}page?id=${encodeURIComponent(bookId.value)}&page=${pageNum}&dpi=${DPI}`
}

// ============ 图片回调 ============
function onImageLoad(pageNum: number) {
  const p = pages[pageNum]
  if (!p) return
  p.loaded = true
  p.error = false
  loader.markDone(pageNum)
}

function onImageError(pageNum: number) {
  const p = pages[pageNum]
  if (!p) return
  p.error = true
  p.loaded = false
  p.src = ''
  loader.markError(pageNum)
}

function retryPage(pageNum: number) {
  const p = pages[pageNum]
  if (!p) return
  p.error = false
  p.src = ''
  loader.request(pageNum, true)
}

// ============ IntersectionObserver 懒加载 ============
let io: IntersectionObserver | null = null

function setupObserver() {
  if (!viewportRef.value) return
  io = new IntersectionObserver((entries) => {
    // 收集当前可见的页码
    for (const entry of entries) {
      const pn = parseInt((entry.target as HTMLElement).dataset.pageNum || '-1', 10)
      if (pn < 0) continue
      if (entry.isIntersecting) {
        // 可见 -> 高优先级加载
        loader.request(pn, true)
        // 顺带预加载前后 N 页
        for (let k = 1; k <= PRELOAD_AHEAD; k++) loader.request(pn + k, false)
        for (let k = 1; k <= PRELOAD_BEHIND; k++) loader.request(pn - k, false)
      }
    }
  }, {
    root: viewportRef.value,
    rootMargin: OBSERVER_ROOT_MARGIN,
    threshold: 0.01,
  })

  // 监听所有页面元素
  const els = viewportRef.value.querySelectorAll('.image-page')
  els.forEach(el => io!.observe(el))
}

function teardownObserver() {
  if (io) {
    io.disconnect()
    io = null
  }
}

// ============ 尺寸计算 ============
function computeDisplaySize(p: PageItem) {
  // 页面按容器宽度自适应，同时限制最大宽，再乘以 scale
  const vw = Math.min(
      (viewportRef.value?.clientWidth || 800) - 24,
      VIEWPORT_WIDTH_LIMIT
  )
  const ratio = p.origHeight / p.origWidth
  const baseWidth = Math.min(vw, p.origWidth * 1.2)
  p.displayWidth = Math.round(baseWidth * scale.value)
  p.displayHeight = Math.round(p.displayWidth * ratio)
}

function recomputeAllSizes() {
  for (const p of pages) computeDisplaySize(p)
}

// ============ 滚动处理 ============
const handleScroll = () => {
  updateCurrentPageFromScroll()
  scheduleCancelInvisible()
}

function updateCurrentPageFromScroll() {
  const vp = viewportRef.value
  if (!vp) return
  const scrollTop = vp.scrollTop
  const viewCenter = scrollTop + vp.clientHeight / 2

  // 二分查找当前中心所在的页
  let acc = 0
  for (let i = 0; i < pages.length; i++) {
    const h = pages[i].displayHeight + 20 // 20 = margin-bottom
    if (viewCenter >= acc && viewCenter < acc + h) {
      if (currentPage.value !== i) {
        currentPage.value = i
        saveProgress(i, i / Math.max(1, total.value - 1))
      }
      return
    }
    acc += h
  }
}

// 快速滚动时，取消远离视口的排队任务
const scheduleCancelInvisible = debounce(() => {
  const vp = viewportRef.value
  if (!vp) return
  const scrollTop = vp.scrollTop
  const viewBottom = scrollTop + vp.clientHeight

  // 计算可视附近范围（含预加载缓冲）
  const keep = new Set<number>()
  let acc = 0
  const buffer = vp.clientHeight * 2 // 上下2屏范围内保留
  for (let i = 0; i < pages.length; i++) {
    const h = pages[i].displayHeight + 20
    const top = acc
    const bottom = acc + h
    if (bottom >= scrollTop - buffer && top <= viewBottom + buffer) {
      keep.add(i)
    }
    acc += h
  }
  loader.keepOnly(keep)
}, 200)

// ============ 进度保存 ============
const saveProgress = debounce(async (pageNum: number, fraction: number) => {
  if (!bookId.value) return
  try {
    await request.post(`progress?id=${bookId.value}`, {
      page: pageNum,
      frac: fraction,
      name: bookName.value,
      scale: scale.value,
      totalPages: total.value,
      percent: ((pageNum + 1) / total.value * 100).toFixed(2),
    }, {headers: {'Content-Type': 'application/json'}})
  } catch (e) {
    console.warn('保存进度失败', e)
  }
}, 800)

// ============ 元数据加载 ============
async function loadMeta() {
  try {
    const response = await request.get(`meta?id=${bookId.value}`)
    const data = response.data

    total.value = data.pageCount || 0
    bookName.value = data.name || ''
    currentPage.value = data.progress?.page || 0

    // 初始化 pages 数组
    pages.length = 0
    const metaPages: Array<{ w: number; h: number }> = data.pages || []
    for (let i = 0; i < total.value; i++) {
      const size = metaPages[i]
      const item: PageItem = {
        pageNum: i,
        origWidth: size?.w || DEFAULT_PAGE_WIDTH,
        origHeight: size?.h || DEFAULT_PAGE_HEIGHT,
        displayWidth: 0,
        displayHeight: 0,
        src: '',
        loaded: false,
        error: false,
      }
      computeDisplaySize(item)
      pages.push(item)
    }

    console.log('📚 PDF 阅读器已初始化', {
      文档: bookName.value,
      总页数: total.value,
      起始页: currentPage.value + 1,
    })

    await nextTick()

    // 挂载 IntersectionObserver
    setupObserver()

    // 滚动到起始页
    scrollToPage(currentPage.value)
  } catch (e) {
    console.error('加载文档元数据失败', e)
  }
}

// 滚动到指定页
function scrollToPage(pageNum: number) {
  const vp = viewportRef.value
  if (!vp) return
  let top = 0
  for (let i = 0; i < pageNum && i < pages.length; i++) {
    top += pages[i].displayHeight + 20
  }
  vp.scrollTop = top
}

// ============ 缩放 ============
// 缩放时保持当前页在视口位置：记录中心页的相对位置，缩放后滚动到相同位置
function applyZoom(newIdx: number) {
  const vp = viewportRef.value
  if (!vp) {
    zoomIdx.value = newIdx
    recomputeAllSizes()
    return
  }

  // 记录当前视口中心相对于当前页顶部的比例
  const scrollTop = vp.scrollTop
  const viewCenter = scrollTop + vp.clientHeight / 2
  let acc = 0
  let anchorPage = 0
  let anchorRatio = 0
  for (let i = 0; i < pages.length; i++) {
    const h = pages[i].displayHeight + 20
    if (viewCenter >= acc && viewCenter < acc + h) {
      anchorPage = i
      anchorRatio = (viewCenter - acc) / h
      break
    }
    acc += h
  }

  // 应用新缩放（只改显示尺寸，不重设 img.src，浏览器不会重新请求）
  zoomIdx.value = newIdx
  recomputeAllSizes()

  // 恢复到原来的相对位置
  nextTick(() => {
    let top = 0
    for (let i = 0; i < anchorPage && i < pages.length; i++) {
      top += pages[i].displayHeight + 20
    }
    top += (pages[anchorPage]?.displayHeight || 0) * anchorRatio
    vp.scrollTop = top - vp.clientHeight / 2
  })
}

function zoomIn() {
  if (zoomIdx.value < zoomLevels.length - 1) {
    applyZoom(zoomIdx.value + 1)
  }
}

function zoomOut() {
  if (zoomIdx.value > 0) {
    applyZoom(zoomIdx.value - 1)
  }
}

// ============ 生命周期 ============
onMounted(async () => {
  bookId.value = route.params.bookId as string
  await loadMeta()
})

onUnmounted(() => {
  teardownObserver()
  loader.clear()
  pages.length = 0
})

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

.image-viewport {
  flex: 1;
  overflow-y: auto;
  overflow-x: hidden;
  position: relative;
  background: #f5f5f5;
  padding: 12px 0;
}

.image-page {
  position: relative;
  background: #fff;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);
  margin: 0 auto 20px;
  overflow: hidden;
  contain: layout paint;
}

.page-img {
  width: 100%;
  height: 100%;
  display: block;
  object-fit: contain;
}

.ph-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: #b3bccb;
  background: linear-gradient(135deg, #f4f7fc, #e9eef7);
  pointer-events: none;
}

.ph-icon svg {
  width: 42px;
  height: 42px;
}

.ph-text {
  font-size: 12px;
  color: #99a3b3;
}

.err-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(255, 255, 255, 0.9);
  font-size: 13px;
  color: #f56c6c;
  cursor: pointer;
}
</style>