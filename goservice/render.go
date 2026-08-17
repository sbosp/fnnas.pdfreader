package main

import (
	"bytes"
	"fmt"
	"image/jpeg"
	"net/http"
	"os"
	"path/filepath"
	"runtime/debug"
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
		startDocIdleReaper() // 启动 doc 空闲超时清理（客户端离开/断开后自动释放内存）
	})
	return pdfiumErr
}

// parseDocMetaPdfium 用 go-pdfium 解析页数 + 页面尺寸。复用渲染的 doc 缓存（getRenderDoc），
// 且只取第一页尺寸、所有页共用同一 ratio —— 逐页调 FPDF_GetPageSizeByIndex 在 WASM 下每次
// 都要跨 Go-WASM 边界，373 页就是 373 次调用、远超前端 10s 超时；而绝大多数书页面尺寸统一，
// 前端 img onLoad 会按 naturalWidth/Height 校正每页真实 ratio，无需逐页精确。
func parseDocMetaPdfium(pdfPath, bid string) (*docMeta, error) {
	renderLock.Lock()
	defer renderLock.Unlock()

	rd, err := getRenderDoc(pdfPath, bid)
	if err != nil {
		return nil, err
	}

	pc, err := rd.instance.FPDF_GetPageCount(&requests.FPDF_GetPageCount{Document: rd.doc})
	if err != nil {
		return nil, err
	}

	w, h := 612.0, 792.0
	if pc.PageCount > 0 {
		if ps, err := rd.instance.FPDF_GetPageSizeByIndex(&requests.FPDF_GetPageSizeByIndex{Document: rd.doc, Index: 0}); err == nil && ps.Width > 0 && ps.Height > 0 {
			w, h = ps.Width, ps.Height
		}
	}
	m := &docMeta{PageCount: pc.PageCount, Pages: make([]PageSize, pc.PageCount)}
	for i := range m.Pages {
		m.Pages[i] = PageSize{W: round1(w), H: round1(h)}
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
	rendered int       // 该 instance 已渲染页数
	lastUsed time.Time // 最后使用时间（空闲超时清理用）
}

var (
	curDoc     *renderDoc
	docCacheMu sync.Mutex
	renderLock sync.Mutex // 渲染互斥：一次只渲染一页
)

// docIdleTimeout doc 空闲超时：超过此时间没有任何渲染/meta 请求（视为客户端已离开/断开），
// 后台 reaper 自动关闭 instance 释放内存。HTTP 无状态、无真正「连接断开」事件，用空闲超时近似。
// 读 PDFR_DOC_IDLE_SECS 环境变量（默认 120s，最小 5s），便于运维调参/测试。
func docIdleTimeout() time.Duration {
	if s := os.Getenv("PDFR_DOC_IDLE_SECS"); s != "" {
		if v, err := strconv.Atoi(s); err == nil && v >= 5 {
			return time.Duration(v) * time.Second
		}
	}
	return 120 * time.Second
}

// closeCurDocLocked 关闭当前 doc 并释放 instance（销毁 WASM worker 释放内存）。调用方须已持 docCacheMu。
func closeCurDocLocked() {
	if curDoc == nil {
		return
	}
	curDoc.instance.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: curDoc.doc})
	curDoc.instance.Close()
	curDoc = nil
}

// startDocIdleReaper 启动后台 goroutine，定期清理空闲超 docIdleTimeout 的 doc。
// 先持 renderLock 再清理，确保不在渲染中途关 instance（避免渲染用到已关闭的 instance 崩溃）。
// 检查间隔取 docIdleTimeout/2（最小 2s），保证空闲超时后能在一个间隔内被发现清理。
func startDocIdleReaper() {
	go func() {
		interval := docIdleTimeout() / 2
		if interval < 2*time.Second {
			interval = 2 * time.Second
		}
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for range ticker.C {
			renderLock.Lock()
			docCacheMu.Lock()
			if curDoc != nil && time.Since(curDoc.lastUsed) > docIdleTimeout() {
				logf("doc 空闲超 %v，自动清理释放内存 (bookId=%s)", docIdleTimeout(), curDoc.bid)
				closeCurDocLocked()
				// 强制 Go runtime 把空闲内存归还 OS。否则 instance.Close 后 Go heap / Wazero 内存池
				// 仍保留这些页，进程 footprint 不会下降（逻辑已清理但物理内存没释放）。
				debug.FreeOSMemory()
			}
			docCacheMu.Unlock()
			renderLock.Unlock()
		}
	}()
}

// getRenderDoc 返回当前书的已打开文档。同书且未超渲染上限则复用（不重新读文件/解析/初始化）；
// 切换书或超限时关闭旧 instance（销毁 WASM worker、释放累积内存）后重建。
func getRenderDoc(pdfPath, bid string) (*renderDoc, error) {
	docCacheMu.Lock()
	defer docCacheMu.Unlock()

	if curDoc != nil && curDoc.bid == bid && curDoc.rendered < maxRenderPerInstance {
		curDoc.lastUsed = time.Now() // 复用：刷新空闲计时
		return curDoc, nil
	}
	closeCurDocLocked()
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
	curDoc = &renderDoc{bid: bid, instance: instance, doc: doc.Document, lastUsed: time.Now()}
	return curDoc, nil
}

// ----------------------------------------------------------------------------
// 图片磁盘缓存：存到书库根目录下的隐藏目录 {书库根}/.pdfreader-cache/{bid}/{dpi}/page-N.jpg。
// 与书籍同盘（通常是大容量存储），隐藏目录不干扰书库浏览；无书库根时回退 DATA_DIR/images。
// ----------------------------------------------------------------------------

func imageCacheRoot() string {
	roots := collectRoots()
	if len(roots) > 0 {
		return filepath.Join(roots[0], ".pdfreader-cache")
	}
	return filepath.Join(cfg.DataDir, "images")
}

// imagePath 第 page1 页（1-based）对应 dpi 的图片缓存文件
func imagePath(bid string, page1 int, dpi int) string {
	return filepath.Join(imageCacheRoot(), bid, strconv.Itoa(dpi), fmt.Sprintf("page-%d.jpg", page1))
}

// renderOnePage 实时渲染单页，写盘缓存后返回 JPEG 字节。
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

	data := buf.Bytes()
	// 写盘缓存（书库隐藏目录），下次同页同 dpi 直接读盘
	ip := imagePath(bid, page1, dpi)
	if err := os.MkdirAll(filepath.Dir(ip), 0755); err == nil {
		if err := os.WriteFile(ip, data, 0644); err != nil {
			logf("图片缓存写盘失败 %s: %v", ip, err)
		}
	}
	return data, nil
}

// ----------------------------------------------------------------------------
// 单页图片接口：命中磁盘缓存（书库隐藏目录 .pdfreader-cache）直接读，未命中实时渲染并写盘。
// 双层缓存复用：后端磁盘缓存（持久）+ 前端浏览器 HTTP 缓存（Cache-Control immutable）。
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

	page1 := page + 1
	ip := imagePath(bid, page1, dpi)
	data, err := os.ReadFile(ip) // 命中磁盘缓存（书库隐藏目录）直接读
	if err != nil {
		// 未命中：实时渲染（renderOnePage 内部已写盘缓存）
		data, err = renderOnePage(entry.Path, bid, page1, dpi)
		if err != nil {
			logf("渲染失败 %s p%d: %v", entry.Name, page1, err)
			http.Error(w, "render failed", http.StatusInternalServerError)
			return
		}
	}

	logf("%s 实时渲染第 %d 页图片(dpi=%d)耗时 %.3f 秒", bid, page, dpi, time.Since(start).Seconds())
	// 图片内容不变（同书同页同 DPI 渲染结果固定），用 30 天 + immutable 让浏览器彻底复用、
	// 连 304 验证请求都不发。后端不落盘。注意：覆盖同名同路径 PDF 后需清缓存（bookId 不变）。
	w.Header().Set("Content-Type", "image/jpeg")
	w.Header().Set("Cache-Control", "max-age=2592000, immutable")
	w.Write(data)
}
