import {useCallback, useEffect, useMemo, useRef, useState} from 'react'
import {useLocation, useNavigate} from 'react-router-dom'
import {request} from '../utils/request'
import Breadcrumb, {Crumb} from './Breadcrumb'
import Folder from './Folder'
import Book from './Book'
import HistoryBook from './HistoryBook'

export interface Item {
    name: string
    path: string
    type: 'file' | 'folder'
    size: number
    mtime: number
    count: number
    segments: string[]
    progress?: {
        page: number
        frac: number
        totalPages: number
        percent: number
        updatedAt: number
    }
}

/** 从 hash 路由 /browse/<encodedPath> 取出真实路径 */
function pathFromLocation(pathname: string): string {
    const m = pathname.match(/^\/browse\/(.*)$/)
    if (!m) return ''
    try {
        return decodeURIComponent(m[1] || '')
    } catch {
        return m[1] || ''
    }
}

export default function HomePage() {
    const navigate = useNavigate()
    const location = useLocation()
    const curPath = pathFromLocation(location.pathname)

    const [items, setItems] = useState<Item[]>([])
    const [crumbs, setCrumbs] = useState<Crumb[]>([])
    const [recent, setRecent] = useState<Item[]>([])
    const [username, setUsername] = useState('')
    const [loading, setLoading] = useState(false)
    const [err, setErr] = useState('')
    const [keyword, setKeyword] = useState('')

    // 目录数据内存缓存：返回上一级时立即命中，避免异步空窗闪动
    const cacheRef = useRef(new Map<string, { items: Item[], crumbs: Crumb[], username: string }>())
    const lastFetchAt = useRef(0)

    // 连点 5 次「用户名」启用 vconsole（真机排障）
    const clickN = useRef(0)
    const clickT = useRef<number | null>(null)
    const onUserClick = () => {
        clickN.current++
        if (clickT.current) clearTimeout(clickT.current)
        clickT.current = window.setTimeout(() => (clickN.current = 0), 2000)
        if (clickN.current >= 5) {
            clickN.current = 0
            const fn = (window as any).__enableVConsole
            if (typeof fn === 'function') Promise.resolve(fn()).then(() => console.log('✅ vConsole 已启用'))
        }
    }

    const loadList = useCallback((p: string, force = false) => {
        // 书库根（空 path）不走内存缓存：授权目录增删后回到首页必须立刻看到新列表
        const cached = !p ? undefined : cacheRef.current.get(p)
        if (cached && !force) {
            setItems(cached.items)
            setCrumbs(cached.crumbs)
            setUsername(cached.username)
        } else {
            setLoading(true)
        }
        setErr('')
        lastFetchAt.current = Date.now()
        request.get(`list?path=${encodeURIComponent(p)}`).then((res) => {
            const d = res.data
            const next = {
                items: (d.items || []) as Item[],
                crumbs: (d.breadcrumb || []) as Crumb[],
                username: d.username || '用户',
            }
            cacheRef.current.set(p, next)
            setItems(next.items)
            setCrumbs(next.crumbs)
            setUsername(next.username)
        }).catch((e) => {
            setErr(e?.response?.status === 404 ? '目录不存在或已被移除' : '加载失败，请重试')
        }).finally(() => setLoading(false))
    }, [])

    const loadRecent = useCallback(() => {
        request.get('recent').then((res) => {
            setRecent((res.data.items || []) as Item[])
        }).catch(() => setRecent([]))
    }, [])

    useEffect(() => {
        loadList(curPath)
    }, [curPath, loadList])

    useEffect(() => {
        loadRecent()
    }, [loadRecent, curPath])

    // 从应用设置回到页面时 iframe 通常还活着。只在「重新可见」时强刷，
    // 不在 pageshow 首屏再打一遍（会和上面的 loadList 叠成两次，接口一慢就像打不开）。
    useEffect(() => {
        const onVis = () => {
            if (document.visibilityState !== 'visible') return
            // iframe 刚挂上时有的 WebView 会立刻 fire visible，跟 mount 的 loadList 叠两次
            if (Date.now() - lastFetchAt.current < 1500) return
            cacheRef.current.clear()
            loadList(curPath, true)
            loadRecent()
        }
        document.addEventListener('visibilitychange', onVis)
        return () => document.removeEventListener('visibilitychange', onVis)
    }, [curPath, loadList, loadRecent])

    const {folders, files} = useMemo(() => {
        const kw = keyword.trim().toLowerCase()
        const match = (it: Item) => !kw || it.name.toLowerCase().includes(kw)
        return {
            folders: items.filter(i => i.type === 'folder' && match(i)),
            files: items.filter(i => i.type === 'file' && match(i)),
        }
    }, [items, keyword])

    const openBook = (it: Item) => navigate(`/read/${encodeURIComponent(it.path)}`)
    const enterFolder = (it: Item) => navigate(`/browse/${encodeURIComponent(it.path)}`)
    const goPath = (p: string) => navigate(`/browse/${encodeURIComponent(p)}`)
    const goHome = () => navigate('/')
    const refresh = () => {
        cacheRef.current.clear()
        loadList(curPath, true)
        loadRecent()
    }
    const canBack = crumbs.length > 1

    return (
        <div className="page">
            {/* 顶栏 + 路径导航条整体吸顶 */}
            <div className="page-head">
                {/* 顶部栏 */}
                <header className="topbar">
                    <div className="brand" onClick={goHome} role="button">
                        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                            <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round"/>
                            <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round"/>
                            <path d="M8.5 13h7M8.5 16.5h7M8.5 9.5h3" stroke="currentColor" strokeWidth="1.4"
                                  strokeLinecap="round"/>
                        </svg>
                        <span className="brand-name">PDF 阅读器</span>
                    </div>

                    <div className="search">
                        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                            <circle cx="11" cy="11" r="6.5" stroke="currentColor" strokeWidth="1.6"/>
                            <path d="M16 16l4.5 4.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"/>
                        </svg>
                        <input
                            value={keyword}
                            onChange={(e) => setKeyword(e.target.value)}
                            placeholder="筛选当前目录…"
                            aria-label="筛选当前目录"
                        />
                        {keyword && (
                            <button className="search-clear" onClick={() => setKeyword('')} aria-label="清空">×</button>
                        )}
                    </div>

                    <button className="iconbtn" onClick={refresh} title="刷新">
                        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                            <path d="M20 12a8 8 0 1 1-2.5-5.8" stroke="currentColor" strokeWidth="1.7"
                                  strokeLinecap="round"/>
                            <path d="M20 4v4.5h-4.5" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round"
                                  strokeLinejoin="round"/>
                        </svg>
                    </button>
                    <span className="user" onClick={onUserClick} title={username}>{username}</span>
                </header>

                {/* 路径导航条 */}
                <div className="crumb-bar">
                    {canBack && (
                        <button className="backbtn" onClick={() => navigate(-1)} title="返回上一级">
                            <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                                <path d="M15 5l-7 7 7 7" stroke="currentColor" strokeWidth="1.8"
                                      strokeLinecap="round" strokeLinejoin="round"/>
                            </svg>
                        </button>
                    )}
                    <Breadcrumb crumbs={crumbs} onNavigate={goPath} onHome={goHome}/>
                </div>
            </div>

            <main className="content">
                {/* 最近阅读 */}
                {recent.length > 0 && !keyword && (
                    <section className="sect">
                        <div className="sect-head">
                            <h2>继续阅读</h2>
                        </div>
                        <div className="recent-strip">
                            {recent.map((b) => (
                                <button className="ritem" key={b.path} onClick={() => openBook(b)}>
                                    <HistoryBook book={b}/>
                                </button>
                            ))}
                        </div>
                    </section>
                )}

                {err && <div className="alert">{err}</div>}

                {loading && items.length === 0 && (
                    <div className="grid">
                        {Array.from({length: 8}).map((_, i) => <div className="skel" key={i}/>)}
                    </div>
                )}

                {/* 文件夹 */}
                {folders.length > 0 && (
                    <section className="sect">
                        <div className="sect-head">
                            <h2>文件夹</h2>
                            <span className="count">{folders.length}</span>
                        </div>
                        <div className="grid grid-folder">
                            {folders.map((f) => (
                                <Folder key={f.path} folder={f} onClick={() => enterFolder(f)}/>
                            ))}
                        </div>
                    </section>
                )}

                {/* 书籍 */}
                {files.length > 0 && (
                    <section className="sect">
                        <div className="sect-head">
                            <h2>书籍</h2>
                            <span className="count">{files.length}</span>
                        </div>
                        <div className="grid">
                            {files.map((b) => (
                                <Book key={b.path} book={b} onClick={() => openBook(b)}/>
                            ))}
                        </div>
                    </section>
                )}

                {/* 空态 */}
                {!loading && !err && folders.length === 0 && files.length === 0 && (
                    <div className="empty">
                        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                            <path d="M4 5h16v14H4z" stroke="currentColor" strokeWidth="1.4"/>
                            <path d="M9 5v14" stroke="currentColor" strokeWidth="1.4"/>
                        </svg>
                        {keyword ? (
                            <p>没有匹配「{keyword}」的内容</p>
                        ) : (
                            <>
                                <p>这里还没有 PDF</p>
                                <p className="hint">
                                    在「文件管理 → 应用文件 → PDF 阅读器 → PDFLibrary」放入 PDF（可建子文件夹），
                                    或在应用设置里添加允许访问的文件夹，然后点右上角刷新。
                                </p>
                            </>
                        )}
                    </div>
                )}
            </main>
        </div>
    )
}
