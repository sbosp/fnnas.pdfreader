import {fileURLToPath, URL} from 'node:url'

import {defineConfig} from 'vite'
import vue from '@vitejs/plugin-vue'
import vueDevTools from 'vite-plugin-vue-devtools'

// https://vite.dev/config/
export default defineConfig(() => {
    // 统一使用飞牛网关前缀
    // 注意：本地开发时使用 Flask 服务端（同时托管前端 + API），不走 Vite
    // Vite 仅用于构建，开发时直接访问 Flask 服务
    const base = '/app/fnnas-pdfreader/'

    return {
        base,
        plugins: [
            vue(),
            vueDevTools(),
        ],
        resolve: {
            alias: {
                '@': fileURLToPath(new URL('./src', import.meta.url))
            },
        },
    }
})
