// pdfium-worker：go-pdfium multi_threaded 子进程，内嵌原生 PDFium（CGO）。
package main

import "github.com/klippa-app/go-pdfium/multi_threaded/worker"

func main() {
	worker.StartWorker(nil)
}
