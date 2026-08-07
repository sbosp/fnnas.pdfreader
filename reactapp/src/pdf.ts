import * as pdfjsLib from 'pdfjs-dist'
// @ts-ignore vite worker 导入
import PdfWorker from 'pdfjs-dist/build/pdf.worker.min.mjs?worker'

// pdf.js worker：用 Vite 的 ?worker 方式打包，随应用发布，不依赖外网 CDN
pdfjsLib.GlobalWorkerOptions.workerPort = new PdfWorker()

export {pdfjsLib}

// canvas 渲染上限：手机多为 3 倍屏，钳到 3 才够锐利（钳 2 会糊）；更高收益极小却翻倍内存。
export const MAX_DPR = 3

/**
 * 把单页 PDF 切片（ArrayBuffer，仅 1 页）按目标宽度渲染成 dataURL（用于书架封面缩略图）。
 * 用完即销毁 doc，不常驻内存。
 */
export async function renderPdfSliceToDataUrl(data: ArrayBuffer, targetWidth: number): Promise<string> {
    // 关键：data.slice(0) 复制后再交给 pdf.js。getDocument 会 detach 传入的 ArrayBuffer
    // （转移给 worker），而 data 常是下载调度器缓存的共享 buffer —— 不复制会把缓存 detach 成
    // 空 buffer，导致后续命中同一切片（如阅读页第 0 页与封面同 URL）时拿到空数据渲染失败。
    const doc = await pdfjsLib.getDocument({data: data.slice(0), disableAutoFetch: true, disableStream: true}).promise
    try {
        const page = await doc.getPage(1)
        const baseViewport = page.getViewport({scale: 1})
        const renderScale = (targetWidth / baseViewport.width) * Math.min(window.devicePixelRatio || 1, MAX_DPR)
        const viewport = page.getViewport({scale: renderScale})
        const canvas = document.createElement('canvas')
        canvas.width = Math.floor(viewport.width)
        canvas.height = Math.floor(viewport.height)
        const ctx = canvas.getContext('2d')
        if (!ctx) throw new Error('no canvas ctx')
        await page.render({canvasContext: ctx, viewport}).promise
        page.cleanup()
        return canvas.toDataURL('image/jpeg', 0.82)
    } finally {
        try {
            doc.destroy()
        } catch { /* ignore */ }
    }
}
