// 阅读进度持久化（按 uid 存 DATA_DIR/progress/{uid}.json）
//
// key 改为「书籍真实路径」（不再是 hash 的 bookId），与路径化 API 一致。
use crate::config::{cfg, safe_name};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Default)]
pub struct Progress {
    pub page: usize,
    pub frac: f64,
    pub name: String,
    pub scale: f64,
    pub total_pages: usize,
    pub percent: f64,
    pub updated_at: u64,
}

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn progress_path(uid: &str) -> PathBuf {
    let d = cfg().data_dir.join("progress");
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{}.json", safe_name(uid)))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ----------------------------------------------------------------------------
// 极简 JSON 编解码（自产自销的固定结构，避免引入 serde 依赖膨胀）
// ----------------------------------------------------------------------------
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn unesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('"') => o.push('"'),
            Some('\\') => o.push('\\'),
            Some('/') => o.push('/'),
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some('t') => o.push('\t'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                if let Ok(v) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(v) {
                        o.push(ch);
                    }
                }
            }
            Some(other) => o.push(other),
            None => break,
        }
    }
    o
}

pub fn progress_to_json(p: &Progress) -> String {
    format!(
        r#"{{"page":{},"frac":{:.4},"name":"{}","scale":{:.4},"totalPages":{},"percent":{:.2},"updatedAt":{}}}"#,
        p.page,
        p.frac,
        esc(&p.name),
        p.scale,
        p.total_pages,
        p.percent,
        p.updated_at
    )
}

/// 从 JSON 对象体里取数字字段
fn jnum(s: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

/// 从 JSON 对象体里取字符串字段
fn jstr(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let body = &rest[1..];
    // 找未转义的结束引号
    let mut end = 0usize;
    let bytes = body.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'"' {
            let mut bs = 0usize;
            let mut k = end;
            while k > 0 && bytes[k - 1] == b'\\' {
                bs += 1;
                k -= 1;
            }
            if bs % 2 == 0 {
                break;
            }
        }
        end += 1;
    }
    Some(unesc(&body[..end.min(body.len())]))
}

pub fn parse_progress_obj(s: &str) -> Progress {
    Progress {
        page: jnum(s, "page").unwrap_or(0.0).max(0.0) as usize,
        frac: jnum(s, "frac").unwrap_or(0.0),
        name: jstr(s, "name").unwrap_or_default(),
        scale: jnum(s, "scale").unwrap_or(0.0),
        total_pages: jnum(s, "totalPages").unwrap_or(0.0).max(0.0) as usize,
        percent: jnum(s, "percent").unwrap_or(0.0),
        updated_at: jnum(s, "updatedAt").unwrap_or(0.0).max(0.0) as u64,
    }
}

/// 顶层 map 拆分为 (key, objectBody) 列表。结构固定为 {"path":{...},...}
fn split_top_objects(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0usize;
    // 跳到第一个 {
    while i < b.len() && b[i] != b'{' {
        i += 1;
    }
    if i >= b.len() {
        return out;
    }
    i += 1;
    loop {
        // 找 key 的起始引号
        while i < b.len() && b[i] != b'"' {
            if b[i] == b'}' {
                return out;
            }
            i += 1;
        }
        if i >= b.len() {
            return out;
        }
        i += 1;
        let ks = i;
        while i < b.len() {
            if b[i] == b'"' {
                let mut bs = 0usize;
                let mut k = i;
                while k > ks && b[k - 1] == b'\\' {
                    bs += 1;
                    k -= 1;
                }
                if bs % 2 == 0 {
                    break;
                }
            }
            i += 1;
        }
        let key = unesc(&s[ks..i.min(s.len())]);
        i += 1;
        // 找值起始 {
        while i < b.len() && b[i] != b'{' {
            i += 1;
        }
        if i >= b.len() {
            return out;
        }
        let vs = i;
        let mut depth = 0i32;
        let mut in_str = false;
        while i < b.len() {
            let c = b[i];
            if in_str {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == b'"' {
                    in_str = false;
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            out.push((key.clone(), s[vs..i].to_string()));
                            break;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        if depth != 0 {
            return out;
        }
    }
}

pub fn load_all(uid: &str) -> BTreeMap<String, Progress> {
    let mut m = BTreeMap::new();
    if let Ok(s) = std::fs::read_to_string(progress_path(uid)) {
        for (k, body) in split_top_objects(&s) {
            m.insert(k, parse_progress_obj(&body));
        }
    }
    m
}

fn save_all(uid: &str, m: &BTreeMap<String, Progress>) {
    let mut s = String::from("{");
    for (i, (k, v)) in m.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push('"');
        s.push_str(&esc(k));
        s.push_str("\":");
        s.push_str(&progress_to_json(v));
    }
    s.push('}');

    let fp = progress_path(uid);
    let tmp = fp.with_extension("json.tmp");
    if std::fs::write(&tmp, s.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &fp);
    }
}

pub fn get_one(uid: &str, key: &str) -> Option<Progress> {
    load_all(uid).get(key).cloned()
}

/// save_one 合并保存（只覆盖传入的非零字段，与 Go 版行为一致）
pub fn save_one(uid: &str, key: &str, incoming: &Progress) -> Progress {
    let _g = lock().lock();
    let mut all = load_all(uid);
    let mut cur = all.get(key).cloned().unwrap_or_default();

    if incoming.page != 0 || cur.page == 0 {
        cur.page = incoming.page;
    }
    cur.frac = incoming.frac;
    if !incoming.name.is_empty() {
        cur.name = incoming.name.clone();
    }
    if incoming.scale != 0.0 {
        cur.scale = incoming.scale;
    }
    if incoming.total_pages != 0 {
        cur.total_pages = incoming.total_pages;
    }
    if incoming.percent != 0.0 {
        cur.percent = incoming.percent;
    }
    cur.updated_at = now_secs();

    all.insert(key.to_string(), cur.clone());
    save_all(uid, &all);
    cur
}

/// recent 最近阅读列表（按更新时间倒序，最多 limit 条，过滤已不存在的文件）
pub fn recent(uid: &str, limit: usize) -> Vec<(String, Progress)> {
    let mut v: Vec<(String, Progress)> = load_all(uid)
        .into_iter()
        .filter(|(k, _)| PathBuf::from(k).is_file())
        .collect();
    v.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
    v.truncate(limit);
    v
}
