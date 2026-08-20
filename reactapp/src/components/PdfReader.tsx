import {memo, useCallback, useEffect, useRef, useState} from 'react'
import {useLocation, useNavigate} from 'react-router-dom'
import {pageImgUrl, request} from '../utils/request'
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

/** 从 hash 路由 /read/<encodedPath> 取出书籍真实路径 */
function pathFromLocation(pathname: string): string {
    const m = pathname.match(/^\/read\/(.*)$/)
    if (!m) return ''
    try {
        return decodeURIComponent(m[1] || '')
    } catch {
        return m[1] || ''
    }
}

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
//
// 【重要】不要把随缩放变化的值（如当前页宽）当 prop 传进来：
// 那会让每一帧都击穿 memo，373 页的书就是每帧 373 次组件重渲染，双指缩放直接卡死。
// 初始尺寸改为通过 widthRef（引用恒定，不参与 memo 比较）在渲染时读取。
const PdfPage = memo(function PdfPage(props: {
    page: PageItem
    imgSrc: string
    /** 页宽读取器：引用恒定，故不会击穿 memo。仅用于挂载首帧给出尺寸，避免塌成 0 高导致多页重叠 */
    widthRef: {current: () => number}
    onLoaded: (pn: number, naturalW: number, naturalH: number) => void
    onError: (pn: number) => void
    onRetry: (pn: number) => void
}) {
    const {page, imgSrc, widthRef, onLoaded, onError, onRetry} = props
    // 挂载即带显式宽高：避免「JS 还没写入尺寸」的空窗期里 div 塌成 0 高，
    // 导致多页重叠（看起来像两页渲染进同一页）。applyTrackWidth 之后会精修这两个值。
    const w = widthRef.current()
    const initStyle = w > 0
        ? {width: w + 'px', height: Math.round(w * page.ratio) + 'px'}
        : undefined
    return (
        <div className="image-page" data-page-num={page.pageNum} style={initStyle}>
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
    const location = useLocation()
    const bookPath = pathFromLocation(location.pathname)
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
    // 视口高度快照：与宽度一样避免手势中反复读 DOM 触发布局
    const stableClientHeightRef = useRef(0)
    const ioRef = useRef<IntersectionObserver | null>(null)
    const initializedRef = useRef(false)
    const pendingInitRef = useRef<{ page: number, frac: number } | null>(null)

    // 双指手势状态。
    //
    // 锚点用「页号 + 页内相对位置」记录，不用文档绝对坐标：绝对坐标乘缩放比推算新位置
    // 会越翻越偏 —— 页间距固定 20px 不随缩放变化，而手势中 applyTrackWidth(true)
    // 只更新视口附近页的尺寸，锚点上方的页高度根本没变。
    const pinchRef = useRef({
        startDist: 0, startScale: 1,
        anchorPage: 0, anchorFracX: 0, anchorFracY: 0,
        lastMidX: 0, lastMidY: 0, pendingScale: 1, rafPending: false,
        vpRect: null as DOMRect | null, pinching: false, lastEndTime: 0,
    })

    const setScale = (v: number) => {
        scaleRef.current = v
        setScaleState(v)
    }
    /**
     * 双指手势进行中只更新 ref，不碰 React state。
     * scale 唯一的 state 用途是工具栏那个百分比文字，没必要为它每帧重渲染整个阅读器；
     * 手势结束时 setScale 同步一次即可。
     */
    const setScaleFast = (v: number) => {
        scaleRef.current = v
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

    // 页宽读取器：引用恒定，作为 prop 传给 memo 化的 PdfPage 也不会击穿 memo
    const pageWidthGetterRef = useRef<() => number>(() => 0)
    pageWidthGetterRef.current = computePageWidth

    // 缓存 .image-page 元素列表，避免每帧 querySelectorAll 全量扫描
    const pageElsCacheRef = useRef<{count: number, els: HTMLElement[]}>({count: 0, els: []})
    const pageEls = (): HTMLElement[] => {
        const track = trackRef.current
        if (!track) return []
        const cache = pageElsCacheRef.current
        const n = pagesRef.current.length
        // 页数变化（换书/加载完成）时才重新查询
        if (cache.count !== n || cache.els.length !== n) {
            cache.els = Array.from(track.querySelectorAll<HTMLElement>('.image-page'))
            cache.count = n
        }
        return cache.els
    }

    // 用「显式像素」设每页宽高 + track 宽度（不用 CSS 变量/aspect-ratio，兼容 iOS 老 webview）。
    // JS 直接改 DOM 不经 React state，缩放零列表重渲染。
    //
    // visibleOnly=true（双指缩放每帧调用）：只更新视口附近的页。
    //   大书全量更新代价太高（373 页 = 746 次样式写入/帧），而当帧只有几页可见。
    //   远处页在手势结束时由 visibleOnly=false 的那次调用补齐。
    // centerPage：指定更新区间的中心页。双指缩放传锚点页进来，一是保证锚点页一定被
    //   更新到（anchorScrollTo 要读它的新尺寸），二是省掉一次 vp.scrollTop 读取 ——
    //   写完 track 宽度再读会触发强制同步布局。
    const applyTrackWidth = (visibleOnly = false, centerPage?: number) => {
        const track = trackRef.current
        const vp = viewportRef.current
        if (!track || !vp) return
        const pw = computePageWidth()

        // 先把需要读的布局值一次读完，再统一写，避免「写→读→写」触发强制同步布局
        const clientW = stableClientWidthRef.current || vp.clientWidth
        track.style.width = Math.max(pw + 24, clientW) + 'px'

        const list = pagesRef.current
        const els = pageEls()
        if (!els.length) return

        // 计算需要精确更新的页区间
        let from = 0
        let to = els.length - 1
        if (visibleOnly) {
            // 用「最矮页」的比例估算单页高度：高度估小 → 反推的页号偏大不会偏小，
            // 配合 span 冗余可保证真正可见的页一定落在区间内（各页比例不同的书也安全，
            // 例如封面竖版 + 内页横版的双页扫描书）。
            let minRatio = Infinity
            for (let i = 0; i < list.length; i++) {
                const r = list[i]?.ratio
                if (r && r < minRatio) minRatio = r
            }
            if (!isFinite(minRatio) || minRatio <= 0) minRatio = 1.414
            const approxH = Math.round(pw * minRatio) + 20
            const center = centerPage ?? (approxH > 0 ? Math.floor(vp.scrollTop / approxH) : 0)
            // 视口最多能放几页（按最矮页算，取上界）+ 冗余
            const perScreen = approxH > 0 ? Math.ceil((stableClientHeightRef.current || vp.clientHeight) / approxH) : 1
            const span = Math.max(3, perScreen + 2)
            from = Math.max(0, center - span)
            to = Math.min(els.length - 1, center + span)
        }

        for (let i = from; i <= to; i++) {
            const el = els[i]
            if (!el) continue
            const pn = parseInt(el.dataset.pageNum || '-1', 10)
            const p = list[pn]
            if (!p) continue
            const h = Math.round(pw * p.ratio)
            // 只在值变化时写，减少无谓的样式失效
            const wPx = pw + 'px'
            const hPx = h + 'px'
            if (el.style.width !== wPx) el.style.width = wPx
            if (el.style.height !== hPx) el.style.height = hPx
        }
    }

    // ============ 缩放锚点 ============
    // 都直接用页元素的真实布局位置（offsetTop/offsetLeft，相对定位的 .image-viewport），
    // 不按缩放比推算：页间距、pages-track 的 padding、页窄于 track 时 margin:auto 的
    // 居中偏移，这些都不随缩放线性变化，自己算一个都不能漏。

    /** anchorFromPoint 把视口内一点换算成「页号 + 页内相对位置」*/
    const anchorFromPoint = (clientX: number, clientY: number, rect: DOMRect) => {
        const vp = viewportRef.current
        const els = pageEls()
        const fallback = {page: 0, fracX: 0.5, fracY: 0}
        if (!vp || !els.length) return fallback
        const contentX = (clientX - rect.left) + vp.scrollLeft
        const contentY = (clientY - rect.top) + vp.scrollTop
        // 找第一个底边越过该点的页（手势开始时布局是稳定的，直接信 DOM）
        let page = els.length - 1
        for (let i = 0; i < els.length; i++) {
            if (els[i].offsetTop + els[i].offsetHeight > contentY) {
                page = i
                break
            }
        }
        const el = els[page]
        if (!el || !el.offsetWidth || !el.offsetHeight) return fallback
        const clamp01 = (v: number) => Math.min(1, Math.max(0, v))
        return {
            page,
            // 落在页间距里时会被夹到边缘，视觉上等价
            fracX: clamp01((contentX - el.offsetLeft) / el.offsetWidth),
            fracY: clamp01((contentY - el.offsetTop) / el.offsetHeight),
        }
    }

    /** anchorScrollTo 滚动到「让锚点重新落在 (clientX, clientY) 」的位置 */
    const anchorScrollTo = (
        page: number, fracX: number, fracY: number,
        clientX: number, clientY: number, rect: DOMRect,
    ) => {
        const vp = viewportRef.current
        const el = pageEls()[page]
        if (!vp || !el) return
        // offsetTop/offsetLeft 是不受滚动影响的静态布局位置，正好就是滚动内容坐标系
        vp.scrollTop = el.offsetTop + fracY * el.offsetHeight - (clientY - rect.top)
        vp.scrollLeft = el.offsetLeft + fracX * el.offsetWidth - (clientX - rect.left)
    }

    const refreshStableWidth = () => {
        const vp = viewportRef.current
        const w = vp?.clientWidth || 0
        const h = vp?.clientHeight || 0
        if (w > 0) stableClientWidthRef.current = w
        if (h > 0) stableClientHeightRef.current = h
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
    // 用 ref 持有最新的书路径/书名，避免 debounce 闭包捕获旧值
    const bookPathRef = useRef(bookPath)
    bookPathRef.current = bookPath
    const bookNameRef = useRef(bookName)
    bookNameRef.current = bookName

    const saveProgress = useRef(debounce(async (pageNum: number, fraction: number) => {
        const p = bookPathRef.current
        if (!p) return
        try {
            await request.post(`progress?path=${encodeURIComponent(p)}`, {
                page: pageNum,
                frac: fraction,
                name: bookNameRef.current,
                scale: scaleRef.current,
                totalPages: totalRef.current,
                percent: Number(((pageNum + 1) / Math.max(1, totalRef.current) * 100).toFixed(2)),
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

    // 回收：滚出视口上下 N 屏的页卸载 img，释放解码位图防 OOM（翻回由 observer 重新请求，命中后端磁盘缓存）
    //
    // 必须用页元素的真实 offsetTop/Height，不能用 pageDisplayHeight 累加：
    // 双指缩放中 applyTrackWidth(true) 只改了附近页的尺寸，远处页还是旧高度，
    // 按「全页已是新比例」推位置会把正在看的页算到视口外 → 卸 img → 松手后重新加载。
    const scheduleUnloadInvisible = useRef(debounce(() => {
        const S = pinchRef.current
        if (S.pinching || Date.now() - S.lastEndTime < 800) return
        const vp = viewportRef.current
        if (!vp) return
        const scrollTop = vp.scrollTop
        const viewH = stableClientHeightRef.current || vp.clientHeight
        const lo = scrollTop - viewH * KEEP_SCREENS
        const hi = scrollTop + viewH + viewH * KEEP_SCREENS
        const keep = new Set<number>()
        const els = pageEls()
        if (els.length) {
            for (let i = 0; i < els.length; i++) {
                const el = els[i]
                const top = el.offsetTop
                if (top + el.offsetHeight >= lo && top <= hi) {
                    const pn = parseInt(el.dataset.pageNum || '-1', 10)
                    if (pn >= 0) keep.add(pn)
                }
            }
        } else {
            let acc = 0
            const list = pagesRef.current
            for (let i = 0; i < list.length; i++) {
                const h = pageDisplayHeight(list[i]) + 20
                if (acc + h >= lo && acc <= hi) keep.add(i)
                acc += h
            }
        }
        const list = pagesRef.current
        let changed = false
        const next = list.map(p => {
            if (keep.has(p.pageNum) || !p.shouldLoad) return p
            changed = true
            return {...p, shouldLoad: false, loaded: false}
        })
        if (changed) setPages(next)
    }, 300)).current

    const handleScroll = useCallback(() => {
        // 双指缩放每帧都在改 scrollTop，这里若跟着跑会把「卸载」误触发在半新半旧布局上
        if (pinchRef.current.pinching) return
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
        if (!bookPath) return
        try {
            // meta（页数+尺寸）与 progress（阅读进度）并行请求。
            // 两者都不走浏览器缓存（后端 no-store），后端改动/换书能立即生效；
            // 页面图片的复用靠后端磁盘缓存（书库 .pdfreader-cache），不依赖浏览器缓存。
            const pq = encodeURIComponent(bookPath)
            const [metaRes, progRes] = await Promise.all([
                request.get(`meta?path=${pq}`),
                request.get(`progress?path=${pq}`),
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
            // 后端只返回首页尺寸（所有页共用），每页真实比例由 img onLoad 校正
            const w = data.width || DEFAULT_PAGE_WIDTH
            const h = data.height || DEFAULT_PAGE_HEIGHT
            const list: PageItem[] = []
            for (let i = 0; i < cnt; i++) {
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
        } catch (e) {
            console.error('加载文档元数据失败', e)
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [bookPath])

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
        // 以视口正中为锚点，缩放前后都用真实布局定位（同双指缩放）
        const rect = vp.getBoundingClientRect()
        const cx = rect.left + vp.clientWidth / 2
        const cy = rect.top + vp.clientHeight / 2
        const a = anchorFromPoint(cx, cy, rect)
        setScale(newScale)
        saveLocalScale(newScale)
        applyTrackWidth()
        anchorScrollTo(a.page, a.fracX, a.fracY, cx, cy, rect)
        centerHorizontally()  // 按钮缩放的横向行为是保持居中，覆盖掉横向锚点
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
        // 只刷高度快照（用于估算可见页区间）。**不能刷宽度**——
        // stableClientWidthRef 是 computePageWidth 的基准，手势中途变更会让页宽突变。
        stableClientHeightRef.current = vp.clientHeight || stableClientHeightRef.current
        const midX = (e.touches[0].clientX + e.touches[1].clientX) / 2
        const midY = (e.touches[0].clientY + e.touches[1].clientY) / 2
        S.lastMidX = midX
        S.lastMidY = midY
        const a = anchorFromPoint(midX, midY, S.vpRect)
        S.anchorPage = a.page
        S.anchorFracX = a.fracX
        S.anchorFracY = a.fracY
    }

    // 手势中的每一帧：全程不碰 React state、只更新可见页、写操作集中在末尾。
    const applyPinchFrame = (newScale: number) => {
        const vp = viewportRef.current
        const S = pinchRef.current
        if (!vp || !S.vpRect) return
        setScaleFast(newScale)                   // 只写 ref，零重渲染
        applyTrackWidth(true, S.anchorPage)      // 只更新锚点页附近
        // 锚点跟随双指中点移动，所以缩放的同时也能平移
        anchorScrollTo(S.anchorPage, S.anchorFracX, S.anchorFracY, S.lastMidX, S.lastMidY, S.vpRect)
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
        S.lastEndTime = Date.now()
        // 手势结束：补齐所有页的精确尺寸（手势中只更新了视口附近页），
        // 并把 scale 同步进 React state（工具栏百分比、以及依赖 scale 的逻辑）。
        applyTrackWidth(false)
        // 补齐会改掉锚点页上方那些页的高度、从而挪动锚点页的布局位置，
        // 必须再对位一次，否则松手瞬间会跳一下。
        if (S.vpRect) anchorScrollTo(S.anchorPage, S.anchorFracX, S.anchorFracY, S.lastMidX, S.lastMidY, S.vpRect)
        S.vpRect = null
        setScale(scaleRef.current)
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
    }, [bookPath])

    const close = () => navigate(-1)
    const zoomLabel = `${Math.round(scale * 100)}%`

    return (
        <div className="reader">
            {/* 工具栏 */}
            <div className="reader-toolbar">
                <button className="iconbtn" onClick={close} title="返回">
                    <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                        <path d="M15 5l-7 7 7 7" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"
                              strokeLinejoin="round"/>
                    </svg>
                </button>
                <h1 className="reader-title" title={bookName}>{bookName}</h1>
                <span className="pageinfo">{currentPage + 1} / {total}</span>
                <div className="zoomgroup">
                    <button className="iconbtn" onClick={zoomOut} title="缩小">－</button>
                    <span className="zoom">{zoomLabel}</span>
                    <button className="iconbtn" onClick={zoomIn} title="放大">＋</button>
                </div>
            </div>

            {/* 视口 */}
            <div className="image-viewport" ref={viewportRef} onScroll={handleScroll}>
                <div className="pages-track" ref={trackRef}>
                    {pages.map((page) => (
                        <PdfPage
                            key={page.pageNum}
                            page={page}
                            imgSrc={pageImgUrl(bookPath, page.pageNum)}
                            widthRef={pageWidthGetterRef}
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
