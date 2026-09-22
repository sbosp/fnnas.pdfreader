import {pageImgUrl} from '../utils/request'

/**
 * 封面缩略图：服务端低 DPI 图，浏览器按 Cache-Control 缓存 7 天。
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
