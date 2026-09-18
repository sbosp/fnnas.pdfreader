package main

import (
	"flag"
	"net"
	"net/http"
	"os"
	"strconv"

	"pdfreader/internal/imgopt"
	"pdfreader/internal/pdfiumx"
	"pdfreader/internal/pdfpipe"
)

func main() {
	port := flag.Int("port", 0, "TCP 调试端口（>0 时不用 unix socket）")
	host := flag.String("host", "0.0.0.0", "TCP 监听地址（仅 --port>0 生效）")
	flag.Parse()

	cfg = loadConfig(*port, *host)
	_ = os.MkdirAll(cfg.DataDir, 0755)

	eng, err := pdfiumx.NewEngine(pdfiumx.EngineConfig{
		MaxWorkers:      cfg.RenderConcurrency,
		MaxOpsPerWorker: 40,
	})
	if err != nil {
		logf("FATAL pdfium: %v", err)
		os.Exit(1)
	}
	renderer := pdfiumx.NewRenderer(eng, cfg.DocIdleSecs, cfg.DocCacheSize)
	compressor := imgopt.New(cfg.CompressMode, cfg.CompressLevel, cfg.JPEGQuality)
	pdfSvc = pdfpipe.NewService(renderer, compressor, &pdfpipe.DiskCache{
		DataDir: cfg.DataDir,
		RootOf:  rootOf,
	}, pdfpipe.Config{
		Concurrency: cfg.RenderConcurrency,
		DefaultDPI:  cfg.DefaultDPI,
		JPEGQuality: cfg.JPEGQuality,
	})

	logf("=== pdfreader go server boot === prefix=%s webroot=%s data=%s",
		cfg.GatewayPrefix, cfg.WebRoot, cfg.DataDir)
	logf("pipeline: 同进程原生PDFium(FilePath)直渲 → WebP(%s/%s) | concurrency=%d dpi=%d docCache=%d | 无 pdfium-worker",
		cfg.CompressMode, cfg.CompressLevel, cfg.RenderConcurrency, cfg.DefaultDPI, cfg.DocCacheSize)
	logf("roots=%v", collectRoots())

	mux := http.NewServeMux()
	p := cfg.GatewayPrefix
	register := func(path string, h http.HandlerFunc) {
		mux.HandleFunc(p+path, h)
		mux.HandleFunc(path, h)
	}
	register("/api/me", requireUser(handleMe))
	register("/api/list", requireUser(handleList))
	register("/api/recent", requireUser(handleRecent))
	register("/api/meta", requireUser(handleMeta))
	register("/api/pageimg", requireUser(handlePageImage))
	register("/api/progress", requireUser(handleProgress))
	mux.HandleFunc(p+"/", serveStatic)
	mux.HandleFunc("/", serveStatic)

	if cfg.Port > 0 {
		addr := net.JoinHostPort(cfg.Host, strconv.Itoa(cfg.Port))
		logf("TCP debug mode on %s", addr)
		if err := http.ListenAndServe(addr, mux); err != nil {
			logf("FATAL: %v", err)
			os.Exit(1)
		}
		return
	}

	_ = os.Remove(cfg.SockPath)
	ln, err := net.Listen("unix", cfg.SockPath)
	if err != nil {
		logf("FATAL listen unix %s: %v", cfg.SockPath, err)
		os.Exit(1)
	}
	_ = os.Chmod(cfg.SockPath, 0666)
	logf("listening unix socket: %s", cfg.SockPath)
	if err := http.Serve(ln, mux); err != nil {
		logf("FATAL serve: %v", err)
		os.Exit(1)
	}
}
