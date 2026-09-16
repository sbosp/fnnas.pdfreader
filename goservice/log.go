package main

import (
	"fmt"
	"os"
	"sync"
	"time"
)

var logMu sync.Mutex

func logf(format string, a ...any) {
	line := fmt.Sprintf("[pdfreader %s] %s\n", time.Now().Format("2006-01-02 15:04:05"), fmt.Sprintf(format, a...))
	logMu.Lock()
	defer logMu.Unlock()
	fmt.Fprint(os.Stderr, line)
	if cfg.LogFile != "" {
		if f, err := os.OpenFile(cfg.LogFile, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644); err == nil {
			_, _ = f.WriteString(line)
			_ = f.Close()
		}
	}
}
