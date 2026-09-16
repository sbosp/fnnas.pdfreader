package main

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
)

var mimeTypes = map[string]string{
	".html":  "text/html; charset=utf-8",
	".js":    "application/javascript; charset=utf-8",
	".mjs":   "application/javascript; charset=utf-8",
	".css":   "text/css; charset=utf-8",
	".png":   "image/png",
	".svg":   "image/svg+xml",
	".ico":   "image/x-icon",
	".json":  "application/json; charset=utf-8",
	".woff":  "font/woff",
	".woff2": "font/woff2",
	".ttf":   "font/ttf",
	".map":   "application/json; charset=utf-8",
}

func serveStatic(w http.ResponseWriter, r *http.Request) {
	rel := strings.TrimPrefix(r.URL.Path, cfg.GatewayPrefix)
	rel = strings.TrimPrefix(rel, "/")
	if rel == "" {
		rel = "index.html"
	}
	full := filepath.Join(cfg.WebRoot, filepath.FromSlash(rel))
	absRoot, _ := filepath.Abs(cfg.WebRoot)
	absFull, _ := filepath.Abs(full)
	if absFull != absRoot && !strings.HasPrefix(absFull, absRoot+string(os.PathSeparator)) {
		http.NotFound(w, r)
		return
	}
	if !fileExists(absFull) {
		absFull = filepath.Join(absRoot, "index.html")
		if !fileExists(absFull) {
			http.Error(w, "前端未构建", http.StatusNotFound)
			return
		}
	}
	if ct, ok := mimeTypes[strings.ToLower(filepath.Ext(absFull))]; ok {
		w.Header().Set("Content-Type", ct)
	}
	http.ServeFile(w, r, absFull)
}
