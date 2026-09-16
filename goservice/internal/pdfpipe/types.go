package pdfpipe

import "image"

// DocMeta 文档元信息（与前端 /api/meta 对齐）
type DocMeta struct {
	PageCount int
	Width     float64
	Height    float64
}

// PagePDF 单页 PDF 字节（抽页后备路径产物）。
type PagePDF struct {
	Bytes     []byte
	PageIndex int
}

// PageRenderer 直渲：FileReader 打开、按页光栅化。不得整本 ReadFile。
type PageRenderer interface {
	Meta(path string) (DocMeta, error)
	// RenderPageImage 返回页面位图；必须调用 cleanup 释放 PDFium 资源。
	RenderPageImage(path string, pageIndex0, dpi int) (img image.Image, cleanup func(), err error)
	Close() error
}

// ImageCompressor 感知/快速压缩（TinyPNG 同类：保视觉质量、压体积）
type ImageCompressor interface {
	Compress(img image.Image) ([]byte, error)
}

// Extractor 抽页后备（可选，直渲失败时可用）
type Extractor interface {
	Meta(path string) (DocMeta, error)
	ExtractPage(path string, pageIndex0 int) (*PagePDF, error)
	Close() error
}

// Rasterizer 单页 PDF → JPEG 后备
type Rasterizer interface {
	RasterizeJPEG(page *PagePDF, dpi int) ([]byte, error)
	Close() error
}
