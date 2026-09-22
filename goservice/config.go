package main

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

type Config struct {
	Appname       string
	GatewayPrefix string
	SockPath      string
	WebRoot       string
	DataDir       string
	LogFile       string
	RequireAuth   bool
	RootsFile     string
	Port          int
	Host          string

	RenderConcurrency int
	DocIdleSecs       int
	DocCacheSize      int
	DefaultDPI        int
	CompressMode      string // smart | fast
	CompressLevel     string // ultra | high | balanced | aggressive
	JPEGQuality       int    // fast 模式 / smart 回退
}

var cfg Config

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func envInt(key string, def int) int {
	if v := os.Getenv(key); v != "" {
		if n, err := strconv.Atoi(strings.TrimSpace(v)); err == nil {
			return n
		}
	}
	return def
}

func cwd() string {
	d, _ := os.Getwd()
	return d
}

func loadConfig(port int, host string) Config {
	c := Config{
		Appname:           envOr("PDFR_APPNAME", "pdfreader"),
		GatewayPrefix:     strings.TrimRight(envOr("PDFR_GATEWAY_PREFIX", "/app/fnnas-pdfreader"), "/"),
		SockPath:          envOr("PDFR_SOCK", filepath.Join(cwd(), "app.sock")),
		DataDir:           envOr("PDFR_DATA_DIR", filepath.Join(cwd(), "data")),
		LogFile:           os.Getenv("PDFR_LOGFILE"),
		RequireAuth:       os.Getenv("PDFR_REQUIRE_AUTH") == "1",
		RootsFile:         os.Getenv("PDFR_ROOTS_FILE"),
		Port:              port,
		Host:              host,
		RenderConcurrency: envInt("PDFR_RENDER_CONCURRENCY", 10),
		DocIdleSecs:       envInt("PDFR_DOC_IDLE_SECS", 120),
		DocCacheSize:      envInt("PDFR_DOC_CACHE_SIZE", 3),
		DefaultDPI:        envInt("PDFR_RENDER_DPI", 300),
		CompressMode:      strings.ToLower(envOr("PDFR_COMPRESS", "smart")),
		CompressLevel:     strings.ToLower(envOr("PDFR_COMPRESS_LEVEL", "high")),
		JPEGQuality:       envInt("PDFR_JPEG_QUALITY", 78),
	}
	if c.RenderConcurrency < 1 {
		c.RenderConcurrency = 10
	}
	if c.RenderConcurrency > 32 {
		c.RenderConcurrency = 32
	}
	if c.DocCacheSize < 1 {
		c.DocCacheSize = 3
	}
	if c.DefaultDPI < 36 {
		c.DefaultDPI = 300
	}
	if c.JPEGQuality < 1 || c.JPEGQuality > 100 {
		c.JPEGQuality = 78
	}
	c.WebRoot = resolveWebroot()
	return c
}

func resolveWebroot() string {
	here, _ := os.Executable()
	here = filepath.Dir(here)
	candidates := []string{
		os.Getenv("PDFR_WEBROOT"),
		filepath.Join(here, "ui"),
		filepath.Join(here, "..", "ui"),
		filepath.Join(cwd(), "ui"),
	}
	for _, c := range candidates {
		if c == "" {
			continue
		}
		if fileExists(filepath.Join(c, "index.html")) {
			abs, _ := filepath.Abs(c)
			return abs
		}
	}
	return filepath.Join(here, "ui")
}

func fileExists(p string) bool {
	st, err := os.Stat(p)
	return err == nil && !st.IsDir()
}

func collectRoots() []string {
	seen := map[string]bool{}
	var roots []string
	add := func(p string) {
		p = strings.TrimSpace(p)
		if p == "" {
			return
		}
		st, err := os.Stat(p)
		if err != nil || !st.IsDir() {
			return
		}
		rp, err := filepath.EvalSymlinks(p)
		if err != nil {
			rp = p
		}
		if !seen[rp] {
			seen[rp] = true
			roots = append(roots, rp)
		}
	}
	if cfg.RootsFile != "" {
		if data, err := os.ReadFile(cfg.RootsFile); err == nil {
			for _, line := range strings.Split(string(data), "\n") {
				add(line)
			}
		}
	}
	if len(roots) == 0 {
		for _, key := range []string{"PDFR_SHARE_PATHS", "PDFR_ACCESSIBLE_PATHS"} {
			for _, p := range strings.Split(os.Getenv(key), ":") {
				add(p)
			}
		}
	}
	return roots
}
