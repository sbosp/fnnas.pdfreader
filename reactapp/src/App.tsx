import {HashRouter, Route, Routes} from 'react-router-dom'
import HomePage from './components/HomePage'
import PdfReader from './components/PdfReader'

// 飞牛应用通过 iframe 加载，用 hash 路由；静态资源 base 由 vite.config 的
// base=/app/fnnas-pdfreader/ 处理，hash 路由本身无需 basename。
export default function App() {
    return (
        <HashRouter>
            <Routes>
                <Route path="/" element={<HomePage/>}/>
                <Route path="/folder/:folderId" element={<HomePage/>}/>
                <Route path="/reader/:bookId" element={<PdfReader/>}/>
            </Routes>
        </HashRouter>
    )
}
