// PDF 阅读器 — Rust 后端配置与基础设施
//
// 与 fnOS cmd/main 注入的 PDFR_* 环境变量对齐；监听 Unix Socket(生产)/TCP(调试)。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub struct Config {
    /// 应用名（PDFR_APPNAME），目前仅用于诊断/日志场景，保留以对齐 fnOS 注入的环境变量
    #[allow(dead_code)]
    pub appname: String,
    pub gateway_prefix: String,
    pub sock_path: String,
    pub web_root: PathBuf,
    pub data_dir: PathBuf,
    pub log_file: Option<String>,
    pub require_auth: bool,
    pub roots_file: Option<String>,
    pub port: u16,
    pub host: String,
}

static CFG: OnceLock<Config> = OnceLock::new();

pub fn cfg() -> &'static Config {
    CFG.get().expect("config not initialized")
}

pub fn env_or(key: &str, def: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => def.to_string(),
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn file_exists(p: &Path) -> bool {
    p.is_file()
}

/// resolve_webroot 查找前端构建产物目录（含 index.html）
fn resolve_webroot() -> PathBuf {
    let here = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(cwd);

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("PDFR_WEBROOT") {
        if !v.is_empty() {
            candidates.push(PathBuf::from(v));
        }
    }
    candidates.push(here.join("ui"));
    candidates.push(here.join("..").join("ui"));
    candidates.push(cwd().join("ui"));

    for c in &candidates {
        if file_exists(&c.join("index.html")) {
            return c.canonicalize().unwrap_or_else(|_| c.clone());
        }
    }
    here.join("ui")
}

pub fn init_config(port: u16, host: String) {
    let gateway_prefix = env_or("PDFR_GATEWAY_PREFIX", "/app/fnnas-pdfreader")
        .trim_end_matches('/')
        .to_string();
    let data_dir = PathBuf::from(env_or(
        "PDFR_DATA_DIR",
        cwd().join("data").to_string_lossy().as_ref(),
    ));
    let _ = std::fs::create_dir_all(&data_dir);

    let c = Config {
        appname: env_or("PDFR_APPNAME", "pdfreader"),
        gateway_prefix,
        sock_path: env_or("PDFR_SOCK", cwd().join("app.sock").to_string_lossy().as_ref()),
        web_root: resolve_webroot(),
        data_dir,
        log_file: std::env::var("PDFR_LOGFILE").ok().filter(|s| !s.is_empty()),
        require_auth: std::env::var("PDFR_REQUIRE_AUTH").as_deref() == Ok("1"),
        roots_file: std::env::var("PDFR_ROOTS_FILE").ok().filter(|s| !s.is_empty()),
        port,
        host,
    };
    let _ = CFG.set(c);
}

thread_local! {
    static REQ_UID: RefCell<String> = RefCell::new(String::new());
}

/// 把当前请求的飞牛 uid 绑到本线程，collect_roots / 开放 API 查询用户授权目录时用。
/// spawn_blocking 的闭包里调用，thread-local 才能跟着走。
pub fn with_request_uid<T>(uid: &str, f: impl FnOnce() -> T) -> T {
    REQ_UID.with(|c| {
        let prev = c.replace(uid.to_string());
        let out = f();
        c.replace(prev);
        out
    })
}

fn request_uid() -> String {
    REQ_UID.with(|c| c.borrow().clone())
}

#[derive(Clone)]
struct LiveRoots {
    key: String,
    at: Instant,
    paths: Vec<PathBuf>,
}

fn live_cache() -> &'static Mutex<Option<LiveRoots>> {
    static C: OnceLock<Mutex<Option<LiveRoots>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn add_root(seen: &mut Vec<PathBuf>, p: &str) {
    let p = p.trim();
    if p.is_empty() {
        return;
    }
    let pb = PathBuf::from(p);
    if !pb.is_dir() {
        return;
    }
    let rp = pb.canonicalize().unwrap_or(pb);
    if !seen.contains(&rp) {
        seen.push(rp);
    }
}

/// 只在书库根列表时调用：向开放 API 问一次授权目录，成功则并进缓存。
/// 失败保留旧缓存；绝不能用空结果覆盖 roots.txt。
pub fn refresh_live_roots() {
    let uid = request_uid();
    if uid.is_empty() || uid == "debug" {
        return;
    }
    if let Ok(g) = live_cache().lock() {
        if let Some(c) = g.as_ref() {
            if c.key == uid && c.at.elapsed() < Duration::from_secs(2) {
                return;
            }
        }
    }
    let Some(raw) = crate::trimapi::query_accessible_folders(&uid) else {
        return;
    };
    let fresh: Vec<PathBuf> = raw
        .into_iter()
        .filter_map(|p| {
            let pb = PathBuf::from(p.trim());
            if pb.is_dir() {
                Some(pb.canonicalize().unwrap_or(pb))
            } else {
                None
            }
        })
        .collect();
    if let Ok(mut g) = live_cache().lock() {
        *g = Some(LiveRoots {
            key: uid,
            at: Instant::now(),
            paths: fresh,
        });
    }
}

/// collect_roots 书库根目录：data-share + 授权目录文件/环境变量，再加上开放 API 缓存。
///
/// 这里不打 RPC（封面/翻页/子目录都会走到），避免开放 API 拖死阅读。
/// 应用设置保存后 cmd/config_callback 会重写 roots.txt 并重启；开放 API 只作增量补充。
pub fn collect_roots() -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for key in [
        "PDFR_SHARE_PATHS",
        "TRIM_DATA_SHARE_PATHS",
        "PDFR_ACCESSIBLE_PATHS",
        "TRIM_DATA_ACCESSIBLE_PATHS",
    ] {
        if let Ok(v) = std::env::var(key) {
            for p in v.split(':') {
                add_root(&mut seen, p);
            }
        }
    }
    if let Some(rf) = &cfg().roots_file {
        if let Ok(data) = std::fs::read_to_string(rf) {
            for line in data.lines() {
                add_root(&mut seen, line);
            }
        }
    }
    let uid = request_uid();
    if !uid.is_empty() {
        if let Ok(g) = live_cache().lock() {
            if let Some(c) = g.as_ref() {
                if c.key == uid {
                    for p in &c.paths {
                        if !seen.contains(p) {
                            seen.push(p.clone());
                        }
                    }
                }
            }
        }
    }
    seen
}

// ----------------------------------------------------------------------------
// 日志：同时写 stderr 与 PDFR_LOGFILE
// ----------------------------------------------------------------------------
pub fn now_stamp() -> String {
    // 不引入 chrono：用 SystemTime 手工格式化到秒（UTC+8 近似本地时间显示）
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64
        + 8 * 3600;
    let days = secs / 86400;
    let tod = secs % 86400;
    // 从 1970-01-01 起推算年月日（民用历算法）
    let mut y = 1970i64;
    let mut d = days;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if d < yd {
            break;
        }
        d -= yd;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let ml = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    while m < 12 && d >= ml[m] {
        d -= ml[m];
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m + 1,
        d + 1,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

#[macro_export]
macro_rules! logf {
    ($($arg:tt)*) => {{
        let line = format!("[pdfreader {}] {}\n", $crate::config::now_stamp(), format!($($arg)*));
        eprint!("{}", line);
        if let Some(lf) = &$crate::config::cfg().log_file {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(lf) {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }};
}

/// safe_name 把 uid 清洗成安全文件名
pub fn safe_name(uid: &str) -> String {
    let s: String = uid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if s.is_empty() {
        "anon".to_string()
    } else {
        s
    }
}
