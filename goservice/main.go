// PDF 阅读器 — Go 后端（fnOS 统一网关版）
//
// 设计要点：
//   - 标准库 net/http + pdfcpu(纯 Go)，单二进制、免运行时依赖、交叉编译 aarch64。
//   - 预切片架构：书籍首次打开时后台把整本拆成单页小 PDF 缓存到磁盘，
//     之后 /api/pagepdf 直接 ServeFile，运行时零重复解析整本、内存趋近静态服务器。
//   - 与 fnOS cmd/main 注入的 PDFR_* 环境变量完全对齐，监听 Unix Socket(生产)/TCP(调试)。
package main

import (
	"crypto/sha1"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

// ----------------------------------------------------------------------------
// 配置（由 PDFR_* 环境变量注入，与 fnOS cmd/main 对齐）
// ----------------------------------------------------------------------------
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
}

var cfg Config

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func cwd() string {
	d, _ := os.Getwd()
	return d
}

// resolveWebroot 查找前端构建产物目录（含 index.html）
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

// collectRoots 书库根目录：优先读 RootsFile（每行一个路径，支持运行期授权目录变更），
// 否则回退 PDFR_SHARE_PATHS / PDFR_ACCESSIBLE_PATHS（':' 分隔）。过滤非目录并 realpath 去重。
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

// ----------------------------------------------------------------------------
// 日志：同时写 stderr 与 PDFR_LOGFILE
// ----------------------------------------------------------------------------
var logMu sync.Mutex

func logf(format string, a ...any) {
	line := fmt.Sprintf("[pdfreader %s] %s\n", time.Now().Format("2006-01-02 15:04:05"), fmt.Sprintf(format, a...))
	logMu.Lock()
	defer logMu.Unlock()
	fmt.Fprint(os.Stderr, line)
	if cfg.LogFile != "" {
		if f, err := os.OpenFile(cfg.LogFile, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644); err == nil {
			f.WriteString(line)
			f.Close()
		}
	}
}

// ----------------------------------------------------------------------------
// 工具
// ----------------------------------------------------------------------------

// hashID 基于绝对路径生成稳定 bookId（sha1 前 16 hex）
func hashID(absPath string) string {
	sum := sha1.Sum([]byte(absPath))
	return hex.EncodeToString(sum[:])[:16]
}

// safeName 把 uid 清洗成安全文件名
func safeName(uid string) string {
	var b strings.Builder
	for _, r := range uid {
		if r >= '0' && r <= '9' || r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r == '-' || r == '_' {
			b.WriteRune(r)
		}
	}
	if b.Len() == 0 {
		return "anon"
	}
	return b.String()
}

// ----------------------------------------------------------------------------
// 鉴权：读取网关注入的 X-Trim-* 头
// ----------------------------------------------------------------------------
type User struct {
	UID      string
	Username string
	IsAdmin  bool
}

func userFrom(r *http.Request) *User {
	uid := r.Header.Get("X-Trim-Userid")
	isAdmin := r.Header.Get("X-Trim-Isadmin") == "true"
	username := r.Header.Get("X-Trim-Username")
	if cfg.RequireAuth && uid == "" {
		return nil
	}
	if uid == "" {
		uid = "debug"
	}
	if username == "" {
		username = uid
	}
	return &User{UID: uid, Username: username, IsAdmin: isAdmin}
}

// requireUser 鉴权中间件：所有 /api/* 必须携带有效用户
func requireUser(next func(http.ResponseWriter, *http.Request, *User)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		u := userFrom(r)
		if u == nil {
			http.Error(w, "Forbidden: gateway auth required", http.StatusForbidden)
			return
		}
		next(w, r, u)
	}
}

// ----------------------------------------------------------------------------
// 静态资源（带网关前缀剥离 + 防目录穿透）
// ----------------------------------------------------------------------------
var mimeTypes = map[string]string{
	".html": "text/html; charset=utf-8",
	".js":   "application/javascript; charset=utf-8",
	".mjs":  "application/javascript; charset=utf-8",
	".css":  "text/css; charset=utf-8",
	".png":  "image/png",
	".svg":  "image/svg+xml",
	".ico":  "image/x-icon",
	".json": "application/json; charset=utf-8",
	".woff": "font/woff",
	".woff2": "font/woff2",
	".ttf":  "font/ttf",
	".map":  "application/json; charset=utf-8",
}

func serveStatic(w http.ResponseWriter, r *http.Request) {
	rel := strings.TrimPrefix(r.URL.Path, cfg.GatewayPrefix)
	rel = strings.TrimPrefix(rel, "/")
	if rel == "" {
		rel = "index.html"
	}
	full := filepath.Join(cfg.WebRoot, filepath.FromSlash(rel))
	// 防目录穿透：必须在 WebRoot 内
	absRoot, _ := filepath.Abs(cfg.WebRoot)
	absFull, _ := filepath.Abs(full)
	if absFull != absRoot && !strings.HasPrefix(absFull, absRoot+string(os.PathSeparator)) {
		http.NotFound(w, r)
		return
	}
	if !fileExists(absFull) {
		// SPA 回退到 index.html
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

// writeJSON 统一 JSON 响应
func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	json.NewEncoder(w).Encode(v)
}

// handleMe 返回当前用户信息
func handleMe(w http.ResponseWriter, r *http.Request, u *User) {
	writeJSON(w, map[string]any{
		"uid":      u.UID,
		"username": u.Username,
		"isAdmin":  u.IsAdmin,
	})
}

// ----------------------------------------------------------------------------
// main
// ----------------------------------------------------------------------------
func main() {
	// 先用局部变量解析 flag，再统一装配 cfg（避免 flag.Parse 后被整 struct 覆盖）
	port := flag.Int("port", 0, "TCP 调试端口（>0 时不用 unix socket）")
	host := flag.String("host", "0.0.0.0", "TCP 监听地址（仅 --port>0 生效）")
	flag.Parse()

	cfg = Config{
		Appname:       envOr("PDFR_APPNAME", "pdfreader"),
		GatewayPrefix: strings.TrimRight(envOr("PDFR_GATEWAY_PREFIX", "/app/fnnas-pdfreader"), "/"),
		SockPath:      envOr("PDFR_SOCK", filepath.Join(cwd(), "app.sock")),
		DataDir:       envOr("PDFR_DATA_DIR", filepath.Join(cwd(), "data")),
		LogFile:       os.Getenv("PDFR_LOGFILE"),
		RequireAuth:   os.Getenv("PDFR_REQUIRE_AUTH") == "1",
		RootsFile:     os.Getenv("PDFR_ROOTS_FILE"),
		Port:          *port,
		Host:          *host,
	}
	cfg.WebRoot = resolveWebroot()
	os.MkdirAll(cfg.DataDir, 0755)

	logf("=== pdfreader go server boot === prefix=%s webroot=%s data=%s", cfg.GatewayPrefix, cfg.WebRoot, cfg.DataDir)
	logf("roots=%v", collectRoots())

	mux := http.NewServeMux()
	p := cfg.GatewayPrefix
	mux.HandleFunc(p+"/api/me", requireUser(handleMe))
	mux.HandleFunc(p+"/api/books", requireUser(handleBooks))
	mux.HandleFunc(p+"/api/meta", requireUser(handleMeta))
	mux.HandleFunc(p+"/api/pageimg", requireUser(handlePageImage))
	mux.HandleFunc(p+"/api/progress", requireUser(handleProgress))
	mux.HandleFunc(p+"/", serveStatic)

	if cfg.Port > 0 {
		addr := net.JoinHostPort(cfg.Host, strconv.Itoa(cfg.Port))
		logf("TCP debug mode on %s", addr)
		if err := http.ListenAndServe(addr, mux); err != nil {
			logf("FATAL: %v", err)
			os.Exit(1)
		}
		return
	}

	// 生产：Unix Socket，供 fnOS 统一网关转发
	os.Remove(cfg.SockPath)
	ln, err := net.Listen("unix", cfg.SockPath)
	if err != nil {
		logf("FATAL listen unix %s: %v", cfg.SockPath, err)
		os.Exit(1)
	}
	// socket 权限 0666，便于 nginx 网关读写
	os.Chmod(cfg.SockPath, 0666)
	logf("listening unix socket: %s", cfg.SockPath)
	if err := http.Serve(ln, mux); err != nil {
		logf("FATAL serve: %v", err)
		os.Exit(1)
	}
}
