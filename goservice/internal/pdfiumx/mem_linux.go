//go:build linux && cgo

package pdfiumx

/*
#include <malloc.h>
void pdfreader_malloc_trim(void) {
	malloc_trim(0);
}
*/
import "C"

func trimCHeap() {
	C.pdfreader_malloc_trim()
}
