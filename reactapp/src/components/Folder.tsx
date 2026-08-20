import type {Item} from './HomePage'

export default function Folder({folder, onClick}: { folder: Item, onClick: () => void }) {
    return (
        <button className="folder" onClick={onClick} title={folder.name}>
            <div className="ficon" aria-hidden="true">
                <svg viewBox="0 0 24 24" fill="none">
                    <path
                        d="M3 6.5C3 5.7 3.7 5 4.5 5H9l2 2h8.5c.8 0 1.5.7 1.5 1.5v9c0 .8-.7 1.5-1.5 1.5h-15C3.7 19 3 18.3 3 17.5v-11z"
                        fill="#fbd277" stroke="#f0b53d" strokeWidth="1"/>
                    <path d="M3 9h18v8.5c0 .8-.7 1.5-1.5 1.5h-15C3.7 19 3 18.3 3 17.5V9z" fill="#fdd98a"/>
                </svg>
            </div>
            <div className="fmeta">
                <p className="fname">{folder.name}</p>
                <p className="fsub">{folder.count > 0 ? `${folder.count} 项` : '空文件夹'}</p>
            </div>
        </button>
    )
}
