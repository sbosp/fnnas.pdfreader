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

/** 页面图片 URL（img src 直接用）。后端返回 no-store，复用由后端磁盘缓存负责 */
export function pageImgUrl(path: string, page: number, dpi?: number) {
    const d = dpi ? `&dpi=${dpi}` : ''
    return `${API_BASE}/pageimg?path=${encodeURIComponent(path)}&page=${page}${d}`
}

/** 书籍/目录路径编码为 hash 路由参数 */
export function encodePathParam(path: string) {
    return encodeURIComponent(path)
}
