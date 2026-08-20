import {fileURLToPath, URL} from 'node:url'
import {defineConfig} from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig(() => {
    // 统一使用飞牛网关前缀
    // 注意：本地开发时直接访问 Go 服务端（同时托管前端 + API），不走 Vite dev server
    // Vite 仅用于构建产物
    const base = '/app/fnnas-pdfreader/'

    return {
        base,
        plugins: [react()],
        build: {
            // 必须锁 CSS 目标：不锁的话压缩器会把 @media (max-width: 639px) 改写成
            // range 语法 (width<=639px)，Safari 16.4 / Chrome 104 以下整条忽略 ——
            // 飞牛手机 App 走系统 WebView，版本偏旧的设备会直接拿到 PC 布局。
            cssTarget: ['chrome87', 'safari14'],
        },
        resolve: {
            alias: {
                '@': fileURLToPath(new URL('./src', import.meta.url))
            },
        },
    }
})
