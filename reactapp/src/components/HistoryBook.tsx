import PdfCover from './PdfCover'

// 最近阅读项。外层 .ritem 包装与点击在 HomePage 中处理，本组件只渲染封面 + 名称。
export default function HistoryBook({book}: { book: any }) {
    return (
        <>
            <div className="rcover">
                <div className="rph">
                    <svg viewBox="0 0 24 24" fill="none">
                        <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                        <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                    </svg>
                </div>
                {/* 封面图：前端 pdf.js 渲染，盖在占位上方 */}
                <PdfCover bookId={book.id}/>
                {book.progress?.page ? (
                    <span className="rpage">
                        {book.progress.page}{book.progress.totalPages ? '/' + book.progress.totalPages : ''}
                    </span>
                ) : null}
                <div className="rbar">
                    <i style={{width: (book.progress?.percent || 0) + '%'}}/>
                </div>
            </div>
            <div className="rname">{book.name}</div>
        </>
    )
}
