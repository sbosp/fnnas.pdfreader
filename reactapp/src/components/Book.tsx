import PdfCover from './PdfCover'
import type {Item} from './HomePage'

function fmtSize(n: number) {
    if (!n) return ''
    if (n < 1024) return n + ' B'
    if (n < 1048576) return (n / 1024).toFixed(0) + ' KB'
    return (n / 1048576).toFixed(1) + ' MB'
}

export default function Book({book, onClick}: { book: Item, onClick: () => void }) {
    const p = book.progress
    const percent = Math.max(0, Math.min(100, p?.percent ?? 0))
    const pageText = p && p.totalPages ? `${(p.page ?? 0) + 1}/${p.totalPages}` : ''

    return (
        <button className="card" onClick={onClick} title={book.name}>
            <div className="cover">
                <div className="ph" aria-hidden="true">
                    <svg viewBox="0 0 24 24" fill="none">
                        <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                        <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                    </svg>
                </div>
                <PdfCover path={book.path}/>
                {pageText && <span className="badge">{pageText}</span>}
                {percent > 0 && <div className="cover-bar"><i style={{width: percent + '%'}}/></div>}
            </div>
            <div className="meta">
                <p className="title">{book.name}</p>
                <p className="sub">{fmtSize(book.size)}{percent > 0 ? ` · ${percent.toFixed(0)}%` : ''}</p>
            </div>
        </button>
    )
}
