import axios, {AxiosInstance, AxiosResponse, InternalAxiosRequestConfig} from 'axios'

// 扩展 axios 配置类型，增加自定义耗时字段
declare module 'axios' {
    interface InternalAxiosRequestConfig {
        _startTime?: number
    }
}

// 统一飞牛网关前缀
export const API_BASE = '/app/fnnas-pdfreader/api'

export const request: AxiosInstance = axios.create({
    baseURL: API_BASE + '/',
    timeout: 30000, // 大书首次渲染较慢，留足余量
    headers: {},
    // 不主动发 Cache-Control/Pragma，让浏览器按接口 Cache-Control 缓存 meta
})

request.interceptors.request.use(
    (config: InternalAxiosRequestConfig) => {
        config._startTime = Date.now()
        return config
    },
    (error) => Promise.reject(error)
)

request.interceptors.response.use(
    (res: AxiosResponse) => {
        const cost = Date.now() - (res.config._startTime ?? Date.now())
        if (cost > 1000) console.log(`⏱ ${res.config.url} 耗时 ${cost}ms`)
        return res
    },
    (err) => {
        const cost = err.config?._startTime ? Date.now() - err.config._startTime : -1
        console.error(`❌ 请求【${err.config?.url ?? 'unknown'}】失败，耗时 ${cost}ms`, err.message)
        return Promise.reject(err)
    }
)

// ----------------------------------------------------------------------------
// 路径化 API 辅助：后端以「书库内真实路径」为标识（不再用 hash 编码的 id）
// ----------------------------------------------------------------------------

/** 页面图片 URL（不含 pri，便于浏览器按 URL 缓存 7 天） */
export function pageImgUrl(path: string, page: number, dpi?: number) {
    const d = dpi ? `&dpi=${dpi}` : ''
    return `${API_BASE}/pageimg?path=${encodeURIComponent(path)}&page=${page}${d}`
}

/** 可取消的页图请求。pri 走请求头，不进 URL，避免拆散 HTTP 缓存。 */
export function fetchPageImage(path: string, page: number, pri: number, signal: AbortSignal, dpi?: number): Promise<Blob> {
    const headers: Record<string, string> = {}
    if (pri != null) headers['X-Page-Pri'] = String(pri)
    return fetch(pageImgUrl(path, page, dpi), {
        signal,
        credentials: 'same-origin',
        cache: 'default',
        headers,
    }).then((res) => {
        if (!res.ok) throw new Error(`pageimg ${res.status}`)
        return res.blob()
    })
}

/** 书籍/目录路径编码为 hash 路由参数 */
export function encodePathParam(path: string) {
    return encodeURIComponent(path)
}
