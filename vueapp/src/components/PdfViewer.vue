<template>
  <div ref="viewport" class="pdf-viewport" @scroll="onScroll">
    <div
        v-for="page in pages"
        :key="page.pageNum"
        :data-pagenum="page.pageNum"
        class="pdf-page"
        :style="{
          height: pageRealHeight[page.pageNum] + 'px',
          width: pageRealWidth[page.pageNum] + 'px',
        }"
    >
      <canvas
          :ref="el => setCanvasRef(el, page.pageNum)"
          class="pdf-canvas"
      />
    </div>
  </div>
</template>

<script setup lang="ts">
import {ref, computed, watch, nextTick} from 'vue'
import type {PdfPageData} from '@/composables/DataBean'
import {throttle, debounce} from '@/utils/UIUtils'

const props = defineProps<{
  pages: PdfPageData[]
  historyPage: number
  historyFrac: number
  scale: number
}>()

const emit = defineEmits<{
  (e: 'scrollPageChange', data: { pageNum: number, fraction: number }): void
}>()

const viewport = ref<HTMLElement>()
const canvasMap = ref<Record<number, HTMLCanvasElement | null>>({})
const screenW = computed(() => {
  return viewport.value?.clientWidth || 680
})
const pageRealWidth = computed(() => {
  const res: Record<number, number> = {}
  const s = props.scale ?? 1
  props.pages.forEach(p => {
    res[p.pageNum] = Math.round(screenW.value * s)
  })
  return res
})

const pageRealHeight = computed(() => {
  const res: Record<number, number> = {}
  const s = props.scale ?? 1
  props.pages.forEach(p => {
    res[p.pageNum] = Math.round(screenW.value / p.width * p.height * s)
  })
  return res
})

const visiblePageNums = ref<Set<number>>(new Set())
const renderedCache = ref<Set<number>>(new Set())
// 改造1：渲染锁（标记正在渲染的页面，避免并发重复渲染）
const renderingLock = ref<Set<number>>(new Set())
// 改造2：渲染任务控制器（用于取消过期任务）
const renderAbortControllers = ref<Record<number, AbortController>>({})

function setCanvasRef(el: any | null, pageNum: number) {
  if (el instanceof HTMLCanvasElement) {
    canvasMap.value[pageNum] = el
  } else {
    canvasMap.value[pageNum] = null
  }
}

// 优化节流参数：减少滚动计算延迟（默认 16ms 对应 60fps）
const calcVisiblePages = throttle(() => {
  const view = viewport.value
  if (!view) return

  const {scrollTop, clientHeight} = view

  // 上下缓冲区：可视区域外额外预加载 1.5 屏
  const buffer = clientHeight * 1.5
  // 滚动范围：顶部 -缓冲区 ~ 底部 +缓冲区
  const viewTop = scrollTop - buffer
  const viewBottom = scrollTop + clientHeight + buffer

  const newVisible = new Set<number>()

  let currentPage = 1
  let scrollFraction = 0
  const pageDomList = Array.from(view.querySelectorAll('.pdf-page')) as HTMLDivElement[]
  pageDomList.forEach((dom) => {
    const pageNum = Number(dom.dataset.pagenum)
    const domTop = dom.offsetTop
    const domHeight = dom.offsetHeight
    const domBottom = domTop + domHeight

    // 判断元素是否在 可视区+缓冲区 内
    if (domBottom > viewTop && domTop < viewBottom) {
      newVisible.add(pageNum)
    }

    // 计算当前视口中心点所在页面 + 页内滚动比例
    const viewCenter = scrollTop + clientHeight / 2
    if (viewCenter >= domTop && viewCenter <= domBottom) {
      currentPage = pageNum
      scrollFraction = (scrollTop - domTop) / domHeight
    }
  })
  emit('scrollPageChange', {pageNum: currentPage, fraction: scrollFraction})
  visiblePageNums.value = newVisible
  renderVisible()
}, 16) // 节流 16ms，平衡性能与实时性

// 改造3：渲染逻辑（并发 + 任务插队 + 取消过期任务）
const renderVisible = debounce(async () => {
  await nextTick()
  const visible = new Set(visiblePageNums.value) // 快照当前可视区，避免后续变化
  const pendingPageNums = Array.from(visible).filter(num => {
    // 过滤：已渲染/正在渲染 跳过
    return !renderedCache.value.has(num) && !renderingLock.value.has(num)
  })

  // 步骤1：取消所有「非当前可视区」的渲染任务（过期任务）
  Object.entries(renderAbortControllers.value).forEach(([pageNumStr, controller]) => {
    const pageNum = Number(pageNumStr)
    if (!visible.has(pageNum)) {
      controller.abort() // 取消过期任务
      delete renderAbortControllers.value[pageNum]
      renderingLock.value.delete(pageNum) // 释放锁
    }
  })

  // 步骤2：并发渲染当前可视区未渲染的页面（新任务优先）
  if (pendingPageNums.length === 0) return

  // 倒序处理（实现「后进入的任务优先」，最新可视区页面先渲染）
  const reversedPending = [...pendingPageNums].reverse()
  await Promise.all(
      reversedPending.map(async (pageNum) => {
        // 二次校验：避免并发过程中已被标记/取消
        if (renderedCache.value.has(pageNum) || renderingLock.value.has(pageNum)) return

        const controller = new AbortController()
        const signal = controller.signal
        renderAbortControllers.value[pageNum] = controller
        renderingLock.value.add(pageNum)

        try {
          const canvas = canvasMap.value[pageNum]
          const page = props.pages.find(p => p.pageNum === pageNum)
          if (!canvas || !page || !page.render || signal.aborted) return

          const pdfPage = await page.page(pageNum)
          if (signal.aborted || !pdfPage) return

          // 执行渲染（传入 abort signal，支持取消）
          await page.render(canvas, props.scale ?? 1, pdfPage)

          // 渲染完成：标记缓存，释放资源
          renderedCache.value.add(pageNum)
        } catch (err) {
          // 忽略取消错误
          if (err instanceof DOMException && err.name === 'AbortError') return
          console.error(`渲染页面 ${pageNum} 失败`, err)
        } finally {
          // 无论成功/失败/取消，都释放锁
          renderingLock.value.delete(pageNum)
          delete renderAbortControllers.value[pageNum]
        }
      })
  )
}, 30) // 防抖 30ms，减少高频滚动时的渲染触发

const onScroll = () => {
  calcVisiblePages()
}

const historyScrollTo = async () => {
  await nextTick()

  const view = viewport.value
  if (!view) return
  // 找到对应页码的 DOM
  const targetDom = (Array.from(view.querySelectorAll('.pdf-page')) as HTMLDivElement[]).find(
      dom => Number(dom.dataset.pagenum) === (props.historyPage || 0)
  ) as HTMLDivElement
  if (!targetDom) return

  // 计算目标滚动距离
  const pageTop = targetDom.offsetTop
  const pageHeight = targetDom.offsetHeight
  // 页内偏移：按比例计算
  const pageOffset = pageHeight * (props.historyFrac || 0)

  // 最终滚动位置
  view.scrollTop = pageTop + pageOffset
}

// 页面列表变化：清空所有缓存和任务
watch(
    () => props.pages,
    async () => {
      // 取消所有未完成的渲染任务
      Object.values(renderAbortControllers.value).forEach(ctrl => ctrl.abort())
      renderAbortControllers.value = {}
      renderingLock.value.clear()
      renderedCache.value.clear()
      console.log('props.pages')
      await historyScrollTo()
      calcVisiblePages()
    },
    {deep: true}
)

// 缩放变化：清空缓存 + 重新渲染
watch(
    () => props.scale,
    async () => {
      Object.values(renderAbortControllers.value).forEach(ctrl => ctrl.abort())
      renderAbortControllers.value = {}
      renderingLock.value.clear()
      renderedCache.value.clear()
      console.log('props.scale')
      await historyScrollTo()
      calcVisiblePages()
    }
)
</script>

<style scoped>
.pdf-viewport {
  height: 100%;
  width: 100%;
  overflow-y: auto;
  overflow-x: auto;
  position: relative;
  scroll-behavior: auto;
  overscroll-behavior: none;
}

/* 给每页容器做居中 + 垂直排布 */
.pdf-page {
  position: relative;
  transform: translateZ(0);
  will-change: transform;
  transition: none;
  background: #fff;

  /* 核心：水平居中 */
  margin: 0 auto;
  /* 上下间距（保留分页间隙，按需调整） */
  margin-bottom: 20px;
}

.pdf-canvas {
  display: block;
  width: 100%;
  height: 100%;
}
</style>