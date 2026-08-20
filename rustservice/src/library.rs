// 书库浏览（路径化，不再用 hash 编码路径）
//
// 设计：按需列目录，不再全量扫描+持久化 file_map。
//   - /api/list?path=<真实路径>  列出该目录下的子目录与 PDF（path 省略则列书库根）
//   - 每项返回真实路径 path + 相对书库根的分段 segments，供前端面包屑导航
use crate::config::collect_roots;
use crate::pathsafe::{file_name_of, is_hidden, is_pdf, rel_segments, resolve_in_roots, root_of};
use std::path::{Path, PathBuf};

pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: &'static str, // "file" | "folder"
    pub size: u64,
    pub mtime: u64,
    pub count: usize, // folder: 直接子项中 PDF/子目录数量
}

fn meta_of(p: &Path) -> (u64, u64) {
    match std::fs::metadata(p) {
        Ok(m) => {
            let mt = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (m.len(), mt)
        }
        Err(_) => (0, 0),
    }
}

/// count_children 统计目录内直接可见子项（PDF 文件 + 子目录）数量，用于列表展示
fn count_children(dir: &Path) -> usize {
    let mut n = 0usize;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if is_hidden(&name) {
                continue;
            }
            match e.file_type() {
                Ok(ft) if ft.is_dir() => n += 1,
                Ok(ft) if ft.is_file() && is_pdf(&name) => n += 1,
                _ => {}
            }
        }
    }
    n
}

/// list_dir 列出目录内容（子目录 + PDF），目录在前、各自按名称排序。
/// dir 必须已通过 resolve_in_roots 校验。
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut folders: Vec<Entry> = Vec::new();
    let mut files: Vec<Entry> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if is_hidden(&name) {
                continue;
            }
            let p = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => {
                    let (_, mt) = meta_of(&p);
                    folders.push(Entry {
                        name,
                        path: p.to_string_lossy().to_string(),
                        kind: "folder",
                        size: 0,
                        mtime: mt,
                        count: count_children(&p),
                    });
                }
                Ok(ft) if ft.is_file() && is_pdf(&name) => {
                    let (sz, mt) = meta_of(&p);
                    files.push(Entry {
                        name,
                        path: p.to_string_lossy().to_string(),
                        kind: "file",
                        size: sz,
                        mtime: mt,
                        count: 0,
                    });
                }
                _ => {}
            }
        }
    }

    folders.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    files.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    folders.extend(files);
    folders
}

/// natural_cmp 自然排序：让「第2章」排在「第10章」前面
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na: String = std::iter::from_fn(|| ai.next_if(|c| c.is_ascii_digit())).collect();
                    let nb: String = std::iter::from_fn(|| bi.next_if(|c| c.is_ascii_digit())).collect();
                    let va = na.parse::<u64>().unwrap_or(0);
                    let vb = nb.parse::<u64>().unwrap_or(0);
                    if va != vb {
                        return va.cmp(&vb);
                    }
                } else {
                    let la = ca.to_lowercase().next().unwrap_or(ca);
                    let lb = cb.to_lowercase().next().unwrap_or(cb);
                    if la != lb {
                        return la.cmp(&lb);
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

/// roots_as_entries 书库根目录列表（前端首页展示；多个根时作为顶级入口）
pub fn roots_as_entries() -> Vec<Entry> {
    collect_roots()
        .into_iter()
        .map(|r| {
            let (_, mt) = meta_of(&r);
            Entry {
                name: file_name_of(&r),
                path: r.to_string_lossy().to_string(),
                kind: "folder",
                size: 0,
                mtime: mt,
                count: count_children(&r),
            }
        })
        .collect()
}

/// resolve_dir 校验目录参数：空则表示书库根层级（返回 None 由调用方特殊处理）
pub fn resolve_dir(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let p = resolve_in_roots(raw)?;
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

/// breadcrumb 面包屑：返回从书库根到该路径的每一级 (名称, 真实路径)。
/// 单书库根时首级即根目录名；多根时前端可再叠加「书库」入口。
pub fn breadcrumb(p: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(root) = root_of(p) {
        out.push((
            file_name_of(&root),
            root.to_string_lossy().to_string(),
        ));
        let mut cur = root.clone();
        for seg in rel_segments(p) {
            cur = cur.join(&seg);
            out.push((seg, cur.to_string_lossy().to_string()));
        }
    }
    out
}
