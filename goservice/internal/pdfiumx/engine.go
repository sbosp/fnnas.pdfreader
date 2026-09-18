package pdfiumx

import (
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"time"

	"github.com/klippa-app/go-pdfium"
	"github.com/klippa-app/go-pdfium/single_threaded"
)

// Engine 同进程原生 PDFium（CGO single_threaded）。
// 不拉起 pdfium-worker；库内全局锁保证线程安全，渲页在进程内串行。
type Engine struct {
	pool pdfium.Pool

	maxOps int

	mu       sync.Mutex
	opsCount int
}

type EngineConfig struct {
	MaxWorkers      int // 保留字段，single_threaded 下不限制进程数
	MaxOpsPerWorker int
}

func NewEngine(cfg EngineConfig) (*Engine, error) {
	if cfg.MaxOpsPerWorker < 1 {
		cfg.MaxOpsPerWorker = 30
	}
	setupNativeLibPath()

	pool := single_threaded.Init(single_threaded.Config{})
	return &Engine{pool: pool, maxOps: cfg.MaxOpsPerWorker}, nil
}

// setupNativeLibPath 把可执行文件旁的 lib/ 或 PDFR_PDFIUM_LIB 加入动态库搜索路径。
func setupNativeLibPath() {
	libDir := os.Getenv("PDFR_PDFIUM_LIB")
	if libDir == "" {
		if exe, err := os.Executable(); err == nil {
			cand := filepath.Join(filepath.Dir(exe), "lib")
			if st, err := os.Stat(cand); err == nil && st.IsDir() {
				libDir = cand
			}
		}
	}
	if libDir == "" {
		return
	}
	key := "LD_LIBRARY_PATH"
	if runtime.GOOS == "darwin" {
		key = "DYLD_LIBRARY_PATH"
	}
	prev := os.Getenv(key)
	if prev == "" {
		_ = os.Setenv(key, libDir)
	} else {
		_ = os.Setenv(key, libDir+string(os.PathListSeparator)+prev)
	}
}

func (e *Engine) Acquire() (pdfium.Pdfium, error) {
	return e.pool.GetInstance(60 * time.Second)
}

func (e *Engine) CloseInstance(inst pdfium.Pdfium) {
	if inst == nil {
		return
	}
	_ = inst.Close()
	e.mu.Lock()
	e.opsCount++
	n := e.opsCount
	e.mu.Unlock()
	if n%8 == 0 {
		go func() {
			time.Sleep(10 * time.Millisecond)
			freeOSMemory()
		}()
	}
}

func (e *Engine) MaxOps() int { return e.maxOps }

func (e *Engine) Close() error {
	if e.pool != nil {
		return e.pool.Close()
	}
	return nil
}
