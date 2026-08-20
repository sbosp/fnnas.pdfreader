import PdfCover from './PdfCover'
import type {Item} from './HomePage'

/** 最近阅读项：封面 + 进度 + 名称。点击由外层 .ritem 处理。 */
export default function HistoryBook({book}: { book: Item }) {
    const p = book.progress
    const percent = Math.max(0, Math.min(100, p?.percent ?? 0))
    // 展示所在目录（相对书库根，去掉文件名本身），帮助区分同名书
    const dir = (book.segments || []).slice(0, -1).join(' / ')

    return (
        <>
            <div className="rcover">
                <div className="rph" aria-hidden="true">
                    <svg viewBox="0 0 24 24" fill="none">
                        <path d="M6 2h8l4 4v16H6z" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                        <path d="M14 2v4h4" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round"/>
                    </svg>
                </div>
                <PdfCover path={book.path}/>
                {p?.totalPages ? (
                    <span className="rpage">{(p.page ?? 0) + 1}/{p.totalPages}</span>
                ) : null}
                <div className="rbar"><i style={{width: percent + '%'}}/></div>
            </div>
            <p className="rname">{book.name}</p>
        </>
    )
}
