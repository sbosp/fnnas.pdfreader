package main

import (
	"os"
	"path/filepath"
	"strings"
)

// resolveInRoots 校验并规范化路径：必须存在且位于某个书库根目录内。
// canonicalize（EvalSymlinks）后比对根前缀，防 ../ 与符号链接逃逸。
func resolveInRoots(raw string) (string, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", false
	}
	real, err := filepath.EvalSymlinks(raw)
	if err != nil {
		// 路径不存在时 EvalSymlinks 失败；仍尝试 Abs 再 Stat
		abs, err2 := filepath.Abs(raw)
		if err2 != nil {
			return "", false
		}
		if _, err3 := os.Stat(abs); err3 != nil {
			return "", false
		}
		real = abs
	}
	for _, root := range collectRoots() {
		if real == root || strings.HasPrefix(real, root+string(os.PathSeparator)) {
			return real, true
		}
	}
	return "", false
}

func isPDF(name string) bool {
	return strings.HasSuffix(strings.ToLower(name), ".pdf")
}

func isHidden(name string) bool {
	return strings.HasPrefix(name, ".")
}

func fileNameOf(p string) string {
	return filepath.Base(p)
}

func rootOf(p string) string {
	for _, root := range collectRoots() {
		if p == root || strings.HasPrefix(p, root+string(os.PathSeparator)) {
			return root
		}
	}
	return ""
}

// relSegments 相对所属书库根的路径分段（面包屑 / 前端导航）
func relSegments(p string) []string {
	root := rootOf(p)
	if root == "" {
		return nil
	}
	rel, err := filepath.Rel(root, p)
	if err != nil || rel == "." || rel == "" {
		return nil
	}
	parts := strings.Split(filepath.ToSlash(rel), "/")
	out := make([]string, 0, len(parts))
	for _, s := range parts {
		if s != "" && s != "." {
			out = append(out, s)
		}
	}
	return out
}
