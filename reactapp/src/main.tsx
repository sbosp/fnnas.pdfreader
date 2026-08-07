import './index.css'
import './components.css'
import ReactDOM from 'react-dom/client'
import App from './App'

// 默认不启用 vconsole，暴露全局方法按需启用（首页连点 5 次「用户」后调用）。
// 注意：不使用 React.StrictMode —— 其开发期双调用 effect 会让 PdfReader 的
// 加载/渲染副作用执行两次，引发重复加载、canvas 重复渲染等问题。
let __vconsoleInstance: any = null
;(window as any).__enableVConsole = () => {
    if (__vconsoleInstance) return __vconsoleInstance
    return import('vconsole').then(({default: VConsole}) => {
        __vconsoleInstance = new VConsole()
        return __vconsoleInstance
    })
}

ReactDOM.createRoot(document.getElementById('root')!).render(<App/>)
