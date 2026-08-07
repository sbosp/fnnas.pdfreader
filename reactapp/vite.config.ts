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
        resolve: {
            alias: {
                '@': fileURLToPath(new URL('./src', import.meta.url))
            },
        },
    }
})
