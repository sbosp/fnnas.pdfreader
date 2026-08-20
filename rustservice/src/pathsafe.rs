// 路径安全：前端传真实路径（不再用 hash 编码），后端必须严格校验路径在书库根内。
//
// 设计：API 参数 `path` 为书库内的真实绝对路径（URL 编码）。任何路径在使用前
// 必须过 resolve_in_roots()：canonicalize 后逐个比对书库根前缀，防目录穿越
// （../、符号链接逃逸都会在 canonicalize + 前缀比对后被拒绝）。
use crate::config::collect_roots;
use std::path::{Path, PathBuf};

/// resolve_in_roots 校验并规范化路径：必须存在且位于某个书库根目录内。
/// 返回规范化后的真实路径；越界/不存在返回 None。
pub fn resolve_in_roots(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let p = PathBuf::from(raw);
    // canonicalize 会解析 .. 与符号链接，是防穿越的关键
    let real = p.canonicalize().ok()?;
    let roots = collect_roots();
    for root in &roots {
        if &real == root || real.starts_with(root) {
            return Some(real);
        }
    }
    None
}

/// is_pdf 是否为 PDF 文件名（大小写不敏感）
pub fn is_pdf(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".pdf")
}

/// is_hidden 是否隐藏文件/目录（以 . 开头）
pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// file_name_of 取路径末段名（失败回退空串）
pub fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// root_of 返回该路径所属的书库根（用于计算相对路径/缓存位置）
pub fn root_of(p: &Path) -> Option<PathBuf> {
    for root in collect_roots() {
        if p == root || p.starts_with(&root) {
            return Some(root);
        }
    }
    None
}

/// rel_segments 返回相对所属书库根的路径分段（用于前端面包屑导航）。
/// 例：root=/vol1/PDFLibrary, p=/vol1/PDFLibrary/技术/Rust.pdf → ["技术", "Rust.pdf"]
pub fn rel_segments(p: &Path) -> Vec<String> {
    if let Some(root) = root_of(p) {
        if let Ok(rel) = p.strip_prefix(&root) {
            return rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
        }
    }
    Vec::new()
}
