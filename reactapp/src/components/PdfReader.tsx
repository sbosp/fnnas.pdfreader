import {memo, useCallback, useEffect, useRef, useState} from 'react'
import {useNavigate, useParams} from 'react-router-dom'
import {download, request} from '../utils/request'
import {debounce} from '../utils/UIUtils'
import {MAX_DPR, pdfjsLib} from '../pdf'
import '../PdfReader.css'

// ============ 缩放常量 ============
const MIN_SCALE = 0.5
const MAX_SCALE = 3
const zoomLevels = [0.5, 0.75, 1, 1.25, 1.5, 2, 3]
const ZOOM_STORE_KEY = 'pdfreader.zoom.scale'

// ============ 加载/布局常量 ============
const MAX_CONCURRENT = 3
const PRELOAD_AHEAD = 2
const PRELOAD_BEHIND = 1
const OBSERVER_ROOT_MARGIN = '400px 0px'
const DEFAULT_PAGE_WIDTH = 595
const DEFAULT_PAGE_HEIGHT = 842
// .image-viewport 上内边距（CSS padding:12px 0）与 .pages-track 左内边距（padding:0 12px）。
const VIEWPORT_PAD_TOP = 12
const TRACK_PAD_LEFT = 12

// ============ 缩放比例本地持久化（按设备，localStorage）============
function loadLocalScale(): number {
    try {
        const raw = localStorage.getItem(ZOOM_STORE_KEY)
        if (raw == null) return 1
        const v = parseFloat(raw)
        if (!isFinite(v)) return 1
        return Math.min(MAX_SCALE, Math.max(MIN_SCALE, v))
    } catch {
        return 1
    }
}

function saveLocalScale(v: number) {
    try {
        localStorage.setItem(ZOOM_STORE_KEY, String(v))
    } catch { /* 隐私模式等静默忽略 */ }
}

interface PageItem {
    pageNum: number
    origWidth: number   // 原始 pt（来自 meta 或 pdf.js 校正）
    origHeight: number
    ratio: number       // origHeight / origWidth，用于 JS 算高度
    loading: boolean
    rendered: boolean
    error: boolean
}

// ============ 并发调度器（下载切片 + pdf.js 渲染）============
class PageLoader {
    private queue: number[] = []
    private running = new Set<number>()
    private aborted = new Set<number>()

    constructor(
        private getPage: (n: number) => PageItem | undefined,
        private pageCount: () => number,
        private doLoad: (n: number) => Promise<void>,
    ) {}

    request(pageNum: number, priority = false) {
        if (pageNum < 0 || pageNum >= this.pageCount()) return
        const p = this.getPage(pageNum)
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
            const p = this.getPage(pageNum)
            if (!p || p.rendered || p.loading) continue
            this.running.add(pageNum)
            this.doLoad(pageNum)
                .catch(() => { /* 失败由调用方标 error */ })
                .finally(() => {
                    this.running.delete(pageNum)
                    this.tick()
                })
        }
    }

    clear() {
        this.queue = []
        this.running.clear()
        this.aborted.clear()
    }
}

// ============ 单页组件（memo：只有该页状态变化才重渲染）============
const PdfPage = memo(function PdfPage(props: {
    page: PageItem
    setCanvasRef: (pn: number, el: HTMLCanvasElement | null) => void
    onRetry: (pn: number) => void
}) {
    const {page, setCanvasRef, onRetry} = props
    return (
        <div
            className="image-page"
            data-page-num={page.pageNum}
        >
            <canvas
                ref={(el) => setCanvasRef(page.pageNum, el)}
                className="page-canvas"
                style={{display: page.rendered ? 'block' : 'none'}}
            />
            {!page.rendered && !page.error && (
                <div className="ph-overlay">
                    <div className="ph-icon">
                        <svg viewBox="0 0 24 24" fill="none">
                            <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                            <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                        </svg>
                    </div>
                    <div className="ph-text">
                        {page.loading ? `加载中 ${page.pageNum + 1}...` : `第 ${page.pageNum + 1} 页`}
                    </div>
                </div>
            )}
            {page.error && (
                <div className="err-overlay" onClick={() => onRetry(page.pageNum)}>
                    <div>加载失败，点击重试</div>
                </div>
            )}
        </div>
    )
})

export default function PdfReader() {
    const {bookId = ''} = useParams()
    const navigate = useNavigate()
    const viewportRef = useRef<HTMLDivElement>(null)
    const trackRef = useRef<HTMLDivElement>(null)

    // UI 状态（驱动渲染的少量状态）
    const [bookName, setBookName] = useState('')
    const [total, setTotal] = useState(0)
    const [currentPage, setCurrentPage] = useState(0)
    const [scale, setScaleState] = useState(loadLocalScale())
    const [pages, setPagesState] = useState<PageItem[]>([])

    // 同步 ref：手势/异步回调里读最新值（避免 React 闭包旧值）
    const scaleRef = useRef(scale)
    const pagesRef = useRef<PageItem[]>(pages)
    const totalRef = useRef(0)
    const startFracRef = useRef(0)
    const stableClientWidthRef = useRef(0)
    const canvasElsRef = useRef(new Map<number, HTMLCanvasElement>())
    const pageDocsRef = useRef(new Map<number, any>())
    const pageAbortsRef = useRef(new Map<number, AbortController>())
    const ioRef = useRef<IntersectionObserver | null>(null)

    // 双指手势状态（全部走 ref，高频 touchmove 里读最新值）
    const pinchRef = useRef({
        startDist: 0, startScale: 1, contentX: 0, contentY: 0,
        lastMidX: 0, lastMidY: 0, pendingScale: 1, rafPending: false,
        vpRect: null as DOMRect | null, pinching: false, lastEndTime: 0,
    })
    // 首次初始化标记 + 待恢复进度（loadMeta 设置，useEffect 在 DOM 就绪后消费）
    const initializedRef = useRef(false)
    const pendingInitRef = useRef<{ page: number, frac: number } | null>(null)

    const setScale = (v: number) => {
        scaleRef.current = v
        setScaleState(v)
    }
    const setPages = (arr: PageItem[]) => {
        pagesRef.current = arr
        setPagesState(arr)
    }
    const updatePage = (pageNum: number, patch: Partial<PageItem>) => {
        const next = pagesRef.current.map(p => p.pageNum === pageNum ? {...p, ...patch} : p)
        pagesRef.current = next
        setPagesState(next)
    }

    // ============ 尺寸计算 ============
    // 页宽 = (稳定视口宽 - 轨道左右 padding 24) × scale。所有页等宽（CSS 变量驱动）。
    const computePageWidth = () => {
        const vw = (stableClientWidthRef.current || 800) - 24
        return Math.round(vw * scaleRef.current)
    }
    // 页高 = 页宽 × ratio（ratio = origHeight/origWidth，aspect-ratio 也据此自动撑高）
    const pageDisplayHeight = (p: PageItem) => Math.round(computePageWidth() * p.ratio)

    // 把页宽应用到 track 与每个 page（显式像素），并精确设定 track 宽度。
    // 关键兼容性决策：page 的宽高用「显式像素」直接设，**不用 CSS 变量 var(--pagew)、也不用
    // aspect-ratio** —— 这两个较新特性在 fnOS 的 iOS webview（内核可能较老）会失效，导致缩放后
    // 页面宽度/居中错乱（偏右、左边滑不出）。显式像素最兼容、最可预测；且用 JS 直接改 DOM、
    // 不经 React state，缩放时不触发列表重渲染（性能与兼容兼得）。
    const applyTrackWidth = () => {
        const track = trackRef.current
        const vp = viewportRef.current
        if (!track || !vp) return
        const pw = computePageWidth()
        // track 宽度 = 页宽 + 左右 padding(24)，缩小时至少撑满视口（不靠 max-content）。
        track.style.width = Math.max(pw + 24, vp.clientWidth) + 'px'
        const els = track.querySelectorAll<HTMLElement>('.image-page')
        els.forEach((el) => {
            const pn = parseInt(el.dataset.pageNum || '-1', 10)
            const p = pagesRef.current[pn]
            if (p) {
                el.style.width = pw + 'px'
                el.style.height = Math.round(pw * p.ratio) + 'px'
            }
        })
    }

    const refreshStableWidth = () => {
        const w = viewportRef.current?.clientWidth || 0
        if (w > 0) stableClientWidthRef.current = w
    }

    // ============ canvas 引用 ============
    const setCanvasRef = useCallback((pageNum: number, el: HTMLCanvasElement | null) => {
        if (el) canvasElsRef.current.set(pageNum, el)
        else canvasElsRef.current.delete(pageNum)
    }, [])

    // ============ 下载切片 + pdf.js 渲染 ============
    const renderPageCanvas = useCallback(async (pageNum: number): Promise<void> => {
        const doc = pageDocsRef.current.get(pageNum)
        const canvas = canvasElsRef.current.get(pageNum)
        const p = pagesRef.current[pageNum]
        if (!doc || !canvas || !p) return

        const pdfPage = await doc.getPage(1)
        const dpr = Math.min(window.devicePixelRatio || 1, MAX_DPR)
        const baseViewport = pdfPage.getViewport({scale: 1})

        // 以 pdf.js 实际渲染尺寸校正该页比例（meta 是 MediaBox，pdf.js 用 CropBox）。
        if (baseViewport.width > 0 && baseViewport.height > 0) {
            const realRatio = baseViewport.height / baseViewport.width
            if (!isFinite(p.ratio) || Math.abs(realRatio - p.ratio) > 0.003) {
                updatePage(pageNum, {
                    origWidth: baseViewport.width,
                    origHeight: baseViewport.height,
                    ratio: realRatio,
                })
                applyTrackWidth() // ratio 变了，按显式像素重算各页高度
            }
        }

        const cssWidth = computePageWidth()
        const renderScale = (cssWidth / baseViewport.width) * dpr
        const viewport = pdfPage.getViewport({scale: renderScale})
        const ctx = canvas.getContext('2d')
        if (!ctx) return
        canvas.width = Math.floor(viewport.width)
        canvas.height = Math.floor(viewport.height)
        canvas.style.width = '100%'
        canvas.style.height = '100%'
        await pdfPage.render({canvasContext: ctx, viewport}).promise
        pdfPage.cleanup()
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    const loadAndRender = useCallback(async (pageNum: number): Promise<void> => {
        const p = pagesRef.current[pageNum]
        if (!p) return
        const ac = new AbortController()
        pageAbortsRef.current.set(pageNum, ac)
        let buf: ArrayBuffer
        try {
            buf = await download(`pagepdf?id=${encodeURIComponent(bookId)}&page=${pageNum}`, ac.signal)
        } finally {
            pageAbortsRef.current.delete(pageNum)
        }
        const data = buf.slice(0)
        const doc = await pdfjsLib.getDocument({data, disableAutoFetch: true, disableStream: true}).promise
        pageDocsRef.current.set(pageNum, doc)
        await renderPageCanvas(pageNum)
        updatePage(pageNum, {loading: false, error: false, rendered: true})
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [bookId, renderPageCanvas])

    const loaderRef = useRef<PageLoader>()
    if (!loaderRef.current) {
        loaderRef.current = new PageLoader(
            (n) => pagesRef.current[n],
            () => pagesRef.current.length,
            async (n) => {
                updatePage(n, {loading: true})
                try {
                    await loadAndRender(n)
                } catch {
                    updatePage(n, {loading: false, error: true})
                }
            },
        )
    }

    // ============ 回收：滚出视区的页销毁 canvas + pdf.js 文档 ============
    const recyclePage = useCallback((pageNum: number) => {
        const p = pagesRef.current[pageNum]
        if (!p) return
        const ac = pageAbortsRef.current.get(pageNum)
        if (ac) {
            ac.abort()
            pageAbortsRef.current.delete(pageNum)
        }
        const doc = pageDocsRef.current.get(pageNum)
        if (doc) {
            try {
                doc.destroy()
            } catch { /* ignore */ }
            pageDocsRef.current.delete(pageNum)
        }
        const canvas = canvasElsRef.current.get(pageNum)
        if (canvas) {
            canvas.width = 0
            canvas.height = 0
        }
        updatePage(pageNum, {rendered: false, loading: false, error: false})
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    const retryPage = useCallback((pageNum: number) => {
        updatePage(pageNum, {error: false, rendered: false, loading: false})
        loaderRef.current?.request(pageNum, true)
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    // ============ IntersectionObserver 懒加载 ============
    const setupObserver = useCallback(() => {
        const vp = viewportRef.current
        if (!vp) return
        ioRef.current = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                const pn = parseInt((entry.target as HTMLElement).dataset.pageNum || '-1', 10)
                if (pn < 0) continue
                if (entry.isIntersecting) {
                    loaderRef.current?.request(pn, true)
                    for (let k = 1; k <= PRELOAD_AHEAD; k++) loaderRef.current?.request(pn + k, false)
                    for (let k = 1; k <= PRELOAD_BEHIND; k++) loaderRef.current?.request(pn - k, false)
                }
            }
        }, {root: vp, rootMargin: OBSERVER_ROOT_MARGIN, threshold: 0.01})
        const els = vp.querySelectorAll('.image-page')
        els.forEach(el => ioRef.current!.observe(el))
    }, [])

    const teardownObserver = useCallback(() => {
        ioRef.current?.disconnect()
        ioRef.current = null
    }, [])

    // ============ 滚动处理 ============
    const saveProgress = useRef(debounce(async (pageNum: number, fraction: number) => {
        if (!bookId) return
        try {
            await request.post(`progress?id=${bookId}`, {
                page: pageNum,
                frac: fraction,
                name: bookName,
                scale: scaleRef.current,
                totalPages: totalRef.current,
                percent: ((pageNum + 1) / totalRef.current * 100).toFixed(2),
            }, {headers: {'Content-Type': 'application/json'}})
        } catch (e) {
            console.warn('保存进度失败', e)
        }
    }, 800)).current

    const updateCurrentPageFromScroll = useCallback(() => {
        const vp = viewportRef.current
        if (!vp) return
        const scrollTop = vp.scrollTop
        const viewCenter = scrollTop + vp.clientHeight / 2
        let acc = 0
        const list = pagesRef.current
        for (let i = 0; i < list.length; i++) {
            const h = pageDisplayHeight(list[i]) + 20
            if (viewCenter >= acc && viewCenter < acc + h) {
                setCurrentPage(i)
                saveProgress(i, Math.min(1, Math.max(0, (viewCenter - acc) / h)))
                return
            }
            acc += h
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [saveProgress])

    const scheduleCancelInvisible = useRef(debounce(() => {
        const vp = viewportRef.current
        if (!vp) return
        const scrollTop = vp.scrollTop
        const viewBottom = scrollTop + vp.clientHeight
        const keep = new Set<number>()
        let acc = 0
        const buffer = vp.clientHeight * 2
        const list = pagesRef.current
        for (let i = 0; i < list.length; i++) {
            const h = pageDisplayHeight(list[i]) + 20
            const top = acc
            const bottom = acc + h
            if (bottom >= scrollTop - buffer && top <= viewBottom + buffer) keep.add(i)
            acc += h
        }
        loaderRef.current?.keepOnly(keep)
        for (const p of list) {
            if (!keep.has(p.pageNum) && (p.rendered || pageDocsRef.current.has(p.pageNum))) {
                recyclePage(p.pageNum)
            }
        }
    }, 200)).current

    const handleScroll = useCallback(() => {
        updateCurrentPageFromScroll()
        scheduleCancelInvisible()
    }, [updateCurrentPageFromScroll, scheduleCancelInvisible])

    // ============ 横向居中 ============
    // 内容(track)比视口宽时，把横向滚动停在正中；不宽时 scrollLeft 归 0 天然居中。
    const centerHorizontally = useCallback(() => {
        const vp = viewportRef.current
        if (!vp) return
        const extra = vp.scrollWidth - vp.clientWidth
        vp.scrollLeft = extra > 0 ? Math.round(extra / 2) : 0
    }, [])

    // ============ 重绘节流：缩放停止后按新 scale 重渲染可视页 canvas ============
    const rerenderVisible = useRef(debounce(() => {
        for (const [pageNum] of pageDocsRef.current) {
            renderPageCanvas(pageNum).catch(() => { /* ignore */ })
        }
    }, 200)).current

    // ============ 进度恢复滚动 ============
    const scrollToPage = useCallback((pageNum: number, frac = 0) => {
        const vp = viewportRef.current
        if (!vp) return
        let top = 0
        const list = pagesRef.current
        for (let i = 0; i < pageNum && i < list.length; i++) top += pageDisplayHeight(list[i]) + 20
        const h = (list[pageNum] ? pageDisplayHeight(list[pageNum]) : 0) + 20
        top += h * frac - vp.clientHeight / 2
        vp.scrollTop = Math.max(0, top)
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    // ============ 元数据加载 ============
    const loadMeta = useCallback(async () => {
        try {
            const response = await request.get(`meta?id=${bookId}`)
            const data = response.data
            const cnt = data.pageCount || 0
            totalRef.current = cnt
            setTotal(cnt)
            setBookName(data.name || '')
            setCurrentPage(data.progress?.page || 0)
            startFracRef.current = typeof data.progress?.frac === 'number' ? data.progress.frac : 0

            refreshStableWidth()
            const metaPages: Array<{ w: number; h: number }> = data.pages || []
            const list: PageItem[] = []
            for (let i = 0; i < cnt; i++) {
                const w = metaPages[i]?.w || DEFAULT_PAGE_WIDTH
                const h = metaPages[i]?.h || DEFAULT_PAGE_HEIGHT
                list.push({
                    pageNum: i,
                    origWidth: w,
                    origHeight: h,
                    ratio: h / w,
                    loading: false,
                    rendered: false,
                    error: false,
                })
            }
            setPages(list)
            // 记录待恢复进度，由 useEffect[pages] 在 DOM 就绪后消费（初始化 observer + 滚动定位）。
            pendingInitRef.current = {page: data.progress?.page || 0, frac: startFracRef.current}

            console.log('📚 PDF 阅读器已初始化', {文档: data.name, 总页数: cnt, 起始页: (data.progress?.page || 0) + 1})
        } catch (e) {
            console.error('加载文档元数据失败', e)
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [bookId])

    // ============ 按钮缩放 ============
    const applyZoom = useCallback((newScale: number) => {
        newScale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, newScale))
        const vp = viewportRef.current
        if (!vp) {
            setScale(newScale)
            applyTrackWidth()
            return
        }
        const anchorY = vp.clientHeight / 2
        const scrollTop = vp.scrollTop
        const focus = scrollTop + anchorY
        const list = pagesRef.current
        let acc = 0, anchorPage = 0, anchorRatio = 0
        for (let i = 0; i < list.length; i++) {
            const h = pageDisplayHeight(list[i]) + 20
            if (focus >= acc && focus < acc + h) {
                anchorPage = i
                anchorRatio = (focus - acc) / h
                break
            }
            acc += h
        }
        setScale(newScale)
        saveLocalScale(newScale)
        applyTrackWidth()
        // track 宽度/DOM 已同步更新（读取 scroll 会强制 reflow）
        let top = 0
        for (let i = 0; i < anchorPage && i < list.length; i++) top += pageDisplayHeight(list[i]) + 20
        top += ((list[anchorPage] ? pageDisplayHeight(list[anchorPage]) : 0) + 20) * anchorRatio
        vp.scrollTop = Math.max(0, top - anchorY)
        centerHorizontally()
        rerenderVisible()
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [centerHorizontally, rerenderVisible])

    const zoomIn = () => {
        const next = zoomLevels.find(z => z > scaleRef.current + 1e-6)
        if (next !== undefined) applyZoom(next)
    }
    const zoomOut = () => {
        const prev = [...zoomLevels].reverse().find(z => z < scaleRef.current - 1e-6)
        if (prev !== undefined) applyZoom(prev)
    }

    // ============ 双指缩放手势 ============
    // scale 是唯一数据源；手势中实时改 scale + applyTrackWidth（DOM），并用 scrollTop/scrollLeft
    // 让「双指中心指向的内容点」始终跟随双指 —— 纯滚动锚定，绝不用 transform。
    // 居中靠 page 的 block margin:0 auto 天然保证。手势中 canvas 不重绘（CSS 变量拉伸），松手重绘高清。
    const touchDist = (t0: Touch, t1: Touch) => Math.hypot(t0.clientX - t1.clientX, t0.clientY - t1.clientY)

    const onTouchStart = (e: TouchEvent) => {
        if (e.touches.length !== 2) return
        const vp = viewportRef.current
        if (!vp) return
        const S = pinchRef.current
        S.pinching = true
        S.startDist = touchDist(e.touches[0], e.touches[1])
        S.startScale = scaleRef.current
        S.vpRect = vp.getBoundingClientRect()
        const midX = (e.touches[0].clientX + e.touches[1].clientX) / 2
        const midY = (e.touches[0].clientY + e.touches[1].clientY) / 2
        S.lastMidX = midX
        S.lastMidY = midY
        // 锁定双指中心指向的内容坐标（当前 scale 布局下）：
        // 水平：page 左边缘屏幕 X = rect.left - scrollLeft + TRACK_PAD_LEFT
        // 垂直：第一页顶部屏幕 Y = rect.top + VIEWPORT_PAD_TOP - scrollTop
        S.contentX = (midX - S.vpRect.left) + vp.scrollLeft - TRACK_PAD_LEFT
        S.contentY = (midY - S.vpRect.top) + vp.scrollTop - VIEWPORT_PAD_TOP
    }

    const applyPinchFrame = (newScale: number) => {
        const vp = viewportRef.current
        const S = pinchRef.current
        if (!vp || !S.vpRect) return
        const k = newScale / S.startScale
        setScale(newScale)      // 更新 zoomLabel + scaleRef
        applyTrackWidth()       // 改 --pagew + track 宽度（DOM），浏览器随即 reflow
        // 内容点新坐标 = 旧 × k；让其屏幕位置 = 当前双指中心。
        vp.scrollTop = S.contentY * k - (S.lastMidY - S.vpRect.top) + VIEWPORT_PAD_TOP
        vp.scrollLeft = S.contentX * k - (S.lastMidX - S.vpRect.left) + TRACK_PAD_LEFT
    }

    const schedulePinchFrame = () => {
        const S = pinchRef.current
        if (S.rafPending) return
        S.rafPending = true
        requestAnimationFrame(() => {
            S.rafPending = false
            applyPinchFrame(S.pendingScale)
        })
    }

    const onTouchMove = (e: TouchEvent) => {
        const S = pinchRef.current
        if (!S.pinching || e.touches.length !== 2) return
        e.preventDefault()
        const dist = touchDist(e.touches[0], e.touches[1])
        if (S.startDist <= 0) return
        S.lastMidX = (e.touches[0].clientX + e.touches[1].clientX) / 2
        S.lastMidY = (e.touches[0].clientY + e.touches[1].clientY) / 2
        const ratio = dist / S.startDist
        S.pendingScale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, S.startScale * ratio))
        schedulePinchFrame()
    }

    const onTouchEnd = (e: TouchEvent) => {
        if (e.touches.length >= 2) return
        const S = pinchRef.current
        if (!S.pinching) return
        S.pinching = false
        S.vpRect = null
        S.lastEndTime = Date.now() // 供 handleResize 豁免判断
        saveLocalScale(scaleRef.current)
        rerenderVisible()
    }

    // ============ 窗口尺寸变化（含 iOS 手势/地址栏误触发豁免）============
    const handleResize = useRef(debounce(() => {
        const S = pinchRef.current
        // 双指手势期间及松手后豁免窗口内忽略 resize：此时多为 iOS 捏合/地址栏显隐误触发，
        // 若照常重排 + centerHorizontally 会覆盖双指锚定的 scroll，导致布局错乱。
        if (S.pinching || Date.now() - S.lastEndTime < 500) return
        const list = pagesRef.current
        if (!list.length) return
        const vp = viewportRef.current
        let anchorPage = 0, anchorRatio = 0
        if (vp) {
            const focus = vp.scrollTop + vp.clientHeight / 2
            let acc = 0
            for (let i = 0; i < list.length; i++) {
                const h = pageDisplayHeight(list[i]) + 20
                if (focus >= acc && focus < acc + h) {
                    anchorPage = i
                    anchorRatio = (focus - acc) / h
                    break
                }
                acc += h
            }
        }
        refreshStableWidth()
        applyTrackWidth()
        if (vp) {
            let top = 0
            for (let i = 0; i < anchorPage && i < list.length; i++) top += pageDisplayHeight(list[i]) + 20
            top += ((list[anchorPage] ? pageDisplayHeight(list[anchorPage]) : 0) + 20) * anchorRatio
            vp.scrollTop = Math.max(0, top - vp.clientHeight / 2)
            centerHorizontally()
        }
        rerenderVisible()
    }, 200)).current

    // ============ 首次初始化 ============
    // pages 渲染出 DOM 后，挂 observer + 恢复进度滚动 + 居中。用 useEffect 而非
    // loadMeta 里的 rAF/flushSync —— React 保证 effect 在 DOM 更新后执行，时序可靠。
    useEffect(() => {
        if (pages.length === 0 || initializedRef.current) return
        initializedRef.current = true
        applyTrackWidth()
        setupObserver()
        const pend = pendingInitRef.current
        if (pend) scrollToPage(pend.page, pend.frac)
        centerHorizontally()
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [pages])

    // ============ 生命周期 ============
    useEffect(() => {
        loadMeta()
        const vp = viewportRef.current
        if (vp) {
            vp.addEventListener('touchstart', onTouchStart, {passive: true})
            vp.addEventListener('touchmove', onTouchMove, {passive: false})
            vp.addEventListener('touchend', onTouchEnd, {passive: true})
            vp.addEventListener('touchcancel', onTouchEnd, {passive: true})
        }
        window.addEventListener('resize', handleResize)
        return () => {
            if (vp) {
                vp.removeEventListener('touchstart', onTouchStart)
                vp.removeEventListener('touchmove', onTouchMove)
                vp.removeEventListener('touchend', onTouchEnd)
                vp.removeEventListener('touchcancel', onTouchEnd)
            }
            window.removeEventListener('resize', handleResize)
            teardownObserver()
            loaderRef.current?.clear()
            for (const [pn] of pageDocsRef.current) recyclePage(pn)
            pageDocsRef.current.clear()
            canvasElsRef.current.clear()
            setPages([])
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [bookId])

    const close = () => navigate(-1)

    const zoomLabel = `${Math.round(scale * 100)}%`

    return (
        <div className="reader">
            {/* 工具栏（手机端隐藏，改用页脚页码 + 双指缩放）*/}
            <div className="reader-toolbar">
                <button className="btn btn-back" onClick={close}>← 返回</button>
                <span className="doc-title">{bookName}</span>
                <div className="spacer"/>
                <span className="pageinfo">{currentPage + 1} / {total}</span>
                <button className="btn" onClick={zoomOut}>－</button>
                <span className="zoom">{zoomLabel}</span>
                <button className="btn" onClick={zoomIn}>＋</button>
            </div>

            {/* 视口 */}
            <div className="image-viewport" ref={viewportRef} onScroll={handleScroll}>
                {/* 内容轨道：page 尺寸由 --pagew 变量 + aspect-ratio 驱动，block margin:0 auto 居中 */}
                <div className="pages-track" ref={trackRef}>
                    {pages.map((page) => (
                        <PdfPage
                            key={page.pageNum}
                            page={page}
                            setCanvasRef={setCanvasRef}
                            onRetry={retryPage}
                        />
                    ))}
                </div>
            </div>

            {/* 页脚页码（手机端顶部工具栏隐藏时显示）*/}
            <div className="page-footer">{currentPage + 1} / {total}</div>
        </div>
    )
}
