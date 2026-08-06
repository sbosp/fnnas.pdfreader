package main

import (
	"encoding/json"
	"fmt"
	"io"
	"math"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/pdfcpu/pdfcpu/pkg/api"
)

// ----------------------------------------------------------------------------
// 文档元信息（页数 + 每页尺寸），带磁盘缓存
// ----------------------------------------------------------------------------

type PageSize struct {
	W float64 `json:"w"`
	H float64 `json:"h"`
}

type docMeta struct {
	PageCount int        `json:"pageCount"`
	Pages     []PageSize `json:"pages"`
}

func metaDir() string {
	d := filepath.Join(cfg.DataDir, "meta")
	os.MkdirAll(d, 0755)
	return d
}

func metaCachePath(bid string) string {
	return filepath.Join(metaDir(), bid+".json")
}

func loadMetaCache(bid string) *docMeta {
	data, err := os.ReadFile(metaCachePath(bid))
	if err != nil {
		return nil
	}
	var m docMeta
	if json.Unmarshal(data, &m) != nil || m.PageCount <= 0 {
		return nil
	}
	return &m
}

func saveMetaCache(bid string, m *docMeta) {
	data, err := json.Marshal(m)
	if err != nil {
		return
	}
	tmp := metaCachePath(bid) + ".tmp"
	if os.WriteFile(tmp, data, 0644) == nil {
		os.Rename(tmp, metaCachePath(bid))
	}
}

func round1(f float64) float64 {
	return math.Round(f*10) / 10
}

// parseDocMeta 用 pdfcpu 解析页数 + 每页 MediaBox 尺寸（升序）
func parseDocMeta(pdfPath string) (*docMeta, error) {
	cnt, err := api.PageCountFile(pdfPath)
	if err != nil {
		return nil, err
	}
	dims, err := api.PageDimsFile(pdfPath)
	if err != nil {
		return nil, err
	}
	m := &docMeta{PageCount: cnt, Pages: make([]PageSize, cnt)}
	for i := 0; i < cnt; i++ {
		if i < len(dims) && dims[i].Width > 0 && dims[i].Height > 0 {
			m.Pages[i] = PageSize{W: round1(dims[i].Width), H: round1(dims[i].Height)}
		} else {
			m.Pages[i] = PageSize{W: 612.0, H: 792.0}
		}
	}
	return m, nil
}

func handleMeta(w http.ResponseWriter, r *http.Request, u *User) {
	bid := r.URL.Query().Get("id")
	entry := loadFileMap(u.UID)[bid]
	if entry == nil || entry.Type != "file" {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}

	meta := loadMetaCache(bid)
	if meta == nil {
		m, err := parseDocMeta(entry.Path)
		if err != nil {
			logf("meta error %s: %v", entry.Name, err)
			w.Header().Set("Content-Type", "application/json; charset=utf-8")
			w.WriteHeader(http.StatusInternalServerError)
			json.NewEncoder(w).Encode(map[string]any{"error": "meta_failed", "detail": err.Error()})
			return
		}
		meta = m
		saveMetaCache(bid, meta)
	}

	// 后台预切整本（幂等、并发去重），运行时翻页直接命中切片
	ensureSlices(bid, entry.Path)

	writeJSON(w, map[string]any{
		"id":        bid,
		"name":      entry.Name,
		"pageCount": meta.PageCount,
		"pages":     meta.Pages,
		"progress":  loadProgress(u.UID)[bid],
	})
}

// ----------------------------------------------------------------------------
// 预切片：把整本拆成单页 PDF 缓存到 slices/{bookId}/page-N.pdf
// ----------------------------------------------------------------------------

var slicing sync.Map // bookID -> struct{}（正在切，并发去重）

func slicesDir(bid string) string {
	return filepath.Join(cfg.DataDir, "slices", bid)
}

func sliceDonePath(bid string) string {
	return filepath.Join(slicesDir(bid), ".done")
}

// slicePath 第 page1 页（1-based）对应的切片文件
func slicePath(bid string, page1 int) string {
	return filepath.Join(slicesDir(bid), fmt.Sprintf("page-%d.pdf", page1))
}

// ensureSlices 后台预切整本；已切或正在切则跳过
func ensureSlices(bid, pdfPath string) {
	if fileExists(sliceDonePath(bid)) {
		return
	}
	if _, loaded := slicing.LoadOrStore(bid, struct{}{}); loaded {
		return
	}
	go func() {
		defer slicing.Delete(bid)
		if err := doSlice(bid, pdfPath); err != nil {
			logf("预切片失败 %s: %v", bid, err)
			return
		}
	}()
}

func doSlice(bid, pdfPath string) error {
	if fileExists(sliceDonePath(bid)) {
		return nil
	}
	tmpDir, err := os.MkdirTemp("", "pdfslice-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmpDir)

	start := time.Now()
	// span=1：一次调用把整本拆成单页 PDF 到 tmpDir
	if err := api.SplitFile(pdfPath, tmpDir, 1, nil); err != nil {
		return err
	}

	dstDir := slicesDir(bid)
	os.MkdirAll(dstDir, 0755)
	entries, err := os.ReadDir(tmpDir)
	if err != nil {
		return err
	}
	n := 0
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".pdf") {
			continue
		}
		pageNr := trailingNumber(e.Name())
		if pageNr <= 0 {
			continue
		}
		src := filepath.Join(tmpDir, e.Name())
		dst := slicePath(bid, pageNr)
		if err := os.Rename(src, dst); err != nil {
			if err := copyFile(src, dst); err != nil {
				return err
			}
		}
		n++
	}
	if n == 0 {
		return fmt.Errorf("no pages produced")
	}
	os.WriteFile(sliceDonePath(bid), []byte("ok"), 0644)
	logf("预切片完成 %s：共 %d 页，耗时 %.2f 秒", bid, n, time.Since(start).Seconds())
	return nil
}

// trailingNumber 从 pdfcpu 拆分产物文件名 `{base}_{N}.pdf` 提取页码 N
func trailingNumber(name string) int {
	s := strings.TrimSuffix(name, ".pdf")
	idx := strings.LastIndex(s, "_")
	if idx < 0 {
		return 0
	}
	n, err := strconv.Atoi(s[idx+1:])
	if err != nil {
		return 0
	}
	return n
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer out.Close()
	_, err = io.Copy(out, in)
	return err
}

// ----------------------------------------------------------------------------
// 单页切片接口：优先命中预切片缓存直接 ServeFile，未切则同步切该页兜底
// ----------------------------------------------------------------------------

func handlePagePDF(w http.ResponseWriter, r *http.Request, u *User) {
	start := time.Now()
	bid := r.URL.Query().Get("id")
	page, err := strconv.Atoi(r.URL.Query().Get("page")) // 前端 0-based
	if err != nil {
		http.Error(w, "bad page", http.StatusBadRequest)
		return
	}

	entry := loadFileMap(u.UID)[bid]
	if entry == nil || entry.Type != "file" {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}

	page1 := page + 1 // pdfcpu 1-based
	sp := slicePath(bid, page1)
	if !fileExists(sp) {
		// 同步切该页兜底，并后台预切整本
		if err := sliceOnePage(entry.Path, bid, page1); err != nil {
			logf("切片失败 %s p%d: %v", entry.Name, page1, err)
			http.Error(w, "slice failed", http.StatusInternalServerError)
			return
		}
		ensureSlices(bid, entry.Path)
	}

	logf("%s 请求第 %d 页耗时 %.3f 秒", bid, page, time.Since(start).Seconds())
	w.Header().Set("Content-Type", "application/pdf")
	http.ServeFile(w, r, sp)
}

// sliceOnePage 用 ExtractPagesFile 同步切单页到 slices/{bid}/page-{page1}.pdf
func sliceOnePage(pdfPath, bid string, page1 int) error {
	dstDir := slicesDir(bid)
	os.MkdirAll(dstDir, 0755)
	tmpDir, err := os.MkdirTemp("", "pdfpage-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmpDir)

	if err := api.ExtractPagesFile(pdfPath, tmpDir, []string{strconv.Itoa(page1)}, nil); err != nil {
		return err
	}
	entries, err := os.ReadDir(tmpDir)
	if err != nil {
		return err
	}
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".pdf") {
			continue
		}
		src := filepath.Join(tmpDir, e.Name())
		dst := slicePath(bid, page1)
		if err := os.Rename(src, dst); err != nil {
			return copyFile(src, dst)
		}
		return nil
	}
	return fmt.Errorf("page %d not produced", page1)
}
