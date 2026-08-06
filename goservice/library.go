package main

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

// ----------------------------------------------------------------------------
// 书库扫描 + file_map 持久化
// ----------------------------------------------------------------------------

// FileEntry 对应一个 PDF 文件或文件夹节点（字段与前端/旧 Python 版对齐）
type FileEntry struct {
	ID         string `json:"id"`
	Fid        string `json:"fid"`
	Name       string `json:"name"`
	Path       string `json:"path"`
	FolderName string `json:"folder_name"`
	Size       int64  `json:"size"`
	Mtime      int64  `json:"mtime"`
	Root       string `json:"root"`
	Type       string `json:"type"` // file | folder
}

var fileMapMu sync.Mutex

func fileMapDir() string {
	d := filepath.Join(cfg.DataDir, "file_map")
	os.MkdirAll(d, 0755)
	return d
}

func fileMapPath(uid string) string {
	return filepath.Join(fileMapDir(), safeName(uid)+".json")
}

func loadFileMap(uid string) map[string]*FileEntry {
	fp := fileMapPath(uid)
	data, err := os.ReadFile(fp)
	if err != nil {
		return map[string]*FileEntry{}
	}
	var m map[string]*FileEntry
	if json.Unmarshal(data, &m) != nil || m == nil {
		return map[string]*FileEntry{}
	}
	return m
}

func saveFileMap(uid string, m map[string]*FileEntry) {
	data, err := json.Marshal(m)
	if err != nil {
		return
	}
	tmp := fileMapPath(uid) + ".tmp"
	if os.WriteFile(tmp, data, 0644) == nil {
		os.Rename(tmp, fileMapPath(uid))
	}
}

// scanAll 递归扫描所有书库根目录下的 *.pdf，构建文件 + 文件夹树
func scanAll(uid string) map[string]*FileEntry {
	fileMapMu.Lock()
	defer fileMapMu.Unlock()

	roots := collectRoots()
	fileMap := map[string]*FileEntry{}

	for _, root := range roots {
		filepath.Walk(root, func(path string, info os.FileInfo, err error) error {
			if err != nil {
				return nil
			}
			name := info.Name()
			if info.IsDir() {
				if strings.HasPrefix(name, ".") && path != root {
					return filepath.SkipDir
				}
				return nil
			}
			if strings.HasPrefix(name, ".") || !strings.HasSuffix(strings.ToLower(name), ".pdf") {
				return nil
			}
			real, err := filepath.EvalSymlinks(path)
			if err != nil {
				real = path
			}
			// 必须在 root 内
			if real != root && !strings.HasPrefix(real, root+string(os.PathSeparator)) {
				return nil
			}
			bid := hashID(real)
			realDir := filepath.Dir(real)
			fileMap[bid] = &FileEntry{
				ID:         bid,
				Fid:        hashID(realDir),
				Name:       name,
				Path:       real,
				FolderName: filepath.Base(realDir),
				Size:       info.Size(),
				Mtime:      info.ModTime().Unix(),
				Root:       root,
				Type:       "file",
			}
			// 向上建文件夹树直到 root
			tmp := realDir
			for {
				folderID := hashID(tmp)
				parentDir := filepath.Dir(tmp)
				if _, ok := fileMap[folderID]; !ok {
					fileMap[folderID] = &FileEntry{
						ID:         folderID,
						Fid:        hashID(parentDir),
						Name:       filepath.Base(tmp),
						Path:       tmp,
						FolderName: filepath.Base(parentDir),
						Size:       1,
						Mtime:      0,
						Root:       root,
						Type:       "folder",
					}
				} else {
					fileMap[folderID].Size++
				}
				if tmp == root {
					break
				}
				tmp = parentDir
			}
			return nil
		})
	}

	saveFileMap(uid, fileMap)
	return fileMap
}

// bookListItem 返回给前端的脱敏条目（不含真实路径）
type bookListItem struct {
	ID       string         `json:"id"`
	Fid      string         `json:"fid"`
	Name     string         `json:"name"`
	Size     int64          `json:"size"`
	Mtime    int64          `json:"mtime"`
	Type     string         `json:"type"`
	Progress *progressEntry `json:"progress,omitempty"`
}

func handleBooks(w http.ResponseWriter, r *http.Request, u *User) {
	start := time.Now()
	queryPath := r.URL.Query().Get("path")
	queryScan := r.URL.Query().Get("scan")

	fileMap := loadFileMap(u.UID)
	if len(fileMap) == 0 || queryScan == "all" {
		fileMap = scanAll(u.UID)
	}
	logf("/api/books 耗时 %.3f 秒", time.Since(start).Seconds())

	// 决定根目录 id 集合
	var rootIDs []string
	if queryPath == "" {
		for _, root := range collectRoots() {
			rootIDs = append(rootIDs, hashID(root))
		}
	} else {
		rootIDs = []string{queryPath}
	}
	rootSet := map[string]bool{}
	for _, id := range rootIDs {
		rootSet[id] = true
	}

	progress := loadProgress(u.UID)
	books := []bookListItem{}
	history := []bookListItem{}
	for k, v := range fileMap {
		item := bookListItem{
			ID:    k,
			Fid:   v.Fid,
			Name:  v.Name,
			Size:  v.Size,
			Mtime: v.Mtime,
			Type:  v.Type,
		}
		if rootSet[v.Fid] {
			books = append(books, item)
		}
		if pe, ok := progress[k]; ok && len(history) < 10 {
			it := item
			it.Progress = &progressEntry{
				Page:       pe.Page,
				TotalPages: pe.TotalPages,
				Percent:    pe.Percent,
				UpdatedAt:  pe.UpdatedAt,
			}
			history = append(history, it)
		}
	}
	sort.Slice(history, func(i, j int) bool {
		return history[i].Progress.UpdatedAt > history[j].Progress.UpdatedAt
	})

	writeJSON(w, map[string]any{
		"books":    books,
		"history":  history,
		"count":    len(books),
		"username": u.Username,
	})
}
