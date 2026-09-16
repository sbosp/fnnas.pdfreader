package pdfiumx

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"time"

	"github.com/klippa-app/go-pdfium"
	"github.com/klippa-app/go-pdfium/multi_threaded"
)

// Engine 原生 PDFium 多进程池（CGO multi_threaded）。
// 每个 instance 对应一个独立 worker 进程，避开 WASM 线性内存常驻。
type Engine struct {
	pool pdfium.Pool

	maxOps int

	mu       sync.Mutex
	opsCount int
}

type EngineConfig struct {
	// MaxWorkers 同时存活的 worker 上限（=可并行渲页槽位数）
	MaxWorkers int
	// MaxOpsPerWorker 单 instance 建议回收阈值
	MaxOpsPerWorker int
	// WorkerBin pdfium-worker 可执行文件路径；空则自动探测
	WorkerBin string
}

func NewEngine(cfg EngineConfig) (*Engine, error) {
	if cfg.MaxWorkers < 1 {
		cfg.MaxWorkers = 2
	}
	if cfg.MaxOpsPerWorker < 1 {
		cfg.MaxOpsPerWorker = 30
	}
	setupNativeLibPath()

	workerBin := cfg.WorkerBin
	if workerBin == "" {
		workerBin = resolveWorkerBin()
	}
	if st, err := os.Stat(workerBin); err != nil || st.IsDir() {
		return nil, fmt.Errorf("pdfium worker 未找到: %s（请先编译 cmd/pdfium-worker，或设 PDFR_PDFIUM_WORKER）", workerBin)
	}

	pool := multi_threaded.Init(multi_threaded.Config{
		MinIdle:  0,
		MaxIdle:  0,
		MaxTotal: cfg.MaxWorkers,
		Command: multi_threaded.Command{
			BinPath:      workerBin,
			StartTimeout: 60 * time.Second,
		},
	})
	return &Engine{pool: pool, maxOps: cfg.MaxOpsPerWorker}, nil
}

func resolveWorkerBin() string {
	if v := os.Getenv("PDFR_PDFIUM_WORKER"); v != "" {
		return v
	}
	if exe, err := os.Executable(); err == nil {
		cand := filepath.Join(filepath.Dir(exe), "pdfium-worker")
		if st, err := os.Stat(cand); err == nil && !st.IsDir() {
			return cand
		}
	}
	// 开发态：goservice 目录下
	if cwd, err := os.Getwd(); err == nil {
		for _, p := range []string{
			filepath.Join(cwd, "pdfium-worker"),
			filepath.Join(cwd, "bin", "pdfium-worker"),
		} {
			if st, err := os.Stat(p); err == nil && !st.IsDir() {
				return p
			}
		}
	}
	return "pdfium-worker"
}

// setupNativeLibPath 把可执行文件旁的 lib/ 或 PDFR_PDFIUM_LIB 加入动态库搜索路径，
// 以便主进程与 go-plugin 拉起的 worker 子进程都能找到 libpdfium。
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
