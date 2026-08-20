import {HashRouter, Route, Routes} from 'react-router-dom'
import HomePage from './components/HomePage'
import PdfReader from './components/PdfReader'

// 飞牛应用通过 iframe 加载，用 hash 路由；静态资源 base 由 vite.config 的
// base=/app/fnnas-pdfreader/ 处理，hash 路由本身无需 basename。
//
// 路径化路由：目录与书籍都用「书库内真实路径」作为参数（URL 编码），
// 不再使用 hash 编码的 id —— 便于展示路径导航与直接分享定位。
export default function App() {
    return (
        <HashRouter>
            <Routes>
                <Route path="/" element={<HomePage/>}/>
                <Route path="/browse/*" element={<HomePage/>}/>
                <Route path="/read/*" element={<PdfReader/>}/>
            </Routes>
        </HashRouter>
    )
}
