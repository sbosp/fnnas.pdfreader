package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"time"

	"pdfreader/internal/pdfpipe"
)

const browserCacheMaxAge = 7 * 24 * 3600 // 7 天，给前端 HTTP 缓存

func setBrowserCache(w http.ResponseWriter, etag string) {
	w.Header().Set("Cache-Control", fmt.Sprintf("private, max-age=%d", browserCacheMaxAge))
	w.Header().Set("ETag", etag)
}

func setNoStore(w http.ResponseWriter) {
	w.Header().Set("Cache-Control", "no-store")
}

func etagMatch(r *http.Request, etag string) bool {
	return r.Header.Get("If-None-Match") == etag
}

var pdfSvc *pdfpipe.Service

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	_ = json.NewEncoder(w).Encode(v)
}

func handleMe(w http.ResponseWriter, r *http.Request, u *User) {
	setNoStore(w)
	writeJSON(w, map[string]any{
		"uid":      u.UID,
		"username": u.Username,
		"isAdmin":  u.IsAdmin,
	})
}

func handleList(w http.ResponseWriter, r *http.Request, u *User) {
	setNoStore(w)
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
	setNoStore(w)
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
	etag := fmt.Sprintf(`"meta-%d-%d"`, st.ModTime().Unix(), st.Size())
	if etagMatch(r, etag) {
		setBrowserCache(w, etag)
		w.WriteHeader(http.StatusNotModified)
		return
	}
	meta, err := pdfSvc.Meta(p)
	if err != nil {
		logf("meta error %s: %v", fileNameOf(p), err)
		setNoStore(w)
		http.Error(w, "meta failed", http.StatusInternalServerError)
		return
	}
	setBrowserCache(w, etag)
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
	pri := 10
	if n, err := strconv.Atoi(r.URL.Query().Get("pri")); err == nil {
		pri = n
	}
	if h := r.Header.Get("X-Page-Pri"); h != "" {
		if n, err := strconv.Atoi(h); err == nil {
			pri = n
		}
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

	etag := fmt.Sprintf(`"pimg-%d-%d-%d-%d"`, st.ModTime().Unix(), st.Size(), page, dpi)
	if etagMatch(r, etag) {
		setBrowserCache(w, etag)
		w.WriteHeader(http.StatusNotModified)
		return
	}

	data, contentType, renderMs, compressMs, fromCache, err := pdfSvc.RenderPageTimed(r.Context(), p, page, dpi, pri)
	if err != nil {
		if errors.Is(err, context.Canceled) {
			return
		}
		logf("渲染失败 %s p%d: %v", fileNameOf(p), page, err)
		setNoStore(w)
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
	setBrowserCache(w, etag)
	w.Header().Set("Content-Type", contentType)
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
