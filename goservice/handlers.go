package main

import (
	"encoding/json"
	"net/http"
	"os"
	"strconv"
	"time"

	"pdfreader/internal/pdfpipe"
)

var pdfSvc *pdfpipe.Service

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	_ = json.NewEncoder(w).Encode(v)
}

func handleMe(w http.ResponseWriter, r *http.Request, u *User) {
	writeJSON(w, map[string]any{
		"uid":      u.UID,
		"username": u.Username,
		"isAdmin":  u.IsAdmin,
	})
}

func handleList(w http.ResponseWriter, r *http.Request, u *User) {
	raw := r.URL.Query().Get("path")
	var entries []Entry
	var crumbs []crumb
	curPath := ""

	if raw == "" {
		roots := collectRoots()
		if len(roots) == 1 {
			curPath = roots[0]
			entries = listDir(curPath)
			crumbs = breadcrumb(curPath)
		} else {
			entries = rootsAsEntries()
		}
	} else {
		dir, ok := resolveDir(raw)
		if !ok {
			http.Error(w, "directory not found", http.StatusNotFound)
			return
		}
		curPath = dir
		entries = listDir(dir)
		crumbs = breadcrumb(dir)
	}

	prog := loadProgress(u.UID)
	items := make([]map[string]any, 0, len(entries))
	for _, e := range entries {
		item := map[string]any{
			"name":     e.Name,
			"path":     e.Path,
			"type":     e.Type,
			"size":     e.Size,
			"mtime":    e.Mtime,
			"count":    e.Count,
			"segments": e.Segments,
		}
		if pe, ok := prog[e.Path]; ok {
			item["progress"] = pe
		}
		items = append(items, item)
	}
	writeJSON(w, map[string]any{
		"path":       curPath,
		"breadcrumb": crumbs,
		"items":      items,
		"count":      len(items),
		"username":   u.Username,
	})
}

func handleRecent(w http.ResponseWriter, r *http.Request, u *User) {
	writeJSON(w, map[string]any{"items": recentItems(u.UID, 12)})
}

func handleMeta(w http.ResponseWriter, r *http.Request, u *User) {
	_ = u
	raw := r.URL.Query().Get("path")
	p, ok := resolveInRoots(raw)
	if !ok {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}
	st, err := os.Stat(p)
	if err != nil || st.IsDir() {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}
	meta, err := pdfSvc.Meta(p)
	if err != nil {
		logf("meta error %s: %v", fileNameOf(p), err)
		http.Error(w, "meta failed", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Cache-Control", "no-store, no-cache, must-revalidate")
	writeJSON(w, map[string]any{
		"path":      p,
		"name":      fileNameOf(p),
		"pageCount": meta.PageCount,
		"width":     meta.Width,
		"height":    meta.Height,
	})
}

func handlePageImage(w http.ResponseWriter, r *http.Request, u *User) {
	_ = u
	start := time.Now()
	raw := r.URL.Query().Get("path")
	page, err := strconv.Atoi(r.URL.Query().Get("page"))
	if err != nil {
		http.Error(w, "bad page", http.StatusBadRequest)
		return
	}
	dpi := cfg.DefaultDPI
	if d, err := strconv.Atoi(r.URL.Query().Get("dpi")); err == nil && d > 0 {
		dpi = d
	}

	p, ok := resolveInRoots(raw)
	if !ok {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}
	st, err := os.Stat(p)
	if err != nil || st.IsDir() {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}

	data, contentType, renderMs, compressMs, fromCache, err := pdfSvc.RenderPageTimed(p, page, dpi)
	if err != nil {
		logf("渲染失败 %s p%d: %v", fileNameOf(p), page, err)
		http.Error(w, "render failed", http.StatusInternalServerError)
		return
	}
	if contentType == "" {
		contentType = sniffImageType(data)
	}
	if fromCache {
		logf("缓存命中 %s 第%d页(dpi=%d) %s %dKB 耗时 %.3fs",
			fileNameOf(p), page, dpi, contentType, len(data)/1024, time.Since(start).Seconds())
	} else {
		logf("渲染 %s 第%d页(dpi=%d) %s %dKB render=%dms compress=%dms total=%.3fs",
			fileNameOf(p), page, dpi, contentType, len(data)/1024, renderMs, compressMs, time.Since(start).Seconds())
	}
	w.Header().Set("Content-Type", contentType)
	w.Header().Set("Cache-Control", "no-store, no-cache, must-revalidate")
	_, _ = w.Write(data)
}

func sniffImageType(data []byte) string {
	if len(data) >= 12 && string(data[0:4]) == "RIFF" && string(data[8:12]) == "WEBP" {
		return "image/webp"
	}
	if len(data) >= 3 && data[0] == 0xff && data[1] == 0xd8 && data[2] == 0xff {
		return "image/jpeg"
	}
	return "image/webp"
}
