package main

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// ----------------------------------------------------------------------------
// 阅读进度持久化（按 uid 存 progress/{uid}.json，原子替换）
// ----------------------------------------------------------------------------

// progressEntry 阅读进度。Percent 用 any 兼容前端 toFixed 字符串或数字。
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

func saveProgressEntry(uid, bid string, entry *progressEntry) *progressEntry {
	progressMu.Lock()
	defer progressMu.Unlock()

	data := loadProgress(uid)
	prev := data[bid]
	if prev == nil {
		prev = &progressEntry{}
	}
	// 只更新非零字段（与 Python 版 prev.update(entry) 对齐）
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
	data[bid] = prev

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
	bid := r.URL.Query().Get("id")

	switch r.Method {
	case http.MethodGet:
		prog := loadProgress(u.UID)
		writeJSON(w, map[string]any{"id": bid, "progress": prog[bid]})

	case http.MethodPost:
		var payload progressEntry
		var raw map[string]any
		if err := json.NewDecoder(r.Body).Decode(&raw); err == nil {
			if v, ok := raw["id"].(string); ok && v != "" {
				bid = v
			}
		}
		// 重新解码到结构体（raw 已消费 body，改用直接 decode 一次）
		// 注：上面已读出 id，这里用 raw 填字段更稳。
		if bid == "" {
			http.Error(w, "missing id", http.StatusBadRequest)
			return
		}
		payload = progressFromMap(raw)
		saved := saveProgressEntry(u.UID, bid, &payload)
		writeJSON(w, map[string]any{"ok": saved != nil, "progress": saved})

	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

// progressFromMap 从 map 提取进度字段（宽松解析）
func progressFromMap(m map[string]any) progressEntry {
	e := progressEntry{}
	if v, ok := m["page"].(float64); ok {
		e.Page = int(v)
	}
	if v, ok := m["frac"].(float64); ok {
		e.Frac = v
	}
	if v, ok := m["name"].(string); ok {
		e.Name = v
	}
	if v, ok := m["scale"].(float64); ok {
		e.Scale = v
	}
	if v, ok := m["totalPages"].(float64); ok {
		e.TotalPages = int(v)
	}
	if v, ok := m["percent"]; ok {
		e.Percent = v
	}
	return e
}
