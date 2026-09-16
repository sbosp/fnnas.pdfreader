package pdfiumx

import (
	"bytes"
	"fmt"
	"math"

	"github.com/klippa-app/go-pdfium"
	"github.com/klippa-app/go-pdfium/references"
	"github.com/klippa-app/go-pdfium/requests"

	"pdfreader/internal/pdfpipe"
)

// Extractor 用原生 PDFium FilePath 打开源文件，
// ImportPages 抽出单页并 SaveAsCopy 成小 PDF，供光栅化库消费。
//
// 每次调用独立 Acquire/Close instance，不长期占用 worker。
type Extractor struct {
	eng *Engine
}

func NewExtractor(eng *Engine, _ int) *Extractor {
	return &Extractor{eng: eng}
}

func openSource(inst pdfium.Pdfium, path string) (references.FPDF_DOCUMENT, error) {
	opened, err := inst.OpenDocument(&requests.OpenDocument{FilePath: &path})
	if err != nil {
		return "", fmt.Errorf("open pdf: %w", err)
	}
	return opened.Document, nil
}

func (e *Extractor) Meta(path string) (pdfpipe.DocMeta, error) {
	inst, err := e.eng.Acquire()
	if err != nil {
		return pdfpipe.DocMeta{}, err
	}
	defer e.eng.CloseInstance(inst)

	doc, err := openSource(inst, path)
	if err != nil {
		return pdfpipe.DocMeta{}, err
	}
	defer func() {
		_, _ = inst.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: doc})
	}()

	pc, err := inst.FPDF_GetPageCount(&requests.FPDF_GetPageCount{Document: doc})
	if err != nil {
		return pdfpipe.DocMeta{}, err
	}
	w, h := 612.0, 792.0
	if pc.PageCount > 0 {
		if ps, err := inst.FPDF_GetPageSizeByIndex(&requests.FPDF_GetPageSizeByIndex{
			Document: doc,
			Index:    0,
		}); err == nil && ps.Width > 0 && ps.Height > 0 {
			w, h = ps.Width, ps.Height
		}
	}
	return pdfpipe.DocMeta{
		PageCount: pc.PageCount,
		Width:     round1(w),
		Height:    round1(h),
	}, nil
}

// ExtractPage 抽出单页为独立 PDF 字节（0-based 页码）。
func (e *Extractor) ExtractPage(path string, pageIndex0 int) (*pdfpipe.PagePDF, error) {
	inst, err := e.eng.Acquire()
	if err != nil {
		return nil, err
	}
	defer e.eng.CloseInstance(inst)

	doc, err := openSource(inst, path)
	if err != nil {
		return nil, err
	}
	defer func() {
		_, _ = inst.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: doc})
	}()

	pc, err := inst.FPDF_GetPageCount(&requests.FPDF_GetPageCount{Document: doc})
	if err != nil {
		return nil, err
	}
	if pageIndex0 < 0 || pageIndex0 >= pc.PageCount {
		return nil, fmt.Errorf("page %d out of range [0,%d)", pageIndex0, pc.PageCount)
	}

	newDoc, err := inst.FPDF_CreateNewDocument(&requests.FPDF_CreateNewDocument{})
	if err != nil {
		return nil, fmt.Errorf("create doc: %w", err)
	}
	defer func() {
		_, _ = inst.FPDF_CloseDocument(&requests.FPDF_CloseDocument{Document: newDoc.Document})
	}()

	_, err = inst.FPDF_ImportPagesByIndex(&requests.FPDF_ImportPagesByIndex{
		Source:      doc,
		Destination: newDoc.Document,
		PageIndices: []int{pageIndex0},
		Index:       0,
	})
	if err != nil {
		return nil, fmt.Errorf("import page: %w", err)
	}

	var buf bytes.Buffer
	_, err = inst.FPDF_SaveAsCopy(&requests.FPDF_SaveAsCopy{
		Document:   newDoc.Document,
		Flags:      requests.SaveFlagNoIncremental,
		FileWriter: &buf,
	})
	if err != nil {
		return nil, fmt.Errorf("save page pdf: %w", err)
	}
	return &pdfpipe.PagePDF{Bytes: buf.Bytes(), PageIndex: pageIndex0}, nil
}

func (e *Extractor) Close() error { return nil }

func round1(f float64) float64 {
	return math.Round(f*10) / 10
}
