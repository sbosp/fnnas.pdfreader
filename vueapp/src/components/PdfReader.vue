<template>
  <div class="reader">
    <!-- 工具栏（手机端隐藏，改用页脚页码 + 双指缩放）-->
    <div class="reader-toolbar">
      <button class="btn btn-back" @click="close">← 返回</button>
      <span class="doc-title">{{ bookName }}</span>
      <div class="spacer"/>
      <span class="pageinfo">{{ currentPage + 1 }} / {{ total }}</span>
      <button class="btn" @click="zoomOut">－</button>
      <span class="zoom">{{ zoomLabel }}</span>
      <button class="btn" @click="zoomIn">＋</button>
    </div>

    <!-- 视口 -->
    <div class="image-viewport" ref="viewportRef" @scroll.passive="handleScroll">
      <!-- 内容轨道：宽度取最宽页面，放大后可左右拖动查看全部内容，缩小时居中 -->
      <div
          class="pages-track"
          ref="trackRef"
          :style="pinchVisualScale !== 1 ? {
            transform: `scale(${pinchVisualScale})`,
            transformOrigin: pinchOrigin,
          } : undefined"
      >
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
          <!-- canvas：pdf.js 矢量渲染，只有进入可视范围才真正下载切片并渲染 -->
          <canvas
              v-show="page.rendered"
              :ref="el => setCanvasRef(page.pageNum, el)"
              class="page-canvas"
          />
          <!-- 占位/加载/失败 -->
          <div v-if="!page.rendered && !page.error" class="ph-overlay">
            <div class="ph-icon">
              <svg viewBox="0 0 24 24" fill="none">
                <path d="M6 2h8l4 4v16H6z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
                <path d="M14 2v4h4" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
              </svg>
            </div>
            <div class="ph-text">
              {{ page.loading ? `加载中 ${page.pageNum + 1}...` : `第 ${page.pageNum + 1} 页` }}
            </div>
          </div>
          <div v-if="page.error" class="err-overlay" @click="retryPage(page.pageNum)">
            <div>加载失败，点击重试</div>
          </div>
        </div>
      </div>
    </div>

    <!-- 页脚页码标记（手机端顶部工具栏隐藏时的页码提示）-->
    <div class="page-footer">{{ currentPage + 1 }} / {{ total }}</div>
  </div>
</template>

<script setup lang="ts">
import {ref, onMounted, onUnmounted, computed, reactive, nextTick} from 'vue'
import {useRoute, useRouter} from 'vue-router'
import {request, download} from '@/utils/request'
import {debounce} from '@/utils/UIUtils'
import * as pdfjsLib from 'pdfjs-dist'
// @ts-ignore vite worker 导入
import PdfWorker from 'pdfjs-dist/build/pdf.worker.min.mjs?worker'

// pdf.js worker：用 Vite 的 ?worker 方式打包，随应用发布，不依赖外网 CDN
pdfjsLib.GlobalWorkerOptions.workerPort = new PdfWorker()

const route = useRoute()
const router = useRouter()
const viewportRef = ref<HTMLElement>()
const trackRef = ref<HTMLElement>()
// 双指临时缩放的 transform-origin（跟随捏合中心，观感更自然）
const pinchOrigin = ref('50% 0')

// ============ 基础状态 ============
const bookId = ref<string>('')
const bookName = ref('')
const total = ref(0)
const currentPage = ref(0)
// 进度恢复用：起始页 + 页内比例(0~1)，与设备/缩放无关
const startFrac = ref(0)

// ============ 缩放 ============
// scale 为连续值，按钮走档位、双指走连续缩放
const MIN_SCALE = 0.5
const MAX_SCALE = 3
const zoomLevels = [0.5, 0.75, 1, 1.25, 1.5, 2, 3]
const scale = ref(1)
// 双指手势进行中的「瞬时视觉倍率」：仅用 CSS transform 缩放内容轨道，
// 不改 div 布局尺寸、不改 canvas 位图、不触发懒加载/重渲染。
// 手势结束时才把它折算进真实 scale 并重排+重绘（矢量清晰）。=1 表示无临时缩放。
const pinchVisualScale = ref(1)
const zoomLabel = computed(() => `${Math.round(scale.value * pinchVisualScale.value * 100)}%`)

// ============ 常量配置 ============
const MAX_CONCURRENT = 3     // 最大并发加载数
const PRELOAD_AHEAD = 2      // 向下预加载页数
const PRELOAD_BEHIND = 1     // 向上预加载页数
const OBSERVER_ROOT_MARGIN = '400px 0px' // 提前触发加载的缓冲距离
const DEFAULT_PAGE_WIDTH = 595   // A4 默认宽（pt）
const DEFAULT_PAGE_HEIGHT = 842  // A4 默认高（pt）
// canvas 渲染上限：手机多为 3 倍屏，钳到 3 才够锐利（钳 2 会糊）。
// 更高的 dpr 收益极小却翻倍内存，故上限取 3。
const MAX_DPR = 3

// ============ 页面数据 ============
interface PageItem {
  pageNum: number
  // 原始 pt 尺寸（来自 meta）
  origWidth: number
  origHeight: number
  // 当前显示尺寸（受 scale 影响，CSS 像素）
  displayWidth: number
  displayHeight: number
  loading: boolean   // 正在下载/渲染
  rendered: boolean  // 已渲染到 canvas
  error: boolean
}

const pages = reactive<PageItem[]>([])

// canvas 元素引用（pageNum -> HTMLCanvasElement）
const canvasEls = new Map<number, HTMLCanvasElement>()
// 每页缓存的 pdf.js 文档代理（用于缩放时按新 viewport 重绘，不必重新下载）
const pageDocs = new Map<number, any>()
// 每页对应的 AbortController（用于取消下载）
const pageAborts = new Map<number, AbortController>()

function setCanvasRef(pageNum: number, el: any) {
  if (el) canvasEls.set(pageNum, el as HTMLCanvasElement)
  else canvasEls.delete(pageNum)
}

// ============ 并发调度器（下载切片 + pdf.js 渲染）============
class PageLoader {
  private queue: number[] = []          // 待加载的 pageNum 队列（按优先级）
  private running = new Set<number>()   // 正在加载的 pageNum
  private aborted = new Set<number>()   // 已被跳过的 pageNum

  request(pageNum: number, priority = false) {
    if (pageNum < 0 || pageNum >= pages.length) return
    const p = pages[pageNum]
    if (!p || p.rendered || p.loading) return
    if (this.running.has(pageNum)) return

    this.aborted.delete(pageNum)

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

  cancel(pageNum: number) {
    const idx = this.queue.indexOf(pageNum)
    if (idx >= 0) {
      this.queue.splice(idx, 1)
      this.aborted.add(pageNum)
    }
  }

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
      if (!p || p.rendered || p.loading) continue

      this.running.add(pageNum)
      p.loading = true
      loadAndRender(pageNum)
        .then(() => this.markDone(pageNum))
        .catch(() => this.markError(pageNum))
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

const loader = new PageLoader()

// ============ URL 构建 ============
function buildPageUrl(pageNum: number): string {
  // 走 request 的 baseURL（相对路径），交给并发下载调度器
  // 页码对外统一 0-based（第一页 = 0），与 Rust 服务端约定一致
  return `pagepdf?id=${encodeURIComponent(bookId.value)}&page=${pageNum}`
}

// ============ 下载切片 + pdf.js 渲染 ============
async function loadAndRender(pageNum: number): Promise<void> {
  const p = pages[pageNum]
  if (!p) return

  // 1) 下载单页 PDF 切片（ArrayBuffer），复用 request.ts 的并发下载调度器
  const ac = new AbortController()
  pageAborts.set(pageNum, ac)
  let buf: ArrayBuffer
  try {
    buf = await download(buildPageUrl(pageNum), ac.signal)
  } finally {
    pageAborts.delete(pageNum)
  }

  // 2) pdf.js 解析（切片只有 1 页，取第 1 页）
  // 复制一份，避免 pdf.js 持有可能被复用的 buffer
  const data = buf.slice(0)
  const doc = await pdfjsLib.getDocument({data, disableAutoFetch: true, disableStream: true}).promise
  pageDocs.set(pageNum, doc)

  // 3) 渲染到 canvas
  await renderPageCanvas(pageNum)

  p.loading = false
  p.error = false
  p.rendered = true
}

// 把某页按当前 scale 渲染/重绘到它的 canvas
async function renderPageCanvas(pageNum: number): Promise<void> {
  const doc = pageDocs.get(pageNum)
  const canvas = canvasEls.get(pageNum)
  const p = pages[pageNum]
  if (!doc || !canvas || !p) return

  const pdfPage = await doc.getPage(1)
  const dpr = Math.min(window.devicePixelRatio || 1, MAX_DPR)
  const baseViewport = pdfPage.getViewport({scale: 1})

  // 关键修复：以 pdf.js 实际渲染尺寸为准校正该页比例。
  // meta 返回的是 MediaBox，而 pdf.js 渲染用 CropBox；若 meta 拿不到/超时还会
  // 回退成 A4 默认值。任一情况都会让占位 div 的宽高比与真实位图对不上，
  // canvas 被 CSS 撑满 div 后就被拉伸变形。这里用 baseViewport 的真实宽高
  // 重写本页原始尺寸并重算显示尺寸，使 div 比例 == 位图比例，从根上消除变形。
  if (baseViewport.width > 0 && baseViewport.height > 0) {
    const realRatio = baseViewport.height / baseViewport.width
    const curRatio = p.origHeight / p.origWidth
    if (!isFinite(curRatio) || Math.abs(realRatio - curRatio) > 0.003) {
      p.origWidth = baseViewport.width
      p.origHeight = baseViewport.height
      computeDisplaySize(p) // 按真实比例重算 displayWidth / displayHeight
    }
  }

  // displayWidth 为 CSS 像素，viewport scale = (显示宽 / PDF 点宽) × dpr
  const cssWidth = p.displayWidth
  const renderScale = (cssWidth / baseViewport.width) * dpr
  const viewport = pdfPage.getViewport({scale: renderScale})

  const ctx = canvas.getContext('2d')
  if (!ctx) return
  canvas.width = Math.floor(viewport.width)
  canvas.height = Math.floor(viewport.height)
  canvas.style.width = '100%'
  canvas.style.height = '100%'

  await pdfPage.render({canvasContext: ctx, viewport}).promise
  // 释放该 page 的中间对象（保留 doc，供缩放重绘）
  pdfPage.cleanup()
}

// ============ 回收：滚出视区的页销毁 canvas 内容 + pdf.js 文档 ============
function recyclePage(pageNum: number) {
  const p = pages[pageNum]
  if (!p) return
  // 取消尚在下载的请求
  const ac = pageAborts.get(pageNum)
  if (ac) {
    ac.abort()
    pageAborts.delete(pageNum)
  }
  // 销毁 pdf.js 文档，释放内存
  const doc = pageDocs.get(pageNum)
  if (doc) {
    try { doc.destroy() } catch { /* ignore */ }
    pageDocs.delete(pageNum)
  }
  // 清空 canvas 位图
  const canvas = canvasEls.get(pageNum)
  if (canvas) {
    canvas.width = 0
    canvas.height = 0
  }
  p.rendered = false
  p.loading = false
  p.error = false
}

// ============ 回调 ============
function retryPage(pageNum: number) {
  const p = pages[pageNum]
  if (!p) return
  p.error = false
  p.rendered = false
  p.loading = false
  loader.request(pageNum, true)
}

// ============ IntersectionObserver 懒加载 ============
let io: IntersectionObserver | null = null

function setupObserver() {
  if (!viewportRef.value) return
  io = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      const pn = parseInt((entry.target as HTMLElement).dataset.pageNum || '-1', 10)
      if (pn < 0) continue
      if (entry.isIntersecting) {
        loader.request(pn, true)
        for (let k = 1; k <= PRELOAD_AHEAD; k++) loader.request(pn + k, false)
        for (let k = 1; k <= PRELOAD_BEHIND; k++) loader.request(pn - k, false)
      }
    }
  }, {
    root: viewportRef.value,
    rootMargin: OBSERVER_ROOT_MARGIN,
    threshold: 0.01,
  })
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
// 100% 缩放(scale=1)的定义：页面宽度铺满当前视口可用宽度，
// 高度按该页真实宽高比(origHeight/origWidth)自适应算出。
// scale>1 时在此基础上等比放大(可左右拖动看全内容)，scale<1 时缩小居中。
function computeDisplaySize(p: PageItem) {
  // 视口可用宽度 = 容器宽度 - 轨道左右内边距(12px×2)
  const vw = (viewportRef.value?.clientWidth || 800) - 24
  const ratio = p.origHeight / p.origWidth
  p.displayWidth = Math.round(vw * scale.value)
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

  let acc = 0
  for (let i = 0; i < pages.length; i++) {
    const h = pages[i].displayHeight + 20 // 20 = margin-bottom
    if (viewCenter >= acc && viewCenter < acc + h) {
      currentPage.value = i
      const inPageFrac = Math.min(1, Math.max(0, (viewCenter - acc) / h))
      saveProgress(i, inPageFrac)
      return
    }
    acc += h
  }
}

// 快速滚动时，取消/回收远离视口的页
const scheduleCancelInvisible = debounce(() => {
  const vp = viewportRef.value
  if (!vp) return
  const scrollTop = vp.scrollTop
  const viewBottom = scrollTop + vp.clientHeight

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
  // 回收保留范围之外、已渲染的页，释放 pdf.js 内存（手机端翻几十页不 OOM）
  for (const p of pages) {
    if (!keep.has(p.pageNum) && (p.rendered || pageDocs.has(p.pageNum))) {
      recyclePage(p.pageNum)
    }
  }
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
    startFrac.value = typeof data.progress?.frac === 'number' ? data.progress.frac : 0

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
        loading: false,
        rendered: false,
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
    setupObserver()
    scrollToPage(currentPage.value, startFrac.value)
  } catch (e) {
    console.error('加载文档元数据失败', e)
  }
}

function scrollToPage(pageNum: number, frac = 0) {
  const vp = viewportRef.value
  if (!vp) return
  let top = 0
  for (let i = 0; i < pageNum && i < pages.length; i++) {
    top += pages[i].displayHeight + 20
  }
  const h = (pages[pageNum]?.displayHeight || 0) + 20
  top += h * frac - vp.clientHeight / 2
  vp.scrollTop = Math.max(0, top)
}

// ============ 缩放 ============
// 重绘节流：缩放停止后再按新 scale 重渲染可视页 canvas（矢量始终清晰）
const rerenderVisible = debounce(() => {
  for (const [pageNum] of pageDocs) {
    renderPageCanvas(pageNum).catch(() => { /* ignore */ })
  }
}, 200)

function applyZoom(newScale: number, anchorClientY?: number) {
  newScale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, newScale))
  const vp = viewportRef.value
  if (!vp) {
    scale.value = newScale
    recomputeAllSizes()
    return
  }

  const anchorY = anchorClientY ?? vp.clientHeight / 2
  const scrollTop = vp.scrollTop
  const focus = scrollTop + anchorY

  let acc = 0
  let anchorPage = 0
  let anchorRatio = 0
  for (let i = 0; i < pages.length; i++) {
    const h = pages[i].displayHeight + 20
    if (focus >= acc && focus < acc + h) {
      anchorPage = i
      anchorRatio = (focus - acc) / h
      break
    }
    acc += h
  }

  scale.value = newScale
  recomputeAllSizes()

  nextTick(() => {
    let top = 0
    for (let i = 0; i < anchorPage && i < pages.length; i++) {
      top += pages[i].displayHeight + 20
    }
    top += ((pages[anchorPage]?.displayHeight || 0) + 20) * anchorRatio
    vp.scrollTop = Math.max(0, top - anchorY)
  })
  // 按新 scale 重绘已加载页的 canvas，保证矢量清晰不发虚
  rerenderVisible()
}

function zoomIn() {
  const next = zoomLevels.find(z => z > scale.value + 1e-6)
  if (next !== undefined) applyZoom(next)
}

function zoomOut() {
  const prev = [...zoomLevels].reverse().find(z => z < scale.value - 1e-6)
  if (prev !== undefined) applyZoom(prev)
}

// ============ 双指缩放手势 ============
// 设计：手势进行中「只做 CSS transform 视觉缩放」，绝不改 div 布局尺寸、
// 不改 canvas 位图、不触发 IntersectionObserver、不重新下载/渲染——因此
// 放大过程丝滑、绝无「重新加载」。手势结束(onTouchEnd)才把视觉倍率一次性
// 折算进真实 scale，做一次真实重排 + 按新 scale 重绘(矢量清晰)。
let pinchStartDist = 0
let pinchStartScale = 1
let pinchAnchorY = 0     // 捏合中心相对视口顶部的 Y（用于结束时的锚点定位）
let pinching = false

function touchDist(t0: Touch, t1: Touch) {
  const dx = t0.clientX - t1.clientX
  const dy = t0.clientY - t1.clientY
  return Math.hypot(dx, dy)
}

function onTouchStart(e: TouchEvent) {
  if (e.touches.length === 2) {
    pinching = true
    pinchStartDist = touchDist(e.touches[0], e.touches[1])
    pinchStartScale = scale.value
    const vp = viewportRef.value
    const rect = vp?.getBoundingClientRect()
    const midX = (e.touches[0].clientX + e.touches[1].clientX) / 2
    const midY = (e.touches[0].clientY + e.touches[1].clientY) / 2
    pinchAnchorY = rect ? midY - rect.top : midY
    // transform-origin 用「捏合中心在轨道坐标系里的位置」，缩放围绕手指中心展开。
    if (vp && rect) {
      const originX = midX - rect.left + vp.scrollLeft
      const originY = midY - rect.top + vp.scrollTop
      pinchOrigin.value = `${originX}px ${originY}px`
    }
    pinchVisualScale.value = 1
  }
}

function onTouchMove(e: TouchEvent) {
  if (!pinching || e.touches.length !== 2) return
  e.preventDefault()
  const dist = touchDist(e.touches[0], e.touches[1])
  if (pinchStartDist <= 0) return
  // 仅更新视觉倍率（CSS transform），并把最终真实倍率钳到 [MIN,MAX] 之内，
  // 避免手势结束后回弹。ratio = 手指张合比例。
  const ratio = dist / pinchStartDist
  const clampedFinal = Math.min(MAX_SCALE, Math.max(MIN_SCALE, pinchStartScale * ratio))
  pinchVisualScale.value = clampedFinal / pinchStartScale
}

function onTouchEnd(e: TouchEvent) {
  // 仍有 ≥2 指按住则不结束
  if (e.touches.length >= 2) return
  if (!pinching) return
  pinching = false

  const vis = pinchVisualScale.value
  if (Math.abs(vis - 1) < 1e-3) {
    // 几乎没缩放，直接复位
    pinchVisualScale.value = 1
    return
  }
  // 把视觉倍率折算进真实 scale，做一次真实重排 + 重绘（此时才会重新按新
  // scale 渲染矢量，清晰且不发虚）。用捏合中心作为锚点保持视觉位置稳定。
  const target = pinchStartScale * vis
  commitPinchZoom(target)
}

// 手势结束时提交缩放：清掉临时 transform，按新 scale 真实重排并锚定捏合中心。
function commitPinchZoom(newScale: number) {
  newScale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, newScale))
  const vp = viewportRef.value
  // 先记录捏合中心当前对应的「内容焦点」(在旧 transform 下的真实滚动坐标)。
  // 旧视觉倍率 vis 下，轨道内容被以 pinchOrigin 为原点放大了 vis 倍，
  // 视口里 pinchAnchorY 处对应的内容点 = origin + (anchor在视口的绝对Y - origin)/vis。
  let focusPage = currentPage.value
  let focusRatio = 0
  if (vp) {
    const vis = pinchVisualScale.value
    // 捏合中心在「未缩放内容坐标系」中的 Y
    const originY = parseFloat(pinchOrigin.value.split(' ')[1]) || 0
    const absYInViewport = pinchAnchorY // 相对视口顶部
    const contentYVisual = vp.scrollTop + absYInViewport // transform 后视觉坐标
    const contentY = originY + (contentYVisual - originY) / vis // 反解到未缩放坐标
    let acc = 0
    for (let i = 0; i < pages.length; i++) {
      const h = pages[i].displayHeight + 20
      if (contentY >= acc && contentY < acc + h) {
        focusPage = i
        focusRatio = (contentY - acc) / h
        break
      }
      acc += h
    }
  }

  // 清掉临时视觉缩放，切到真实 scale 并重排
  pinchVisualScale.value = 1
  scale.value = newScale
  recomputeAllSizes()

  nextTick(() => {
    if (vp) {
      let top = 0
      for (let i = 0; i < focusPage && i < pages.length; i++) top += pages[i].displayHeight + 20
      top += ((pages[focusPage]?.displayHeight || 0) + 20) * focusRatio
      vp.scrollTop = Math.max(0, top - pinchAnchorY)
    }
    // 按新 scale 重绘已加载页 canvas，矢量重新光栅化 → 放大后依旧锐利
    rerenderVisible()
  })
}

// ============ 窗口尺寸变化：宽度绑定视口，需重算并重绘 ============
const handleResize = debounce(() => {
  if (!pages.length) return
  // 记录当前锚点页与页内比例，重算后滚回原位，避免跳动
  const vp = viewportRef.value
  let anchorPage = currentPage.value
  let anchorRatio = 0
  if (vp) {
    const focus = vp.scrollTop + vp.clientHeight / 2
    let acc = 0
    for (let i = 0; i < pages.length; i++) {
      const h = pages[i].displayHeight + 20
      if (focus >= acc && focus < acc + h) {
        anchorPage = i
        anchorRatio = (focus - acc) / h
        break
      }
      acc += h
    }
  }
  recomputeAllSizes()
  nextTick(() => {
    if (vp) {
      let top = 0
      for (let i = 0; i < anchorPage && i < pages.length; i++) top += pages[i].displayHeight + 20
      top += ((pages[anchorPage]?.displayHeight || 0) + 20) * anchorRatio
      vp.scrollTop = Math.max(0, top - vp.clientHeight / 2)
    }
    rerenderVisible()
  })
}, 200)

// ============ 生命周期 ============
onMounted(async () => {
  bookId.value = route.params.bookId as string
  await loadMeta()
  const vp = viewportRef.value
  if (vp) {
    vp.addEventListener('touchstart', onTouchStart, {passive: true})
    vp.addEventListener('touchmove', onTouchMove, {passive: false})
    vp.addEventListener('touchend', onTouchEnd, {passive: true})
    vp.addEventListener('touchcancel', onTouchEnd, {passive: true})
  }
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  const vp = viewportRef.value
  if (vp) {
    vp.removeEventListener('touchstart', onTouchStart)
    vp.removeEventListener('touchmove', onTouchMove)
    vp.removeEventListener('touchend', onTouchEnd)
    vp.removeEventListener('touchcancel', onTouchEnd)
  }
  window.removeEventListener('resize', handleResize)
  teardownObserver()
  loader.clear()
  // 释放所有 pdf.js 文档
  for (const [pn] of pageDocs) recyclePage(pn)
  pageDocs.clear()
  canvasEls.clear()
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
  overflow-x: auto;
  position: relative;
  background: #f5f5f5;
  padding: 12px 0;
  -webkit-overflow-scrolling: touch;
  touch-action: pan-x pan-y;
  overscroll-behavior: contain;
}

.pages-track {
  display: flex;
  flex-direction: column;
  align-items: center;
  width: max-content;
  min-width: 100%;
  box-sizing: border-box;
  padding: 0 12px;
  /* 双指临时缩放走 CSS transform：提示浏览器用 GPU 合成层，缩放跟手流畅；
     手势结束清掉 transform 后即回到普通布局，不残留合成开销。 */
  will-change: transform;
}

.image-page {
  position: relative;
  background: #fff;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);
  margin: 0 0 20px;
  overflow: hidden;
  contain: layout paint;
  flex: 0 0 auto;
}

.page-canvas {
  width: 100%;
  height: 100%;
  display: block;
  /* 兜底防拉伸：即使 div 比例与位图有瞬时偏差，也按位图比例缩放留白边，绝不变形 */
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

.page-footer {
  display: none;
  position: fixed;
  left: 50%;
  bottom: 12px;
  transform: translateX(-50%);
  padding: 4px 12px;
  border-radius: 999px;
  background: rgba(0, 0, 0, 0.55);
  color: #fff;
  font-size: 12px;
  line-height: 1;
  z-index: 20;
  pointer-events: none;
  backdrop-filter: blur(4px);
}

/* ============ 手机端适配 ============ */
@media (max-width: 640px) {
  .reader-toolbar {
    display: none;
  }

  .page-footer {
    display: block;
  }
}
</style>
