// 封面缩略图：直接由服务端 pageimg 渲染（低 dpi 小图），img 原生懒加载 + 浏览器 HTTP 缓存。
// 图片方案下前端不再引入 pdf.js —— 封面/正文统一走服务端渲染的图片。
export default function PdfCover({bookId, className}: { bookId: string, className?: string }) {
    return (
        <img
            className={className}
            src={`/app/fnnas-pdfreader/api/pageimg?id=${encodeURIComponent(bookId)}&page=0&dpi=80`}
            loading="lazy"
            alt=""
            draggable={false}
        />
    )
}
