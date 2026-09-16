package pdfiumx

import (
	"fmt"
	"image"
	"sync"
	"time"

	"github.com/klippa-app/go-pdfium"
	"github.com/klippa-app/go-pdfium/references"
	"github.com/klippa-app/go-pdfium/requests"

	"pdfreader/internal/pdfpipe"
)

// Renderer 直渲：原生 PDFium 按 FilePath 打开 + 按页光栅化。
// multi_threaded 下 FilePath 由 worker 进程直接读盘，避免跨进程传整本。
// 同书短缓存；同书请求串行（instance 非并发安全）；异书可并行。
type Renderer struct {
	eng      *Engine
	idle     time.Duration
	maxCache int

	mu    sync.Mutex
	cache map[string]*cachedDoc
	order []string
}

type cachedDoc struct {
	path string
	inst pdfium.Pdfium
	doc  references.FPDF_DOCUMENT

	mu     sync.Mutex
	ops    int
	refs   int // 正在使用的请求数（持 r.mu 时改）
	last   time.Time
	closed bool
}

func NewRenderer(eng *Engine, idleSecs, maxCache int) *Renderer {
	if idleSecs < 5 {
		idleSecs = 120
	}
	if maxCache < 1 {
		maxCache = 3
	}
	r := &Renderer{
		eng:      eng,
		idle:     time.Duration(idleSecs) * time.Second,
		maxCache: maxCache,
		cache:    make(map[string]*cachedDoc),
	}
	go r.reaper()
	return r
}

func (r *Renderer) reaper() {
	t := time.NewTicker(r.idle / 2)
	defer t.Stop()
	for range t.C {
		r.mu.Lock()
		var victims []*cachedDoc
		now := time.Now()
		for path, d := range r.cache {
			if d.refs == 0 && now.Sub(d.last) > r.idle {
				delete(r.cache, path)
				r.removeOrderLocked(path)
				victims = append(victims, d)
			}
		}
		r.mu.Unlock()
		for _, d := range victims {
			r.destroyDoc(d)
		}
		if len(victims) > 0 {
			freeOSMemory()
		}
	}
}

func (r *Renderer) removeOrderLocked(path string) {
	for i, p := range r.order {
		if p == path {
			r.order = append(r.order[:i], r.order[i+1:]...)
			return
		}
	}
}

func (r *Renderer) touchLocked(path string) {
	r.removeOrderLocked(path)
	r.order = append(r.order, path)
}

func (r *Renderer) destroyDoc(d *cachedDoc) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.closed {
		return
	}
	d.closed = true
	_, _ = d.inst.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: d.doc})
	r.eng.CloseInstance(d.inst)
}

// acquireDoc 返回已 Lock 的 doc；调用方负责 Unlock（或经 cleanup）。
func (r *Renderer) acquireDoc(path string) (*cachedDoc, error) {
	r.mu.Lock()
	if d, ok := r.cache[path]; ok && !d.closed && d.ops < r.eng.MaxOps() {
		d.refs++
		r.touchLocked(path)
		d.last = time.Now()
		r.mu.Unlock()
		d.mu.Lock()
		if !d.closed {
			return d, nil
		}
		// 极少见：拿到后已被关
		d.mu.Unlock()
		r.mu.Lock()
		d.refs--
		r.mu.Unlock()
	} else {
		r.mu.Unlock()
	}
	return r.openFresh(path)
}

func (r *Renderer) releaseRef(d *cachedDoc) {
	r.mu.Lock()
	d.refs--
	if d.refs < 0 {
		d.refs = 0
	}
	over := d.ops >= r.eng.MaxOps()
	path := d.path
	if over && d.refs == 0 {
		if r.cache[path] == d {
			delete(r.cache, path)
			r.removeOrderLocked(path)
		}
		r.mu.Unlock()
		r.destroyDoc(d)
		return
	}
	r.mu.Unlock()
}

func (r *Renderer) openFresh(path string) (*cachedDoc, error) {
	// 腾槽：只赶 refs==0 的
	r.mu.Lock()
	for len(r.cache) >= r.maxCache {
		evicted := false
		for _, p := range r.order {
			d := r.cache[p]
			if d != nil && d.refs == 0 {
				delete(r.cache, p)
				r.removeOrderLocked(p)
				r.mu.Unlock()
				r.destroyDoc(d)
				r.mu.Lock()
				evicted = true
				break
			}
		}
		if !evicted {
			break // 全在用，允许短暂超过 maxCache
		}
	}
	r.mu.Unlock()

	inst, err := r.eng.Acquire()
	if err != nil {
		return nil, err
	}
	opened, err := inst.OpenDocument(&requests.OpenDocument{FilePath: &path})
	if err != nil {
		r.eng.CloseInstance(inst)
		return nil, fmt.Errorf("open pdf: %w", err)
	}
	d := &cachedDoc{
		path: path,
		inst: inst,
		doc:  opened.Document,
		refs: 1,
		last: time.Now(),
	}
	d.mu.Lock()

	r.mu.Lock()
	// 若并发已有同 path，关掉新建的，改用已有的
	if exist, ok := r.cache[path]; ok && !exist.closed {
		exist.refs++
		r.touchLocked(path)
		exist.last = time.Now()
		r.mu.Unlock()
		d.mu.Unlock()
		d.refs = 0
		r.destroyDoc(d)
		exist.mu.Lock()
		if !exist.closed {
			return exist, nil
		}
		exist.mu.Unlock()
		r.mu.Lock()
		exist.refs--
		r.mu.Unlock()
		return r.openFresh(path)
	}
	r.cache[path] = d
	r.touchLocked(path)
	r.mu.Unlock()
	return d, nil
}

func (r *Renderer) Meta(path string) (pdfpipe.DocMeta, error) {
	d, err := r.acquireDoc(path)
	if err != nil {
		return pdfpipe.DocMeta{}, err
	}
	defer func() {
		d.mu.Unlock()
		r.releaseRef(d)
	}()

	pc, err := d.inst.FPDF_GetPageCount(&requests.FPDF_GetPageCount{Document: d.doc})
	if err != nil {
		return pdfpipe.DocMeta{}, err
	}
	w, h := 612.0, 792.0
	if pc.PageCount > 0 {
		if ps, err := d.inst.FPDF_GetPageSizeByIndex(&requests.FPDF_GetPageSizeByIndex{
			Document: d.doc, Index: 0,
		}); err == nil && ps.Width > 0 && ps.Height > 0 {
			w, h = ps.Width, ps.Height
		}
	}
	d.ops++
	d.last = time.Now()
	return pdfpipe.DocMeta{PageCount: pc.PageCount, Width: round1(w), Height: round1(h)}, nil
}

func (r *Renderer) RenderPageImage(path string, pageIndex0, dpi int) (image.Image, func(), error) {
	if dpi < 36 {
		dpi = 72
	}
	d, err := r.acquireDoc(path)
	if err != nil {
		return nil, nil, err
	}

	pc, err := d.inst.FPDF_GetPageCount(&requests.FPDF_GetPageCount{Document: d.doc})
	if err != nil {
		d.mu.Unlock()
		r.releaseRef(d)
		return nil, nil, err
	}
	if pageIndex0 < 0 || pageIndex0 >= pc.PageCount {
		d.mu.Unlock()
		r.releaseRef(d)
		return nil, nil, fmt.Errorf("page %d out of range [0,%d)", pageIndex0, pc.PageCount)
	}

	pr, err := d.inst.RenderPageInDPI(&requests.RenderPageInDPI{
		DPI: dpi,
		Page: requests.Page{ByIndex: &requests.PageByIndex{
			Document: d.doc,
			Index:    pageIndex0,
		}},
	})
	if err != nil {
		d.mu.Unlock()
		r.releaseRef(d)
		return nil, nil, fmt.Errorf("render: %w", err)
	}
	d.ops++
	d.last = time.Now()

	done := false
	cleanup := func() {
		if done {
			return
		}
		done = true
		pr.Cleanup()
		d.mu.Unlock()
		r.releaseRef(d)
	}
	return pr.Result.Image, cleanup, nil
}

func (r *Renderer) Close() error {
	r.mu.Lock()
	victims := make([]*cachedDoc, 0, len(r.cache))
	for p, d := range r.cache {
		delete(r.cache, p)
		victims = append(victims, d)
	}
	r.order = nil
	r.mu.Unlock()
	for _, d := range victims {
		r.destroyDoc(d)
	}
	return nil
}
