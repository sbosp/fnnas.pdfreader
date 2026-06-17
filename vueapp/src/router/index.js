import {createRouter, createWebHistory, createWebHashHistory} from 'vue-router'
import HomePage from '../components/HomePage.vue'
import PdfReader from '../components/PdfReader.vue'

// 飞牛应用通过 iframe 加载，base 须与 vite.config.js 的构建 base 一致
const routerBase = '/app/fnnas-pdfreader/'

const router = createRouter({
    history: createWebHashHistory(routerBase),
    routes: [
        {
            path: '/',
            component: HomePage,
        },
        {
            path: '/folder/:folderId',
            component: HomePage,
        },
        {
            path: '/reader/:bookId',
            component: PdfReader,
        }
    ]
})

export default router