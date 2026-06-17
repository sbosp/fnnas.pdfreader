import axios from 'axios'

// 统一使用飞牛网关前缀
// Flask 服务端同时托管前端和 API，无需 Vite 代理
const baseURL = '/app/fnnas-pdfreader/api/'

const service = axios.create({
    baseURL,
    timeout: 15000,  // PDF 切片可能较大，增加超时
    headers: {
        'x-trim-userid': '1213123213',
        'x-trim-isadmin': 'true',
        'x-trim-username': 'king',
    },
})

// 请求拦截：记录开始时间
service.interceptors.request.use(config => {
    // 挂载开始时间戳到 config 对象
    config._startTime = Date.now()

    console.log('🚀 发送请求:', {
        url: config.url,
        method: config.method,
        headers: config.headers,
        baseURL: config.baseURL
    })
    return config
})

// 响应拦截：成功时计算耗时
service.interceptors.response.use(res => {
    const startTime = res.config._startTime
    const cost = Date.now() - startTime
    console.log(`✅ 请求【${res.config.url}】耗时: ${cost} ms`, {
        status: res.status,
        statusText: res.statusText,
        data: res.data,
        headers: res.headers
    })
    return res
}, err => {
    // 异常也统计耗时
    let cost = -1
    if (err.config && err.config._startTime) {
        cost = Date.now() - err.config._startTime
    }
    console.error(`❌ 请求【${err.config.url}】异常，耗时: ${cost} ms`, {
        message: err.message,
        code: err.code,
        response: err.response ? {
            status: err.response.status,
            statusText: err.response.statusText,
            data: err.response.data
        } : null
    })
    return Promise.reject(err)
})

export default service