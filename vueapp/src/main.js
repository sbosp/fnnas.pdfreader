import './assets/main.css'

import { createApp } from 'vue'
import App from './App.vue'
import router from './router/index.js'

import('vconsole').then(({ default: VConsole }) => {
    new VConsole()
})

const app = createApp(App)
app.use(router) // 使用
app.mount('#app')
