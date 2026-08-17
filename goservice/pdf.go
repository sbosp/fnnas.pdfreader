package main

import (
	"encoding/json"
	"math"
	"net/http"
	"os"
	"path/filepath"
)

// ----------------------------------------------------------------------------
// 文档元信息（页数 + 每页尺寸），带磁盘缓存。
// 图片方案下，页面渲染与 meta 解析统一走 go-pdfium（见 render.go），不再使用 pdfcpu。
// ----------------------------------------------------------------------------

type PageSize struct {
	W float64 `json:"w"`
	H float64 `json:"h"`
}

type docMeta struct {
	PageCount int        `json:"pageCount"`
	Pages     []PageSize `json:"pages"`
}

func metaDir() string {
	d := filepath.Join(cfg.DataDir, "meta")
	os.MkdirAll(d, 0755)
	return d
}

func metaCachePath(bid string) string {
	return filepath.Join(metaDir(), bid+".json")
}

func loadMetaCache(bid string) *docMeta {
	data, err := os.ReadFile(metaCachePath(bid))
	if err != nil {
		return nil
	}
	var m docMeta
	if json.Unmarshal(data, &m) != nil || m.PageCount <= 0 {
		return nil
	}
	return &m
}

func saveMetaCache(bid string, m *docMeta) {
	data, err := json.Marshal(m)
	if err != nil {
		return
	}
	tmp := metaCachePath(bid) + ".tmp"
	if os.WriteFile(tmp, data, 0644) == nil {
		os.Rename(tmp, metaCachePath(bid))
	}
}

func round1(f float64) float64 {
	return math.Round(f*10) / 10
}

// parseDocMeta 解析页数 + 页面尺寸。实现用 go-pdfium（见 render.go），复用渲染 doc 缓存。
func parseDocMeta(pdfPath, bid string) (*docMeta, error) {
	return parseDocMetaPdfium(pdfPath, bid)
}

func handleMeta(w http.ResponseWriter, r *http.Request, u *User) {
	bid := r.URL.Query().Get("id")
	entry := loadFileMap(u.UID)[bid]
	if entry == nil || entry.Type != "file" {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}

	meta := loadMetaCache(bid)
	if meta == nil {
		m, err := parseDocMeta(entry.Path, bid)
		if err != nil {
			logf("meta error %s: %v", entry.Name, err)
			w.Header().Set("Content-Type", "application/json; charset=utf-8")
			w.WriteHeader(http.StatusInternalServerError)
			json.NewEncoder(w).Encode(map[string]any{"error": "meta_failed", "detail": err.Error()})
			return
		}
		meta = m
		saveMetaCache(bid, meta)
	}

	// 图片方案：后端不预渲染整本，pageimg 请求时实时渲染该页返回（不落盘）。
	// meta 只含页数+尺寸（同书内容不变），用 30 天 + immutable 长缓存；
	// progress（阅读进度，会变）拆到 GET /api/progress 单独实时获取，不随 meta 缓存。
	w.Header().Set("Cache-Control", "max-age=2592000, immutable")
	writeJSON(w, map[string]any{
		"id":        bid,
		"name":      entry.Name,
		"pageCount": meta.PageCount,
		"pages":     meta.Pages,
	})
}
