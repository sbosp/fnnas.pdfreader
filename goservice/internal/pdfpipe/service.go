package pdfpipe

import (
	"fmt"
	"sync"
	"time"
)

// Config 管道运行参数
type Config struct {
	Concurrency int
	DefaultDPI  int
	JPEGQuality int // 兼容旧配置名：实际用作 WebP 质量（fast / 档位回退）
}

func (c *Config) normalize() {
	if c.Concurrency < 1 {
		c.Concurrency = 10
	}
	if c.Concurrency > 32 {
		c.Concurrency = 32
	}
	if c.DefaultDPI < 36 {
		c.DefaultDPI = 200
	}
	if c.JPEGQuality < 1 || c.JPEGQuality > 100 {
		c.JPEGQuality = 80
	}
}

// Service 直渲 → WebP → 磁盘缓存（读缓存兼容旧 JPEG）
type Service struct {
	Render PageRenderer
	Opt    ImageCompressor
	Cache  *DiskCache
	Cfg    Config

	sem chan struct{}
	_   sync.Mutex
}

func NewService(render PageRenderer, opt ImageCompressor, cache *DiskCache, cfg Config) *Service {
	cfg.normalize()
	return &Service{
		Render: render,
		Opt:    opt,
		Cache:  cache,
		Cfg:    cfg,
		sem:    make(chan struct{}, cfg.Concurrency),
	}
}

func (s *Service) acquire() { s.sem <- struct{}{} }
func (s *Service) release() { <-s.sem }

func (s *Service) Meta(path string) (*DocMeta, error) {
	if m := s.Cache.LoadMeta(path); m != nil {
		return m, nil
	}
	s.acquire()
	defer s.release()

	m, err := s.Render.Meta(path)
	if err != nil {
		return nil, err
	}
	s.Cache.SaveMeta(path, &m)
	return &m, nil
}

func clampDPI(dpi, def int) int {
	if dpi < 36 {
		dpi = def
	}
	if dpi > 400 {
		dpi = 400
	}
	return dpi
}

// RenderPageTimed 磁盘缓存(webp|旧jpeg) → 直渲 → WebP → 写 webp 缓存
func (s *Service) RenderPageTimed(path string, pageIndex0, dpi int) (data []byte, contentType string, renderMs, compressMs int64, fromCache bool, err error) {
	dpi = clampDPI(dpi, s.Cfg.DefaultDPI)
	page1 := pageIndex0 + 1
	if hit, ok := s.Cache.LoadPage(path, page1, dpi); ok {
		return hit.Data, hit.ContentType, 0, 0, true, nil
	}

	s.acquire()
	defer s.release()

	if hit, ok := s.Cache.LoadPage(path, page1, dpi); ok {
		return hit.Data, hit.ContentType, 0, 0, true, nil
	}

	t0 := time.Now()
	img, cleanup, err := s.Render.RenderPageImage(path, pageIndex0, dpi)
	if err != nil {
		return nil, "", 0, 0, false, fmt.Errorf("render page %d: %w", pageIndex0, err)
	}
	renderMs = time.Since(t0).Milliseconds()

	t1 := time.Now()
	out, err := s.Opt.Compress(img)
	cleanup()
	if err != nil {
		return nil, "", renderMs, 0, false, fmt.Errorf("compress page %d: %w", pageIndex0, err)
	}
	compressMs = time.Since(t1).Milliseconds()

	s.Cache.SaveWebP(path, page1, dpi, out)
	return out, ContentTypeWebP, renderMs, compressMs, false, nil
}

// RenderPageJPEGTimed 兼容旧调用名
func (s *Service) RenderPageJPEGTimed(path string, pageIndex0, dpi int) (data []byte, renderMs, compressMs int64, fromCache bool, err error) {
	data, _, renderMs, compressMs, fromCache, err = s.RenderPageTimed(path, pageIndex0, dpi)
	return
}

func (s *Service) Close() error {
	if s.Render != nil {
		return s.Render.Close()
	}
	return nil
}
