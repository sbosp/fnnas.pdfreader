package pdfiumx

import (
	"runtime"
	"runtime/debug"
)

func freeOSMemory() {
	ReclaimMemory()
}

// ReclaimMemory 把闲置堆尽量还给 OS：Go 堆 + Linux 上 PDFium/WebP 的 C malloc arena。
func ReclaimMemory() {
	runtime.GC()
	debug.FreeOSMemory()
	trimCHeap()
}
