import './assets/main.css'

import { createApp } from 'vue'
import App from './App.vue'
import router from './router/index.js'

// 默认不启用 vconsole，暴露一个全局方法用于按需启用
// 在首页连续点击 5 次「用户」按钮后调用 window.__enableVConsole()
let __vconsoleInstance = null
window.__enableVConsole = () => {
    if (__vconsoleInstance) return __vconsoleInstance
    return import('vconsole').then(({ default: VConsole }) => {
        __vconsoleInstance = new VConsole()
        return __vconsoleInstance
    })
}

const app = createApp(App)
app.use(router) // 使用
app.mount('#app')
