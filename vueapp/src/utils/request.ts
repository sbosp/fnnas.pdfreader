import axios, {AxiosInstance, AxiosRequestConfig, AxiosResponse, InternalAxiosRequestConfig} from 'axios'

// 扩展axios配置类型，增加自定义耗时字段
declare module 'axios' {
    interface InternalAxiosRequestConfig {
        _startTime?: number
    }
}

// 统一飞牛网关前缀
const baseURL = '/app/fnnas-pdfreader/api/'

// 创建axios实例
export const request: AxiosInstance = axios.create({
    baseURL,
    timeout: 10000, // PDF切片体积大，延长超时
    headers: {},
})

// 请求拦截器
request.interceptors.request.use(
    (config: InternalAxiosRequestConfig) => {
        // 挂载请求开始时间戳
        config._startTime = Date.now()

        console.log('🚀 发送请求:', {
            url: config.url,
            method: config.method,
            headers: config.headers,
            baseURL: config.baseURL,
        })
        return config
    },
    (error) => Promise.reject(error)
)

// 响应拦截器 - 成功分支
request.interceptors.response.use(
    (res: AxiosResponse) => {
        const startTime = res.config._startTime ?? Date.now()
        const cost = Date.now() - startTime
        console.log(`✅ 请求【${res.config.url}】耗时: ${cost} ms`, {
            status: res.status,
            statusText: res.statusText,
            data: res.data,
            headers: res.headers,
        })
        return res
    },
    // 响应拦截器 - 异常分支
    (err) => {
        let cost = -1
        if (err.config && err.config._startTime) {
            cost = Date.now() - err.config._startTime
        }
        console.error(`❌ 请求【${err.config?.url ?? 'unknown'}】异常，耗时: ${cost} ms`, {
            message: err.message,
            code: err.code,
            response: err.response
                ? {
                    status: err.response.status,
                    statusText: err.response.statusText,
                    data: err.response.data,
                }
                : null,
        })
        return Promise.reject(err)
    }
)

// ====================== 并发调度核心 ======================
type DownloadTask = {
    url: string
    signal?: AbortSignal
    resolve: (buf: ArrayBuffer) => void
    reject: (err: unknown) => void
    abortHandler?: () => void
}

class DownloadScheduler {
    private readonly MAX_CONCURRENT = 2
    private running = 0
    private queue: DownloadTask[] = []
    private cache = new Map<string, ArrayBuffer>()

    runTask(task: DownloadTask) {
        // 入队前先判断是否已取消，直接拒绝不进队列
        if (task.signal?.aborted) {
            task.reject(new Error('请求已取消'))
            return
        }

        // 缓存取消回调，后续销毁监听
        const abortHandler = () => {
            // 从队列剔除任务
            this.queue = this.queue.filter(t => t !== task)
            task.reject(new Error('请求已取消'))
        }
        task.abortHandler = abortHandler
        task.signal?.addEventListener('abort', abortHandler)

        this.queue.push(task)
        // 多次调用tick无风险
        this.tick()
    }

    private tick() {
        // 循环拉取任务，并发多次调用只会空跑一次判断
        while (this.running < this.MAX_CONCURRENT && this.queue.length) {
            const task = this.queue.shift()!
            this.running++

            // 任务开始执行，移除abort监听，释放内存
            if (task.signal && task.abortHandler) {
                task.signal.removeEventListener('abort', task.abortHandler)
            }

            request.get<ArrayBuffer>(task.url, {
                responseType: 'arraybuffer',
                timeout: 15000,
                signal: task.signal
            })
                .then(res => {
                    this.cache.set(task.url, res.data)
                    task.resolve(res.data)
                })
                .catch(err => task.reject(err))
                .finally(() => {
                    this.running--
                    // 任务完成后再次tick，补队列任务
                    this.tick()
                })
        }
    }

    clearCache() {
        this.cache.clear()
        this.queue = []
    }

    getCache(url: string): ArrayBuffer | undefined {
        return this.cache.get(url)
    }
}

// 全局单例调度器
const downloadScheduler = new DownloadScheduler()

/**
 * 带并发控制的PDF二进制下载
 * @param url 接口相对地址
 * @param signal 取消请求信号
 */
export function download(url: string, signal?: AbortSignal): Promise<ArrayBuffer> {
    // 命中缓存直接返回，不走队列
    const cacheBuf = downloadScheduler.getCache(url)
    if (cacheBuf) return Promise.resolve(cacheBuf)

    return new Promise((resolve, reject) => {
        downloadScheduler.runTask({url, signal, resolve, reject})
    })
}

/** 清空下载缓存 */
export function clearDownloadCache() {
    downloadScheduler.clearCache()
}

// ====================== 图片下载函数（Base64格式） ======================

interface ImageResponse {
  success: boolean
  page: number
  dpi: number
  format: string
  width: number
  height: number
  size: number
  data: string  // Base64 data URL
}

interface BatchImageResponse {
  success: boolean
  dpi: number
  pages: Array<{
    page: number
    data: string | null
    size?: number
    error?: string
  }>
  total_time: number
}

/**
 * 获取单页PDF图片（Base64格式）
 * @param id 书籍ID
 * @param page 页码（1-based）
 * @param dpi 分辨率，默认96
 * @param format 格式，默认png
 */
export function downloadPageImage(id: string, page: number, dpi = 96, format: 'png' | 'jpeg' = 'png'): Promise<ImageResponse> {
  const url = `pageimage?id=${id}&page=${page}&dpi=${dpi}&format=${format}`
  return request.get(url).then(res => res.data)
}

/**
 * 批量获取PDF页面图片
 * @param id 书籍ID
 * @param pages 页码数组（1-based）或范围字符串（如"1-5"）
 * @param dpi 分辨率，默认96
 */
export function downloadPagesBatch(id: string, pages: number[] | string, dpi = 96): Promise<BatchImageResponse> {
  let pagesParam: string
  
  if (Array.isArray(pages)) {
    // 数组转逗号分隔字符串
    pagesParam = pages.join(',')
  } else {
    pagesParam = pages
  }
  
  const url = `pageimages/batch?id=${id}&pages=${pagesParam}&dpi=${dpi}`
  return request.get(url).then(res => res.data)
}

/**
 * 图片下载调度器（专用于Base64图片）
 */
class ImageDownloadScheduler {
  private readonly MAX_CONCURRENT = 3  // 图片下载可以稍微多并发
  private running = 0
  private queue: Array<{
    url: string
    signal?: AbortSignal
    resolve: (data: ImageResponse) => void
    reject: (err: unknown) => void
  }> = []
  private imageCache = new Map<string, ImageResponse>()

  runTask(url: string, signal?: AbortSignal): Promise<ImageResponse> {
    // 检查缓存
    const cached = this.imageCache.get(url)
    if (cached) {
      return Promise.resolve(cached)
    }

    return new Promise((resolve, reject) => {
      this.queue.push({url, signal, resolve, reject})
      this.tick()
    })
  }

  private tick() {
    while (this.running < this.MAX_CONCURRENT && this.queue.length) {
      const task = this.queue.shift()!
      this.running++

      request.get<ImageResponse>(task.url, {
        timeout: 15000,
        signal: task.signal
      })
        .then(res => {
          this.imageCache.set(task.url, res.data)
          task.resolve(res.data)
        })
        .catch(err => task.reject(err))
        .finally(() => {
          this.running--
          this.tick()
        })
    }
  }

  clearCache() {
    this.imageCache.clear()
    this.queue = []
  }

  getCache(url: string): ImageResponse | undefined {
    return this.imageCache.get(url)
  }
}

// 全局图片下载调度器
const imageDownloadScheduler = new ImageDownloadScheduler()

/**
 * 带并发控制的图片下载（Base64格式）
 * @param url 接口相对地址
 * @param signal 取消请求信号
 */
export function downloadImage(url: string, signal?: AbortSignal): Promise<ImageResponse> {
  return imageDownloadScheduler.runTask(url, signal)
}

/** 清空图片缓存 */
export function clearImageCache() {
  imageDownloadScheduler.clearCache()
}