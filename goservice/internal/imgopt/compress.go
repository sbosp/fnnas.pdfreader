package imgopt

import (
	"bytes"
	"fmt"
	"image"

	"github.com/gen2brain/webp"
)

// Mode 压缩模式（均输出 WebP）
type Mode string

const (
	ModeSmart Mode = "smart" // 按档位选更高质量/更努力编码
	ModeFast  Mode = "fast"  // 更快 Method、较低质量
)

// Compressor 光栅化后编码为 WebP
type Compressor struct {
	Mode       Mode
	FastQual   int
	SmartLevel string
}

func New(mode, smartLevel string, fastQual int) *Compressor {
	m := Mode(mode)
	if m != ModeSmart && m != ModeFast {
		m = ModeSmart
	}
	if fastQual < 1 || fastQual > 100 {
		fastQual = 80
	}
	if smartLevel == "" {
		smartLevel = "high"
	}
	return &Compressor{Mode: m, FastQual: fastQual, SmartLevel: smartLevel}
}

func (c *Compressor) Compress(img image.Image) ([]byte, error) {
	if img == nil {
		return nil, fmt.Errorf("nil image")
	}
	q, method := c.params()
	return encodeWebP(img, q, method)
}

func (c *Compressor) params() (quality, method int) {
	if c.Mode == ModeFast {
		method = 2 // 更快
		return c.FastQual, method
	}
	method = 4
	switch c.SmartLevel {
	case "ultra":
		return 90, 5
	case "balanced":
		return 80, 4
	case "aggressive":
		return 70, 4
	default: // high
		return 85, 4
	}
}

func encodeWebP(img image.Image, quality, method int) ([]byte, error) {
	if quality < 1 {
		quality = 75
	}
	if quality > 100 {
		quality = 100
	}
	if method < 0 {
		method = 0
	}
	if method > 6 {
		method = 6
	}
	var buf bytes.Buffer
	if err := webp.Encode(&buf, img, webp.Options{
		Quality: quality,
		Method:  method,
	}); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
