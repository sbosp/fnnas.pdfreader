import PdfCover from './PdfCover'

export default function Book({book, onClick}: { book: any, onClick: () => void }) {
    const percent = book.progress?.percent ?? 0
    const p = book.progress
    const progressText = p && p.page ? `读到 ${p.page + 1}${p.totalPages ? '/' + p.totalPages : ''} 页` : ''

    return (
        <div className="card" onClick={onClick}>
            <div className="cover">
                {/* 封面图：前端 pdf.js 渲染，绝对定位盖在占位上方 */}
                <PdfCover bookId={book.id}/>
                {/* 占位 */}
                <div className="ph">
                    <svg viewBox="0 0 24 24" fill="none">
                        <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                        <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                    </svg>
                </div>
                {/* 阅读进度徽标 */}
                {progressText && <span className="badge">{progressText}</span>}
            </div>

            <div className="meta">
                <p className="title" title={book.name}>{book.name}</p>
                <div className="sub">
                    {fmtSize(book.size)}
                    {percent ? `　·　${percent}%` : ''}
                </div>
                {percent ? <div className="progressbar"><i style={{width: percent + '%'}}/></div> : null}
            </div>
        </div>
    )
}

function fmtSize(n: number) {
    if (n < 1024) return n + ' B'
    if (n < 1048576) return (n / 1024).toFixed(0) + ' KB'
    return (n / 1048576).toFixed(1) + ' MB'
}
