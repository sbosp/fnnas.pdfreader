package main

import (
	"bytes"
	"image/jpeg"
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/klippa-app/go-pdfium"
	"github.com/klippa-app/go-pdfium/references"
	"github.com/klippa-app/go-pdfium/requests"
	"github.com/klippa-app/go-pdfium/webassembly"
)

// ----------------------------------------------------------------------------
// PDF 页面渲染成图片（go-pdfium WebAssembly/Wazero 模式）
// 纯 Go、无 CGO、可交叉编译 aarch64 静态链接单二进制；PDFium 为 BSD 许可。
// 实测渲染 22-37ms/页（150-200DPI），初始化一次性 ~1.2s。
// ----------------------------------------------------------------------------

// RENDER_DPI 渲染分辨率：300 清晰度更高（100% 锐利、放大 2x 内仍清晰，手机/电脑都不错），
// 代价是单页更大(复杂页 JPG ~500KB)、渲染更慢(复杂页 ~575ms)、预渲染磁盘占用更高。
const RENDER_DPI = 300

var (
	pdfiumPool pdfium.Pool
	pdfiumOnce sync.Once
	pdfiumErr  error
)

// initPdfium 懒初始化 PDFium WASM pool（进程级单例，纯 Go 无 CGO）。
// MinIdle/MaxIdle=0：instance.Close() 后真正销毁 worker（WASM 实例销毁、内存释放），
// 不保留常驻 idle worker（常驻会让 WASM 线性内存只增不减、最终爆内存）。
func initPdfium() error {
	pdfiumOnce.Do(func() {
		pool, err := webassembly.Init(webassembly.Config{
			MinIdle:  0,
			MaxIdle:  0,
			MaxTotal: 2,
		})
		if err != nil {
			pdfiumErr = err
			return
		}
		pdfiumPool = pool
		logf("PDFium WASM pool 初始化完成")
	})
	return pdfiumErr
}

// parseDocMetaPdfium 用 go-pdfium 解析页数 + 每页尺寸（pt，升序），替代原 pdfcpu 实现。
// 有磁盘缓存，仅首次/缓存失效时执行；go-pdfium 渲染与 meta 统一走同一引擎，后端只依赖 go-pdfium。
func parseDocMetaPdfium(pdfPath string) (*docMeta, error) {
	if err := initPdfium(); err != nil {
		return nil, err
	}
	instance, err := pdfiumPool.GetInstance(time.Second * 60)
	if err != nil {
		return nil, err
	}
	defer instance.Close()

	pdfBytes, err := os.ReadFile(pdfPath)
	if err != nil {
		return nil, err
	}
	doc, err := instance.OpenDocument(&requests.OpenDocument{File: &pdfBytes})
	if err != nil {
		return nil, err
	}
	defer instance.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: doc.Document})

	pc, err := instance.FPDF_GetPageCount(&requests.FPDF_GetPageCount{Document: doc.Document})
	if err != nil {
		return nil, err
	}

	m := &docMeta{PageCount: pc.PageCount, Pages: make([]PageSize, pc.PageCount)}
	for i := 0; i < pc.PageCount; i++ {
		ps, err := instance.FPDF_GetPageSizeByIndex(&requests.FPDF_GetPageSizeByIndex{Document: doc.Document, Index: i})
		if err == nil && ps.Width > 0 && ps.Height > 0 {
			m.Pages[i] = PageSize{W: round1(ps.Width), H: round1(ps.Height)}
		} else {
			m.Pages[i] = PageSize{W: 612.0, H: 792.0}
		}
	}
	return m, nil
}

// ----------------------------------------------------------------------------
// 单页实时渲染（go-pdfium），返回 JPEG 字节。后端不落盘、不缓存 —— 缓存复用交给前端
// 浏览器 HTTP 缓存（Cache-Control）。只渲染实际翻阅到的页，无需整本预渲染。
//
// 性能关键（NAS CPU/内存防爆）：
// 1. 文档句柄缓存：一本书只「读文件 + OpenDocument 解析」一次，翻页复用 doc —— 否则每页
//    都重复读+解析整个 PDF，大书 CPU 直接爆。
// 2. 渲染互斥锁：一次只渲染一页，避免并发渲染的位图内存叠加。
// 3. instance 渲染页数上限：WASM 线性内存不归还 OS，渲染 N 页后关闭重建 instance 释放内存。
// ----------------------------------------------------------------------------

// maxRenderPerInstance 单 instance 渲染页数上限，超过则关闭重建 instance 释放 WASM 内存。
const maxRenderPerInstance = 20

// renderDoc 缓存的已打开文档句柄（doc 属于 instance，二者同生命周期）
type renderDoc struct {
	bid      string
	instance pdfium.Pdfium
	doc      references.FPDF_DOCUMENT
	rendered int // 该 instance 已渲染页数
}

var (
	curDoc     *renderDoc
	docCacheMu sync.Mutex
	renderLock sync.Mutex // 渲染互斥：一次只渲染一页
)

// getRenderDoc 返回当前书的已打开文档。同书且未超渲染上限则复用（不重新读文件/解析/初始化）；
// 切换书或超限时关闭旧 instance（销毁 WASM worker、释放累积内存）后重建。
func getRenderDoc(pdfPath, bid string) (*renderDoc, error) {
	docCacheMu.Lock()
	defer docCacheMu.Unlock()

	if curDoc != nil && curDoc.bid == bid && curDoc.rendered < maxRenderPerInstance {
		return curDoc, nil // 复用：省去读文件 + 解析 + instance 初始化
	}
	if curDoc != nil {
		curDoc.instance.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: curDoc.doc})
		curDoc.instance.Close()
		curDoc = nil
	}
	if err := initPdfium(); err != nil {
		return nil, err
	}
	instance, err := pdfiumPool.GetInstance(time.Second * 60)
	if err != nil {
		return nil, err
	}
	pdfBytes, err := os.ReadFile(pdfPath)
	if err != nil {
		instance.Close()
		return nil, err
	}
	doc, err := instance.OpenDocument(&requests.OpenDocument{File: &pdfBytes})
	if err != nil {
		instance.Close()
		return nil, err
	}
	curDoc = &renderDoc{bid: bid, instance: instance, doc: doc.Document}
	return curDoc, nil
}

// renderOnePage 实时渲染单页，返回 JPEG 字节。
func renderOnePage(pdfPath, bid string, page1 int, dpi int) ([]byte, error) {
	renderLock.Lock()
	defer renderLock.Unlock()

	rd, err := getRenderDoc(pdfPath, bid)
	if err != nil {
		return nil, err
	}

	pr, err := rd.instance.RenderPageInDPI(&requests.RenderPageInDPI{
		DPI:  dpi,
		Page: requests.Page{ByIndex: &requests.PageByIndex{Document: rd.doc, Index: page1 - 1}},
	})
	if err != nil {
		return nil, err
	}
	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, pr.Result.Image, &jpeg.Options{Quality: 82}); err != nil {
		pr.Cleanup()
		return nil, err
	}
	pr.Cleanup() // WASM 模式必须释放图像资源
	rd.rendered++
	return buf.Bytes(), nil
}

// ----------------------------------------------------------------------------
// 单页图片接口：实时渲染该页并直接返回字节。后端不落盘、不缓存 —— 缓存复用交给
// 前端浏览器 HTTP 缓存（Cache-Control）。只渲染实际翻阅到的页。
// ----------------------------------------------------------------------------

func handlePageImage(w http.ResponseWriter, r *http.Request, u *User) {
	start := time.Now()
	bid := r.URL.Query().Get("id")
	page, err := strconv.Atoi(r.URL.Query().Get("page")) // 前端 0-based
	if err != nil {
		http.Error(w, "bad page", http.StatusBadRequest)
		return
	}
	// dpi 可选（默认正文 RENDER_DPI；封面等缩略图传低 dpi 如 80），钳制 36-300
	dpi := RENDER_DPI
	if d, err := strconv.Atoi(r.URL.Query().Get("dpi")); err == nil && d > 0 {
		if d < 36 {
			d = 36
		}
		if d > 300 {
			d = 300
		}
		dpi = d
	}

	entry := loadFileMap(u.UID)[bid]
	if entry == nil || entry.Type != "file" {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}

	data, err := renderOnePage(entry.Path, bid, page+1, dpi)
	if err != nil {
		logf("渲染失败 %s p%d: %v", entry.Name, page+1, err)
		http.Error(w, "render failed", http.StatusInternalServerError)
		return
	}

	logf("%s 实时渲染第 %d 页图片(dpi=%d)耗时 %.3f 秒", bid, page, dpi, time.Since(start).Seconds())
	// 图片内容不变（同书同页同 DPI 渲染结果固定），用 30 天 + immutable 让浏览器彻底复用、
	// 连 304 验证请求都不发。后端不落盘。注意：覆盖同名同路径 PDF 后需清缓存（bookId 不变）。
	w.Header().Set("Content-Type", "image/jpeg")
	w.Header().Set("Cache-Control", "max-age=2592000, immutable")
	w.Write(data)
}
