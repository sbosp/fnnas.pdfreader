import {pageImgUrl} from '../utils/request'

/**
 * 封面缩略图：服务端渲染的低 DPI 图片，img 原生懒加载。
 * 图片不走浏览器缓存（后端 no-store），复用由后端磁盘缓存负责。
 * 路径化：以书籍真实路径为标识（不再用 hash id）。
 */
export default function PdfCover({path, className}: { path: string, className?: string }) {
    return (
        <img
            className={className}
            src={pageImgUrl(path, 0, 80)}
            loading="lazy"
            decoding="async"
            alt=""
            draggable={false}
        />
    )
}
