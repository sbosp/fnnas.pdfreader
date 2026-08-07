import {useEffect, useRef, useState} from 'react'
import {download} from '../utils/request'
import {renderPdfSliceToDataUrl} from '../pdf'

// 封面内存缓存：bookId -> dataURL，避免重复下载/渲染。
const coverCache = new Map<string, string>()

// 1px 透明占位，避免无 src 时的破图图标。
const TRANSPARENT = 'data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw=='

// 封面缩略图：由前端 pdf.js 渲染第 0 页切片得到（Go 后端是纯处理库、不做光栅化，
// 故封面不走服务端 PNG 接口）。进入视口才懒加载 + 复用下载调度器限并发 + 内存缓存。
export default function PdfCover({bookId, className}: { bookId: string, className?: string }) {
    const [src, setSrc] = useState<string>(() => coverCache.get(bookId) || '')
    const imgRef = useRef<HTMLImageElement>(null)

    useEffect(() => {
        if (src) return
        const el = imgRef.current
        if (!el) return
        let cancelled = false
        const io = new IntersectionObserver((entries) => {
            for (const e of entries) {
                if (!e.isIntersecting) continue
                io.disconnect()
                void (async () => {
                    try {
                        const buf = await download(`pagepdf?id=${encodeURIComponent(bookId)}&page=0`)
                        const url = await renderPdfSliceToDataUrl(buf, 200)
                        if (cancelled) return
                        coverCache.set(bookId, url)
                        setSrc(url)
                    } catch { /* 封面失败则保留占位 */ }
                })()
            }
        }, {rootMargin: '200px'})
        io.observe(el)
        return () => {
            cancelled = true
            io.disconnect()
        }
    }, [bookId, src])

    return (
        <img
            ref={imgRef}
            className={className}
            src={src || TRANSPARENT}
            alt=""
            style={src ? undefined : {opacity: 0}}
        />
    )
}
