import {useCallback, useEffect, useMemo, useRef, useState} from 'react'
import {useNavigate, useParams} from 'react-router-dom'
import {request} from '../utils/request'
import Folder from './Folder'
import Book from './Book'
import HistoryBook from './HistoryBook'

// 轻量比较签名：列表长度 + 每项 id/类型/进度，用于判断网络数据与当前显示是否一致
function sigOf(list: any[]) {
    return (list || []).map(b => `${b.id}:${b.type || ''}:${b.page ?? ''}:${b.pageCount ?? ''}`).join('|')
}

export default function HomePage() {
    const navigate = useNavigate()
    const {folderId = ''} = useParams()
    const [allBooks, setAllBooks] = useState<any[]>([])
    const [recentBooks, setRecentBooks] = useState<any[]>([])
    const [username, setUsername] = useState('')

    // 目录数据内存缓存：key=folderId('' 表示根)，返回上一级时立即命中，避免异步空窗闪动
    const pageCacheRef = useRef(new Map<string, { books: any[], history: any[], username: string }>())
    // 当前显示数据快照，用于「缓存命中且网络一致则跳过赋值」
    const snapRef = useRef({books: [] as any[], history: [] as any[], username: ''})

    // 连点 5 次「用户」启用 vconsole
    const userClickCount = useRef(0)
    const userClickTimer = useRef<number | null>(null)
    const onUserClick = () => {
        userClickCount.current++
        if (userClickTimer.current) clearTimeout(userClickTimer.current)
        userClickTimer.current = window.setTimeout(() => {
            userClickCount.current = 0
        }, 2000)
        if (userClickCount.current >= 5) {
            userClickCount.current = 0
            if (userClickTimer.current) {
                clearTimeout(userClickTimer.current)
                userClickTimer.current = null
            }
            const fn = (window as any).__enableVConsole
            if (typeof fn === 'function') Promise.resolve(fn()).then(() => console.log('✅ vConsole 已启用'))
        }
    }

    const refreshPage = useCallback((scan = "", path = '') => {
        if (path === '') path = folderId || ''
        let hitCache = false
        if (scan !== 'all') {
            const cached = pageCacheRef.current.get(path)
            if (cached) {
                hitCache = true
                snapRef.current = cached
                setAllBooks(cached.books)
                setRecentBooks(cached.history)
                setUsername(cached.username)
            }
        }
        request.get(`books?path=${path}&scan=${scan}`).then((data) => {
            const books = data.data.books || []
            const history = data.data.history || []
            const uname = data.data.username || '用户'
            pageCacheRef.current.set(path, {books, history, username: uname})
            // 命中缓存且网络数据与当前一致时跳过赋值，避免二次渲染闪动
            const cur = snapRef.current
            if (hitCache && uname === cur.username
                && sigOf(books) === sigOf(cur.books)
                && sigOf(history) === sigOf(cur.history)) return
            snapRef.current = {books, history, username: uname}
            setAllBooks(books)
            setRecentBooks(history)
            setUsername(uname)
        }).catch((err) => {
            console.error('❌ books请求失败:', err)
        })
    }, [folderId])

    useEffect(() => {
        refreshPage()
    }, [refreshPage])

    // 计算当前层级的数据（文件夹 / 文件分组）
    const currentLevel = useMemo(() => {
        const folders: any[] = []
        const files: any[] = []
        allBooks.forEach((b) => {
            if (b.type === 'folder') folders.push(b)
            else if (b.type === 'file') files.push(b)
        })
        return {folders, files}
    }, [allBooks])

    const refreshClick = () => {
        navigate('/')
        refreshPage('all')
    }
    const openBook = (book: any) => navigate(`/reader/${book.id}`)
    const enterFolder = (folder: any) => navigate(`/folder/${folder.id}`)
    const back = () => navigate(-1)

    return (
        <>
            {/* 顶部栏 */}
            <div className="topbar">
        <span className="brand">
          <button className="btn" onClick={back}>← 返回</button>
          <svg viewBox="0 0 24 24" fill="none">
            <path d="M6 2h8l4 4v16H6z" stroke="#2f6fed" strokeWidth="1.6" strokeLinejoin="round"/>
            <path d="M14 2v4h4" stroke="#2f6fed" strokeWidth="1.6" strokeLinejoin="round"/>
            <path d="M8.5 13h7M8.5 16.5h7M8.5 9.5h3" stroke="#2f6fed" strokeWidth="1.4" strokeLinecap="round"/>
          </svg>
          PDF 阅读器
        </span>
                <div className="spacer"></div>
                <button className="btn icon" onClick={refreshClick} title="刷新书库">刷新</button>
                <span className="user" onClick={onUserClick}>{username}</span>
            </div>

            {/* 书架内容 */}
            <div className="shelf-wrap">
                {/* 最近阅读 */}
                {recentBooks.length > 0 && (
                    <div className="recent">
                        <p className="rhead"><span className="ricon">🕘</span> 最近阅读</p>
                        <div className="recent-strip">
                            {recentBooks.map((book) => (
                                <div key={book.id} className="ritem" onClick={() => openBook(book)}>
                                    <HistoryBook book={book}/>
                                </div>
                            ))}
                        </div>
                    </div>
                )}

                <div className="shelf-head">
                    <h2>我的书架</h2>
                    {allBooks.length > 0 && <span className="count">共 {allBooks.length} 个</span>}
                </div>
                {allBooks.length === 0 && (
                    <div className="empty">
                        <svg viewBox="0 0 24 24" fill="none">
                            <path d="M4 5h16v14H4z" stroke="currentColor" strokeWidth="1.4"/>
                            <path d="M9 5v14" stroke="currentColor" strokeWidth="1.4"/>
                        </svg>
                        书架还是空的。<br/>请在「文件管理 → 应用文件 → PDF 阅读器 → PDFLibrary」中放入 PDF（可建子文件夹），<br/>或在「应用设置
                        → 允许访问的文件夹」中添加目录，然后点右上角「⟳ 刷新」。
                    </div>
                )}
                <div className="grid">
                    {/* 书籍 */}
                    {currentLevel.files.map((book) => (
                        <Book key={book.id} book={book} onClick={() => openBook(book)}/>
                    ))}
                </div>
                <div style={{height: 16}}/>
                <div className="grid">
                    {/* 文件夹 */}
                    {currentLevel.folders.map((folder) => (
                        <Folder key={folder.id} folder={folder} onClick={() => enterFolder(folder)}/>
                    ))}
                </div>
            </div>
        </>
    )
}
