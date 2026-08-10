import {memo, useCallback, useEffect, useRef, useState} from 'react'
import {useNavigate, useParams} from 'react-router-dom'
import {request} from '../utils/request'
import {debounce} from '../utils/UIUtils'
import '../PdfReader.css'

// ============ 缩放常量 ============
const MIN_SCALE = 0.5
const MAX_SCALE = 3
const zoomLevels = [0.5, 0.75, 1, 1.25, 1.5, 2, 3]
const ZOOM_STORE_KEY = 'pdfreader.zoom.scale'

// ============ 加载/布局常量 ============
const PRELOAD_AHEAD = 3      // 向下预加载页数
const PRELOAD_BEHIND = 1     // 向上预加载页数
const OBSERVER_ROOT_MARGIN = '800px 0px' // 提前触发加载的缓冲距离
const KEEP_SCREENS = 2       // 视口上下保留 N 屏的图片，之外卸载防 OOM
const DEFAULT_PAGE_WIDTH = 595
const DEFAULT_PAGE_HEIGHT = 842
const VIEWPORT_PAD_TOP = 12
const TRACK_PAD_LEFT = 12
const API_BASE = '/app/fnnas-pdfreader/api'

// ============ 缩放比例本地持久化 ============
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
    origWidth: number   // 原始 pt（来自 meta，或 img 加载后按真实比例校正）
    origHeight: number
    ratio: number       // origHeight / origWidth，JS 算高度用
    shouldLoad: boolean // observer 触发：是否渲染 img
    loaded: boolean     // img 加载完成
    error: boolean
}

// ============ 单页组件（memo：只有该页状态变化才重渲染）============
// 宽高由父级 applyTrackWidth 用 JS 直接设显式像素（不经 React），兼容 iOS 老 webview。
const PdfPage = memo(function PdfPage(props: {
    page: PageItem
    imgSrc: string
    onLoaded: (pn: number, naturalW: number, naturalH: number) => void
    onError: (pn: number) => void
    onRetry: (pn: number) => void
}) {
    const {page, imgSrc, onLoaded, onError, onRetry} = props
    return (
        <div className="image-page" data-page-num={page.pageNum}>
            {page.shouldLoad && (
                <img
                    className="page-img"
                    src={imgSrc}
                    alt=""
                    draggable={false}
                    onLoad={(e) => onLoaded(page.pageNum, e.currentTarget.naturalWidth, e.currentTarget.naturalHeight)}
                    onError={() => onError(page.pageNum)}
                    style={{display: page.loaded ? 'block' : 'none'}}
                />
            )}
            {!page.loaded && !page.error && (
                <div className="ph-overlay">
                    <div className="ph-icon">
                        <svg viewBox="0 0 24 24" fill="none">
                            <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                            <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                        </svg>
                    </div>
                    <div className="ph-text">{page.shouldLoad ? `加载中 ${page.pageNum + 1}...` : `第 ${page.pageNum + 1} 页`}</div>
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

    // UI 状态
    const [bookName, setBookName] = useState('')
    const [total, setTotal] = useState(0)
    const [currentPage, setCurrentPage] = useState(0)
    const [scale, setScaleState] = useState(loadLocalScale())
    const [pages, setPagesState] = useState<PageItem[]>([])

    // 同步 ref：手势/异步回调读最新值
    const scaleRef = useRef(scale)
    const pagesRef = useRef<PageItem[]>(pages)
    const totalRef = useRef(0)
    const startFracRef = useRef(0)
    const stableClientWidthRef = useRef(0)
    const ioRef = useRef<IntersectionObserver | null>(null)
    const initializedRef = useRef(false)
    const pendingInitRef = useRef<{ page: number, frac: number } | null>(null)

    // 双指手势状态
    const pinchRef = useRef({
        startDist: 0, startScale: 1, contentX: 0, contentY: 0,
        lastMidX: 0, lastMidY: 0, pendingScale: 1, rafPending: false,
        vpRect: null as DOMRect | null, pinching: false, lastEndTime: 0,
    })

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
    const computePageWidth = () => {
        const vw = (stableClientWidthRef.current || 800) - 24
        return Math.round(vw * scaleRef.current)
    }
    const pageDisplayHeight = (p: PageItem) => Math.round(computePageWidth() * p.ratio)

    // 用「显式像素」设每页宽高 + track 宽度（不用 CSS 变量/aspect-ratio，兼容 iOS 老 webview）。
    // JS 直接改 DOM 不经 React state，缩放零列表重渲染。
    const applyTrackWidth = () => {
        const track = trackRef.current
        const vp = viewportRef.current
        if (!track || !vp) return
        const pw = computePageWidth()
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

    // ============ 图片加载回调 ============
    const onLoaded = useCallback((pageNum: number, naturalW: number, naturalH: number) => {
        const p = pagesRef.current[pageNum]
        if (!p) return
        // 按图片真实宽高比校正（meta 的 MediaBox 与渲染图可能略有差异），避免拉伸
        if (naturalW > 0 && naturalH > 0) {
            const realRatio = naturalH / naturalW
            if (Math.abs(realRatio - p.ratio) > 0.003) {
                const next = pagesRef.current.map(x => x.pageNum === pageNum
                    ? {...x, origWidth: naturalW, origHeight: naturalH, ratio: realRatio, loaded: true, error: false}
                    : x)
                pagesRef.current = next
                setPagesState(next)
                applyTrackWidth()
                return
            }
        }
        updatePage(pageNum, {loaded: true, error: false})
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    const onError = useCallback((pageNum: number) => {
        updatePage(pageNum, {loaded: false, error: true, shouldLoad: false})
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    const retryPage = useCallback((pageNum: number) => {
        updatePage(pageNum, {error: false, loaded: false, shouldLoad: true})
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    // ============ IntersectionObserver 懒加载 ============
    const markLoad = useCallback((pageNum: number) => {
        const list = pagesRef.current
        if (pageNum < 0 || pageNum >= list.length) return
        if (!list[pageNum].shouldLoad && !list[pageNum].loaded) {
            updatePage(pageNum, {shouldLoad: true})
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [])

    const setupObserver = useCallback(() => {
        const vp = viewportRef.current
        if (!vp) return
        ioRef.current = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                const pn = parseInt((entry.target as HTMLElement).dataset.pageNum || '-1', 10)
                if (pn < 0) continue
                if (entry.isIntersecting) {
                    markLoad(pn)
                    for (let k = 1; k <= PRELOAD_AHEAD; k++) markLoad(pn + k)
                    for (let k = 1; k <= PRELOAD_BEHIND; k++) markLoad(pn - k)
                }
            }
        }, {root: vp, rootMargin: OBSERVER_ROOT_MARGIN, threshold: 0.01})
        vp.querySelectorAll('.image-page').forEach(el => ioRef.current!.observe(el))
    }, [markLoad])

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

    // 回收：滚出视口上下 N 屏的页卸载 img，释放解码位图防 OOM（翻回由 observer 重新加载，走 HTTP 缓存）
    const scheduleUnloadInvisible = useRef(debounce(() => {
        const vp = viewportRef.current
        if (!vp) return
        const scrollTop = vp.scrollTop
        const viewBottom = scrollTop + vp.clientHeight
        const buffer = vp.clientHeight * KEEP_SCREENS
        let acc = 0
        const list = pagesRef.current
        const keep = new Set<number>()
        for (let i = 0; i < list.length; i++) {
            const h = pageDisplayHeight(list[i]) + 20
            if (acc + h >= scrollTop - buffer && acc <= viewBottom + buffer) keep.add(i)
            acc += h
        }
        for (const p of list) {
            if (!keep.has(p.pageNum) && p.shouldLoad) {
                updatePage(p.pageNum, {shouldLoad: false, loaded: false})
            }
        }
    }, 300)).current

    const handleScroll = useCallback(() => {
        updateCurrentPageFromScroll()
        scheduleUnloadInvisible()
    }, [updateCurrentPageFromScroll, scheduleUnloadInvisible])

    // ============ 横向居中 ============
    const centerHorizontally = useCallback(() => {
        const vp = viewportRef.current
        if (!vp) return
        const extra = vp.scrollWidth - vp.clientWidth
        vp.scrollLeft = extra > 0 ? Math.round(extra / 2) : 0
    }, [])

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
            // meta（页数+尺寸，immutable 长缓存）与 progress（阅读进度，实时）分开请求：
            // meta 可被浏览器长缓存，progress 必须实时，否则「继续阅读」会停在旧进度。
            const [metaRes, progRes] = await Promise.all([
                request.get(`meta?id=${bookId}`),
                request.get(`progress?id=${bookId}`),
            ])
            const data = metaRes.data
            const prog = progRes.data?.progress
            const cnt = data.pageCount || 0
            totalRef.current = cnt
            setTotal(cnt)
            setBookName(data.name || '')
            const startPage = prog?.page || 0
            const startFrac = typeof prog?.frac === 'number' ? prog.frac : 0
            setCurrentPage(startPage)
            startFracRef.current = startFrac

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
                    shouldLoad: false,
                    loaded: false,
                    error: false,
                })
            }
            setPages(list)
            pendingInitRef.current = {page: startPage, frac: startFrac}
            console.log('📚 PDF 阅读器已初始化（图片方案）', {文档: data.name, 总页数: cnt, 起始页: startPage + 1})
        } catch (e) {
            console.error('加载文档元数据失败', e)
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [bookId])

    // ============ 首次初始化 ============
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
        let top = 0
        for (let i = 0; i < anchorPage && i < list.length; i++) top += pageDisplayHeight(list[i]) + 20
        top += ((list[anchorPage] ? pageDisplayHeight(list[anchorPage]) : 0) + 20) * anchorRatio
        vp.scrollTop = Math.max(0, top - anchorY)
        centerHorizontally()
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [centerHorizontally])

    const zoomIn = () => {
        const next = zoomLevels.find(z => z > scaleRef.current + 1e-6)
        if (next !== undefined) applyZoom(next)
    }
    const zoomOut = () => {
        const prev = [...zoomLevels].reverse().find(z => z < scaleRef.current - 1e-6)
        if (prev !== undefined) applyZoom(prev)
    }

    // ============ 双指缩放手势（纯滚动锚定，不 transform）============
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
        S.contentX = (midX - S.vpRect.left) + vp.scrollLeft - TRACK_PAD_LEFT
        S.contentY = (midY - S.vpRect.top) + vp.scrollTop - VIEWPORT_PAD_TOP
    }

    const applyPinchFrame = (newScale: number) => {
        const vp = viewportRef.current
        const S = pinchRef.current
        if (!vp || !S.vpRect) return
        const k = newScale / S.startScale
        setScale(newScale)
        applyTrackWidth()
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
        S.lastEndTime = Date.now()
        saveLocalScale(scaleRef.current)
    }

    // ============ 窗口尺寸变化（含 iOS 手势/地址栏误触发豁免）============
    const handleResize = useRef(debounce(() => {
        const S = pinchRef.current
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
    }, 200)).current

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
            setPages([])
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [bookId])

    const close = () => navigate(-1)
    const zoomLabel = `${Math.round(scale * 100)}%`

    return (
        <div className="reader">
            {/* 工具栏（手机端隐藏）*/}
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
                <div className="pages-track" ref={trackRef}>
                    {pages.map((page) => (
                        <PdfPage
                            key={page.pageNum}
                            page={page}
                            imgSrc={`${API_BASE}/pageimg?id=${encodeURIComponent(bookId)}&page=${page.pageNum}`}
                            onLoaded={onLoaded}
                            onError={onError}
                            onRetry={retryPage}
                        />
                    ))}
                </div>
            </div>

            {/* 页脚页码（手机端）*/}
            <div className="page-footer">{currentPage + 1} / {total}</div>
        </div>
    )
}
