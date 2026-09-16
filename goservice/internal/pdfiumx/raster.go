package pdfiumx

import (
	"bytes"
	"fmt"
	"image/jpeg"

	"github.com/klippa-app/go-pdfium/requests"

	"pdfreader/internal/pdfpipe"
)

// Rasterizer 将单页 PDF 光栅化为 JPEG。
// 输入已是抽好的小 PDF，可安全以 File/[]byte 打开（体积 ≈ 单页资源，非整本）。
type Rasterizer struct {
	eng     *Engine
	quality int
}

func NewRasterizer(eng *Engine, jpegQuality int) *Rasterizer {
	if jpegQuality < 1 || jpegQuality > 100 {
		jpegQuality = 82
	}
	return &Rasterizer{eng: eng, quality: jpegQuality}
}

func (r *Rasterizer) RasterizeJPEG(page *pdfpipe.PagePDF, dpi int) ([]byte, error) {
	if page == nil || len(page.Bytes) == 0 {
		return nil, fmt.Errorf("empty page pdf")
	}
	if dpi < 36 {
		dpi = 72
	}

	inst, err := r.eng.Acquire()
	if err != nil {
		return nil, err
	}
	defer r.eng.CloseInstance(inst)

	data := page.Bytes
	doc, err := inst.OpenDocument(&requests.OpenDocument{File: &data})
	if err != nil {
		return nil, fmt.Errorf("open page pdf: %w", err)
	}
	defer func() {
		_, _ = inst.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: doc.Document})
	}()

	pr, err := inst.RenderPageInDPI(&requests.RenderPageInDPI{
		DPI: dpi,
		Page: requests.Page{ByIndex: &requests.PageByIndex{
			Document: doc.Document,
			Index:    0,
		}},
	})
	if err != nil {
		return nil, fmt.Errorf("render: %w", err)
	}
	defer pr.Cleanup()

	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, pr.Result.Image, &jpeg.Options{Quality: r.quality}); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func (r *Rasterizer) Close() error { return nil }
