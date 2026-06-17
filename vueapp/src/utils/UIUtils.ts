

// 节流工具
export function throttle<T extends (...args: any[]) => void>(fn: T, delay = 60) {
    let timer: number | null = null
    return (...args: Parameters<T>) => {
        if (timer) return
        timer = window.setTimeout(() => {
            fn(...args)
            timer = null
        }, delay)
    }
}

// 防抖工具
export function debounce<T extends (...args: any[]) => void>(fn: T, delay = 80) {
    let timer: number | null = null
    return (...args: Parameters<T>) => {
        if (timer) clearTimeout(timer)
        timer = window.setTimeout(() => {
            fn(...args)
            timer = null
        }, delay)
    }
}