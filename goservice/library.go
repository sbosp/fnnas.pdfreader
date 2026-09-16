package main

import (
	"os"
	"path/filepath"
	"sort"
	"unicode"
)

// Entry 目录项（与 React Item 对齐）
type Entry struct {
	Name     string   `json:"name"`
	Path     string   `json:"path"`
	Type     string   `json:"type"` // file | folder
	Size     int64    `json:"size"`
	Mtime    int64    `json:"mtime"`
	Count    int      `json:"count"`
	Segments []string `json:"segments"`
}

func metaOf(p string) (size, mtime int64) {
	st, err := os.Stat(p)
	if err != nil {
		return 0, 0
	}
	return st.Size(), st.ModTime().Unix()
}

func countChildren(dir string) int {
	rd, err := os.ReadDir(dir)
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range rd {
		name := e.Name()
		if isHidden(name) {
			continue
		}
		if e.IsDir() {
			n++
		} else if e.Type().IsRegular() && isPDF(name) {
			n++
		}
	}
	return n
}

func listDir(dir string) []Entry {
	rd, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	var folders, files []Entry
	for _, e := range rd {
		name := e.Name()
		if isHidden(name) {
			continue
		}
		p := filepath.Join(dir, name)
		if e.IsDir() {
			_, mt := metaOf(p)
			folders = append(folders, Entry{
				Name:     name,
				Path:     p,
				Type:     "folder",
				Size:     0,
				Mtime:    mt,
				Count:    countChildren(p),
				Segments: relSegments(p),
			})
		} else if e.Type().IsRegular() && isPDF(name) {
			sz, mt := metaOf(p)
			files = append(files, Entry{
				Name:     name,
				Path:     p,
				Type:     "file",
				Size:     sz,
				Mtime:    mt,
				Count:    0,
				Segments: relSegments(p),
			})
		}
	}
	sort.Slice(folders, func(i, j int) bool { return naturalLess(folders[i].Name, folders[j].Name) })
	sort.Slice(files, func(i, j int) bool { return naturalLess(files[i].Name, files[j].Name) })
	return append(folders, files...)
}

func rootsAsEntries() []Entry {
	roots := collectRoots()
	out := make([]Entry, 0, len(roots))
	for _, r := range roots {
		_, mt := metaOf(r)
		out = append(out, Entry{
			Name:     fileNameOf(r),
			Path:     r,
			Type:     "folder",
			Size:     0,
			Mtime:    mt,
			Count:    countChildren(r),
			Segments: nil,
		})
	}
	return out
}

func resolveDir(raw string) (string, bool) {
	p, ok := resolveInRoots(raw)
	if !ok {
		return "", false
	}
	st, err := os.Stat(p)
	if err != nil || !st.IsDir() {
		return "", false
	}
	return p, true
}

type crumb struct {
	Name string `json:"name"`
	Path string `json:"path"`
}

func breadcrumb(p string) []crumb {
	root := rootOf(p)
	if root == "" {
		return nil
	}
	out := []crumb{{Name: fileNameOf(root), Path: root}}
	cur := root
	for _, seg := range relSegments(p) {
		cur = filepath.Join(cur, seg)
		out = append(out, crumb{Name: seg, Path: cur})
	}
	return out
}

// naturalLess：让「第2章」排在「第10章」前
func naturalLess(a, b string) bool {
	ai, bi := 0, 0
	ra, rb := []rune(a), []rune(b)
	for ai < len(ra) && bi < len(rb) {
		ca, cb := ra[ai], rb[bi]
		if unicode.IsDigit(ca) && unicode.IsDigit(cb) {
			na, nb := 0, 0
			for ai < len(ra) && unicode.IsDigit(ra[ai]) {
				na = na*10 + int(ra[ai]-'0')
				ai++
			}
			for bi < len(rb) && unicode.IsDigit(rb[bi]) {
				nb = nb*10 + int(rb[bi]-'0')
				bi++
			}
			if na != nb {
				return na < nb
			}
			continue
		}
		la := unicode.ToLower(ca)
		lb := unicode.ToLower(cb)
		if la != lb {
			return la < lb
		}
		ai++
		bi++
	}
	return len(ra)-ai < len(rb)-bi
}
