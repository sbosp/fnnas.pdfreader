import {useEffect, useRef} from 'react'

export interface Crumb {
    name: string
    path: string
}

/**
 * 路径导航条（面包屑）。展示书库根到当前目录的完整层级，每级可点击跳转。
 * 移动端横向滚动，并自动滚到最右（保证当前层级可见）。
 */
export default function Breadcrumb(props: {
    crumbs: Crumb[]
    onNavigate: (path: string) => void
    onHome: () => void
}) {
    const {crumbs, onNavigate, onHome} = props
    const navRef = useRef<HTMLElement>(null)

    // 只在层级变化时滚到最右保证当前位置可见。
    // 不能用 ref 回调每次渲染都滚 —— 用户横向拖去点上层目录时会被立刻弹回最右。
    useEffect(() => {
        const el = navRef.current
        if (el) el.scrollLeft = el.scrollWidth
    }, [crumbs])

    return (
        <nav className="crumb" ref={navRef} aria-label="路径导航">
            <button className="crumb-item crumb-home" onClick={onHome} title="书库根目录">
                <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <path d="M3 11.5 12 4l9 7.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"
                          strokeLinejoin="round"/>
                    <path d="M5.5 10v9.5h13V10" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round"/>
                </svg>
                <span>书库</span>
            </button>

            {crumbs.map((c, i) => {
                const isLast = i === crumbs.length - 1
                return (
                    <span className="crumb-seg" key={c.path}>
                        <svg className="crumb-sep" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                            <path d="M9 5l7 7-7 7" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"
                                  strokeLinejoin="round"/>
                        </svg>
                        {isLast ? (
                            <span className="crumb-item is-current" aria-current="page">{c.name}</span>
                        ) : (
                            <button className="crumb-item" onClick={() => onNavigate(c.path)}>{c.name}</button>
                        )}
                    </span>
                )
            })}
        </nav>
    )
}
