package main

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"time"
)

// progressEntry 阅读进度。key 为书籍真实绝对路径。
type progressEntry struct {
	Page       int     `json:"page"`
	Frac       float64 `json:"frac,omitempty"`
	Name       string  `json:"name,omitempty"`
	Scale      float64 `json:"scale,omitempty"`
	TotalPages int     `json:"totalPages"`
	Percent    any     `json:"percent,omitempty"`
	UpdatedAt  int64   `json:"updatedAt"`
}

var progressMu sync.Mutex

func progressDir() string {
	d := filepath.Join(cfg.DataDir, "progress")
	os.MkdirAll(d, 0755)
	return d
}

func progressPath(uid string) string {
	return filepath.Join(progressDir(), safeName(uid)+".json")
}

func loadProgress(uid string) map[string]*progressEntry {
	data, err := os.ReadFile(progressPath(uid))
	if err != nil {
		return map[string]*progressEntry{}
	}
	var m map[string]*progressEntry
	if json.Unmarshal(data, &m) != nil || m == nil {
		return map[string]*progressEntry{}
	}
	return m
}

func saveProgressEntry(uid, key string, entry *progressEntry) *progressEntry {
	progressMu.Lock()
	defer progressMu.Unlock()

	data := loadProgress(uid)
	prev := data[key]
	if prev == nil {
		prev = &progressEntry{}
	}
	if entry.Page != 0 || prev.Page == 0 {
		prev.Page = entry.Page
	}
	prev.Frac = entry.Frac
	if entry.Name != "" {
		prev.Name = entry.Name
	}
	if entry.Scale != 0 {
		prev.Scale = entry.Scale
	}
	if entry.TotalPages != 0 {
		prev.TotalPages = entry.TotalPages
	}
	if entry.Percent != nil {
		prev.Percent = entry.Percent
	}
	prev.UpdatedAt = time.Now().Unix()
	data[key] = prev

	raw, err := json.Marshal(data)
	if err != nil {
		return nil
	}
	tmp := progressPath(uid) + ".tmp"
	if os.WriteFile(tmp, raw, 0644) != nil {
		return nil
	}
	if os.Rename(tmp, progressPath(uid)) != nil {
		return nil
	}
	return prev
}

func handleProgress(w http.ResponseWriter, r *http.Request, u *User) {
	setNoStore(w)
	raw := r.URL.Query().Get("path")
	if raw == "" {
		http.Error(w, "missing path", http.StatusBadRequest)
		return
	}
	key, ok := resolveInRoots(raw)
	if !ok {
		http.Error(w, "book not found", http.StatusNotFound)
		return
	}

	switch r.Method {
	case http.MethodGet:
		prog := loadProgress(u.UID)
		writeJSON(w, map[string]any{"path": key, "progress": prog[key]})

	case http.MethodPost:
		var payload progressEntry
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			http.Error(w, "bad json", http.StatusBadRequest)
			return
		}
		saved := saveProgressEntry(u.UID, key, &payload)
		writeJSON(w, map[string]any{"ok": saved != nil, "progress": saved})

	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

// recentItems 最近阅读（按 updatedAt 降序，最多 n 条）
func recentItems(uid string, n int) []map[string]any {
	prog := loadProgress(uid)
	type pair struct {
		path string
		pe   *progressEntry
	}
	var list []pair
	for k, v := range prog {
		if v == nil {
			continue
		}
		list = append(list, pair{k, v})
	}
	sort.Slice(list, func(i, j int) bool {
		return list[i].pe.UpdatedAt > list[j].pe.UpdatedAt
	})
	if len(list) > n {
		list = list[:n]
	}
	out := make([]map[string]any, 0, len(list))
	for _, it := range list {
		p := it.path
		// 文件已删仍可展示历史；路径校验失败也返回条目
		name := fileNameOf(p)
		if it.pe.Name != "" {
			name = it.pe.Name
		}
		out = append(out, map[string]any{
			"name":     name,
			"path":     p,
			"segments": relSegments(p),
			"progress": it.pe,
		})
	}
	return out
}
