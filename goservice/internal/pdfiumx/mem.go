package pdfiumx

import (
	"runtime/debug"
)

func freeOSMemory() {
	debug.FreeOSMemory()
}
