package pdfpipe

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// DiskCache 页图 + meta 磁盘缓存（与书同盘隐藏目录）。
// 新缓存写 WebP；读时优先 WebP，兼容旧 JPEG。
type DiskCache struct {
	DataDir string
	RootOf  func(pdfPath string) string
	mu      sync.Mutex
}

const cacheDirName = ".pdfreader-cache"

const (
	ContentTypeWebP = "image/webp"
	ContentTypeJPEG = "image/jpeg"
)

type metaJSON struct {
	PageCount int     `json:"pageCount"`
	Width     float64 `json:"width"`
	Height    float64 `json:"height"`
}

// CachedImage 磁盘命中的一页图
type CachedImage struct {
	Data        []byte
	ContentType string
}

func (c *DiskCache) root(pdfPath string) string {
	if r := c.RootOf(pdfPath); r != "" {
		return filepath.Join(r, cacheDirName)
	}
	return filepath.Join(c.DataDir, "cache")
}

func (c *DiskCache) key(pdfPath string) string {
	rel := filepath.Base(pdfPath)
	if r := c.RootOf(pdfPath); r != "" {
		if x, err := filepath.Rel(r, pdfPath); err == nil {
			rel = x
		}
	}
	var b strings.Builder
	for _, ch := range rel {
		switch ch {
		case '/', '\\', ':', '*', '?', '"', '<', '>', '|':
			b.WriteByte('_')
		default:
			b.WriteRune(ch)
		}
	}
	sig := "0-0"
	if st, err := os.Stat(pdfPath); err == nil {
		sig = fmt.Sprintf("%d-%d", st.ModTime().Unix(), st.Size())
	}
	return b.String() + "." + sig
}

func (c *DiskCache) metaPath(pdfPath string) string {
	return filepath.Join(c.root(pdfPath), c.key(pdfPath), "meta.json")
}

func (c *DiskCache) webpPath(pdfPath string, page1, dpi int) string {
	return filepath.Join(c.root(pdfPath), c.key(pdfPath), fmt.Sprintf("%d", dpi), fmt.Sprintf("page-%d.webp", page1))
}

func (c *DiskCache) jpegPath(pdfPath string, page1, dpi int) string {
	return filepath.Join(c.root(pdfPath), c.key(pdfPath), fmt.Sprintf("%d", dpi), fmt.Sprintf("page-%d.jpg", page1))
}

func (c *DiskCache) LoadMeta(pdfPath string) *DocMeta {
	data, err := os.ReadFile(c.metaPath(pdfPath))
	if err != nil {
		return nil
	}
	var raw metaJSON
	if json.Unmarshal(data, &raw) != nil || raw.PageCount <= 0 {
		return nil
	}
	return &DocMeta{PageCount: raw.PageCount, Width: raw.Width, Height: raw.Height}
}

func (c *DiskCache) SaveMeta(pdfPath string, m *DocMeta) {
	c.mu.Lock()
	defer c.mu.Unlock()
	fp := c.metaPath(pdfPath)
	_ = os.MkdirAll(filepath.Dir(fp), 0755)
	data, err := json.Marshal(metaJSON{
		PageCount: m.PageCount,
		Width:     m.Width,
		Height:    m.Height,
	})
	if err != nil {
		return
	}
	tmp := fp + ".tmp"
	if os.WriteFile(tmp, data, 0644) == nil {
		_ = os.Rename(tmp, fp)
	}
}

// LoadPage 优先 WebP，其次旧 JPEG（兼容历史缓存）
func (c *DiskCache) LoadPage(pdfPath string, page1, dpi int) (*CachedImage, bool) {
	if data, err := os.ReadFile(c.webpPath(pdfPath, page1, dpi)); err == nil && len(data) > 0 {
		return &CachedImage{Data: data, ContentType: ContentTypeWebP}, true
	}
	if data, err := os.ReadFile(c.jpegPath(pdfPath, page1, dpi)); err == nil && len(data) > 0 {
		return &CachedImage{Data: data, ContentType: ContentTypeJPEG}, true
	}
	return nil, false
}

// SaveWebP 只写新格式；不删除旧 JPEG（可读兼容）
func (c *DiskCache) SaveWebP(pdfPath string, page1, dpi int, data []byte) {
	c.mu.Lock()
	defer c.mu.Unlock()
	fp := c.webpPath(pdfPath, page1, dpi)
	_ = os.MkdirAll(filepath.Dir(fp), 0755)
	tmp := fp + ".tmp"
	if os.WriteFile(tmp, data, 0644) == nil {
		_ = os.Rename(tmp, fp)
	}
}
