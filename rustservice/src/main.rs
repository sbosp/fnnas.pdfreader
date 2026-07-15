// PDF 阅读器 — Rust 服务端 (fnOS 统一网关版)
//
// 设计要点：
//   * 纯 Rust，零 C 依赖：用 lopdf 把指定页无损抽成独立单页 PDF 切片返回
//     （lopdf 保留页面内容流 + 字体/XObject 资源，pdf.js 端已验证文字/图像完整）
//   * 服务端不再光栅化（不出图片），渲染完全交给前端 pdf.js —— 内存问题从根上消失
//   * 同时托管前端静态文件 + API
//   * 书库递归扫描 *.pdf；bookId = sha1(realpath)[:16]，与旧 Python 版逐字节一致
//   * 阅读进度 / file_map 按飞牛 UID 持久化为 JSON，多端共享续读
//   * 读取统一网关注入的 X-Trim-* 头做鉴权
//   * 生产：监听 Unix Socket（chmod 0666 供网关读写）；调试：--port TCP

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use lopdf::{Document as PdfDoc, Object as PdfObj, ObjectId};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tiny_http::{Header, Method, Response, Server};

// ----------------------------------------------------------------------------
// 配置（环境变量注入，含本地调试默认值）
// ----------------------------------------------------------------------------
struct Config {
    gateway_prefix: String,
    sock_path: String,
    data_dir: String,
    require_auth: bool,
    web_root: PathBuf,
    logfile: String,
}

static CONFIG: OnceLock<Config> = OnceLock::new();
static PROGRESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn cfg() -> &'static Config {
    CONFIG.get().expect("config not initialized")
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn init_config() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let gateway_prefix = env_or("PDFR_GATEWAY_PREFIX", "/app/fnnas-pdfreader")
        .trim_end_matches('/')
        .to_string();
    let sock_path = env_or(
        "PDFR_SOCK",
        cwd.join("app.sock").to_string_lossy().as_ref(),
    );
    let data_dir = env_or("PDFR_DATA_DIR", cwd.join("data").to_string_lossy().as_ref());
    let require_auth = env_or("PDFR_REQUIRE_AUTH", "0") == "1";
    let web_root = resolve_webroot();
    let logfile = env_or("PDFR_LOGFILE", "");

    let _ = CONFIG.set(Config {
        gateway_prefix,
        sock_path,
        data_dir,
        require_auth,
        web_root,
        logfile,
    });
    let _ = PROGRESS_LOCK.set(Mutex::new(()));
}

fn resolve_webroot() -> PathBuf {
    if let Ok(p) = std::env::var("PDFR_WEBROOT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let here = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    for c in [
        here.join("../vueapp/dist"),
        here.join("../ui"),
        here.join("ui"),
    ] {
        if c.join("index.html").is_file() {
            return c;
        }
    }
    here.join("ui")
}

// ----------------------------------------------------------------------------
// 日志
// ----------------------------------------------------------------------------
fn log(msg: &str) {
    let now = now_secs();
    let line = format!("[pdfreader {}] {}\n", fmt_ts(now), msg);
    eprint!("{}", line);
    let lf = &cfg().logfile;
    if !lf.is_empty() {
        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(lf) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// 轻量时间戳（本地无依赖，仅用于日志可读性，用 UTC 秒转粗略时分秒）
fn fmt_ts(secs: i64) -> String {
    // 简易 UTC 换算（不含时区/闰秒，仅日志展示用）
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("d{} {:02}:{:02}:{:02}", days, h, m, s)
}

// ----------------------------------------------------------------------------
// 书库根目录
// ----------------------------------------------------------------------------
fn collect_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut add = |p: &str| {
        let p = p.trim();
        if p.is_empty() {
            return;
        }
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            if let Ok(rp) = fs::canonicalize(&pb) {
                if !roots.contains(&rp) {
                    roots.push(rp);
                }
            }
        }
    };
    for key in ["PDFR_SHARE_PATHS", "PDFR_ACCESSIBLE_PATHS"] {
        if let Ok(val) = std::env::var(key) {
            for p in val.split(':') {
                add(p);
            }
        }
    }
    // 兼容 roots 文件（cmd/main 写入，运行期授权目录变更后重启即可生效）
    if let Ok(rf) = std::env::var("PDFR_ROOTS_FILE") {
        if let Ok(content) = fs::read_to_string(&rf) {
            for line in content.lines() {
                add(line);
            }
        }
    }
    roots
}

// ----------------------------------------------------------------------------
// bookId：sha1(realpath)[:16]，必须与 Python hashlib.sha1(...).hexdigest()[:16] 一致
// ----------------------------------------------------------------------------
fn hash_id_for(abspath: &Path) -> String {
    let mut hasher = Sha1::new();
    hasher.update(abspath.as_os_str().as_bytes());
    let digest = hasher.finalize();
    let hexstr = hex::encode(digest);
    hexstr[..16].to_string()
}

// ----------------------------------------------------------------------------
// file_map 结构
// ----------------------------------------------------------------------------
#[derive(Clone, Serialize, Deserialize)]
struct BookEntry {
    id: String,
    fid: String,
    name: String,
    path: String,
    folder_name: String,
    size: i64,
    mtime: i64,
    root: String,
    #[serde(rename = "type")]
    type_: String,
}

type FileMap = BTreeMap<String, BookEntry>;

fn scan_all(uid: &str) -> FileMap {
    let roots = collect_roots();
    let mut file_map: FileMap = BTreeMap::new();

    for root in &roots {
        walk_dir(root, root, &roots, &mut file_map);
    }
    save_file_map(uid, &file_map);
    file_map
}

fn walk_dir(dir: &Path, root: &Path, roots: &[PathBuf], file_map: &mut FileMap) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let name_str = name.to_string_lossy().to_string();
        if name_str.starts_with('.') {
            continue;
        }
        let ft = match ent.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let full = ent.path();
        if ft.is_dir() {
            walk_dir(&full, root, roots, file_map);
        } else if ft.is_file() {
            if !name_str.to_lowercase().ends_with(".pdf") {
                continue;
            }
            let real = match fs::canonicalize(&full) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !roots.iter().any(|r| real == *r || real.starts_with(r)) {
                continue;
            }
            let meta = match fs::metadata(&real) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let bid = hash_id_for(&real);
            let real_dir = real.parent().unwrap_or(&real).to_path_buf();
            let dirpath_id = hash_id_for(&real_dir);
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            file_map.insert(
                bid.clone(),
                BookEntry {
                    id: bid.clone(),
                    fid: dirpath_id,
                    name: name_str.clone(),
                    path: real.to_string_lossy().to_string(),
                    folder_name: real_dir
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    size: meta.len() as i64,
                    mtime,
                    root: root.to_string_lossy().to_string(),
                    type_: "file".to_string(),
                },
            );

            // 向上聚合文件夹节点（size 作为计数累加）
            let mut temp_real = real.clone();
            loop {
                temp_real = match temp_real.parent() {
                    Some(p) => p.to_path_buf(),
                    None => break,
                };
                let folder_id = hash_id_for(&temp_real);
                let folder_name = temp_real
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let p_folder_dir = temp_real.parent().map(|p| p.to_path_buf());
                let p_folder_name = p_folder_dir
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();

                if let Some(existing) = file_map.get_mut(&folder_id) {
                    existing.size += 1;
                } else {
                    file_map.insert(
                        folder_id.clone(),
                        BookEntry {
                            id: folder_id.clone(),
                            fid: p_folder_dir
                                .as_ref()
                                .map(|p| hash_id_for(p))
                                .unwrap_or_default(),
                            name: folder_name,
                            path: temp_real.to_string_lossy().to_string(),
                            folder_name: p_folder_name,
                            size: 1,
                            mtime: 0,
                            root: root.to_string_lossy().to_string(),
                            type_: "folder".to_string(),
                        },
                    );
                }
                if roots.iter().any(|r| temp_real == *r) {
                    break;
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// 持久化：file_map + progress
// ----------------------------------------------------------------------------
fn safe_uid(uid: &str) -> String {
    let s: String = uid
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if s.is_empty() {
        "anon".to_string()
    } else {
        s
    }
}

fn data_subdir(sub: &str) -> PathBuf {
    let d = PathBuf::from(&cfg().data_dir).join(sub);
    let _ = fs::create_dir_all(&d);
    d
}

fn file_map_file(uid: &str) -> PathBuf {
    data_subdir("file_map").join(format!("{}.json", safe_uid(uid)))
}

fn progress_file(uid: &str) -> PathBuf {
    data_subdir("progress").join(format!("{}.json", safe_uid(uid)))
}

fn load_file_map(uid: &str) -> FileMap {
    let fp = file_map_file(uid);
    if !fp.is_file() {
        return BTreeMap::new();
    }
    fs::read_to_string(&fp)
        .ok()
        .and_then(|s| serde_json::from_str::<FileMap>(&s).ok())
        .unwrap_or_default()
}

fn save_file_map(uid: &str, file_map: &FileMap) {
    if let Ok(s) = serde_json::to_string(file_map) {
        let _ = fs::write(file_map_file(uid), s);
    }
}

type ProgressMap = serde_json::Map<String, serde_json::Value>;

fn load_progress(uid: &str) -> ProgressMap {
    let fp = progress_file(uid);
    if !fp.is_file() {
        return serde_json::Map::new();
    }
    fs::read_to_string(&fp)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_progress_entry(
    uid: &str,
    bid: &str,
    entry: serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let _guard = PROGRESS_LOCK.get().unwrap().lock().unwrap();
    let mut data = load_progress(uid);
    let mut prev = data
        .get(bid)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (k, v) in entry {
        prev.insert(k, v);
    }
    prev.insert("updatedAt".to_string(), serde_json::json!(now_secs()));
    let prev_val = serde_json::Value::Object(prev);
    data.insert(bid.to_string(), prev_val.clone());

    let fp = progress_file(uid);
    let tmp = fp.with_extension("json.tmp");
    let s = serde_json::to_string(&data).ok()?;
    if fs::write(&tmp, s).is_err() {
        log("save progress failed: write tmp");
        return None;
    }
    if fs::rename(&tmp, &fp).is_err() {
        log("save progress failed: rename");
        return None;
    }
    Some(prev_val)
}

// ----------------------------------------------------------------------------
// PDF 处理（纯 Rust，lopdf 无损抽页）
// ----------------------------------------------------------------------------
//
// 文档缓存（单槽）：一本 47MB 扫描书每次翻页都重新解析约 0.7s；缓存已解析的
// Document 后，抽页只需 clone + 删页，约几十 ms。只缓存"最近一本"，内存有界
// （最多常驻一本书的对象图），不会重新引入旧版 PyMuPDF 那种累积不回收的问题。
struct DocCache {
    path: String,
    mtime: i64,
    doc: PdfDoc,
}
static DOC_CACHE: OnceLock<Mutex<Option<DocCache>>> = OnceLock::new();

fn doc_cache() -> &'static Mutex<Option<DocCache>> {
    DOC_CACHE.get_or_init(|| Mutex::new(None))
}

fn file_mtime(path: &str) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// 取一份可修改的 Document：命中缓存则 clone，否则解析并写入缓存
fn load_doc_cached(path: &str) -> Result<PdfDoc, String> {
    let mtime = file_mtime(path);
    {
        let guard = doc_cache().lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if c.path == path && c.mtime == mtime {
                return Ok(c.doc.clone());
            }
        }
    }
    let doc = PdfDoc::load(path).map_err(|e| format!("load: {e:?}"))?;
    let cloned = doc.clone();
    {
        let mut guard = doc_cache().lock().unwrap();
        *guard = Some(DocCache {
            path: path.to_string(),
            mtime,
            doc,
        });
    }
    Ok(cloned)
}

fn obj_num(o: &PdfObj) -> f64 {
    match o {
        PdfObj::Integer(i) => *i as f64,
        PdfObj::Real(r) => *r as f64,
        _ => 0.0,
    }
}

// MediaBox / Rotate 可能继承自 Pages 树，需沿 Parent 向上查找
fn inherited_attr<'a>(
    doc: &'a PdfDoc,
    mut id: ObjectId,
    key: &[u8],
    max_depth: u32,
) -> Option<&'a PdfObj> {
    let mut depth = max_depth;
    loop {
        if depth == 0 {
            return None;
        }
        let dict = doc.get_object(id).ok()?.as_dict().ok()?;
        if let Ok(v) = dict.get(key) {
            return Some(v);
        }
        let parent = dict.get(b"Parent").ok()?;
        id = parent.as_reference().ok()?;
        depth -= 1;
    }
}

fn page_dimensions(doc: &PdfDoc, page_id: ObjectId) -> (f64, f64) {
    let mut w = 612.0_f64;
    let mut h = 792.0_f64;
    if let Some(PdfObj::Array(a)) = inherited_attr(doc, page_id, b"MediaBox", 32) {
        if a.len() == 4 {
            let x0 = obj_num(&a[0]);
            let y0 = obj_num(&a[1]);
            let x1 = obj_num(&a[2]);
            let y1 = obj_num(&a[3]);
            w = (x1 - x0).abs();
            h = (y1 - y0).abs();
        }
    }
    let rotate = inherited_attr(doc, page_id, b"Rotate", 32)
        .map(|o| (obj_num(o) as i64).rem_euclid(360))
        .unwrap_or(0);
    if rotate == 90 || rotate == 270 {
        std::mem::swap(&mut w, &mut h);
    }
    (round1(w), round1(h))
}

fn get_doc_meta(path: &str) -> Result<(u32, Vec<(f64, f64)>), String> {
    let doc = load_doc_cached(path)?;
    let pages = doc.get_pages(); // BTreeMap<页号(1-based), ObjectId>，已按页序排列
    let cnt = pages.len() as u32;
    let mut dims = Vec::with_capacity(pages.len());
    for (_n, id) in &pages {
        dims.push(page_dimensions(&doc, *id));
    }
    Ok((cnt, dims))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

// 抽第 page 页（0-based）成独立单页 PDF，返回 bytes。
// 做法：clone 出整份文档 → 删掉除目标页外的所有页 → prune 孤立对象 → 重排+压缩 → 存到内存。
// 关键：删页不触碰存活页的内容流与资源字典，因此字体/图像随页一起保留（已 pdf.js 验证）。
fn extract_page_pdf(path: &str, page: usize) -> Result<Vec<u8>, String> {
    let mut doc = load_doc_cached(path)?;
    let pages = doc.get_pages();
    let total = pages.len();
    let target_1based = (page as u32) + 1;
    if page >= total {
        return Err(format!("page {} out of range (total {})", page, total));
    }
    let to_delete: Vec<u32> = pages
        .keys()
        .cloned()
        .filter(|&n| n != target_1based)
        .collect();
    doc.delete_pages(&to_delete);
    doc.prune_objects();
    doc.renumber_objects();
    doc.compress();
    let mut buf: Vec<u8> = Vec::new();
    doc.save_to(&mut buf).map_err(|e| format!("save: {e:?}"))?;
    Ok(buf)
}

// ----------------------------------------------------------------------------
// HTTP 工具
// ----------------------------------------------------------------------------
struct User {
    uid: String,
    username: String,
    is_admin: bool,
}

fn header_val<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn get_user(headers: &[Header]) -> Option<User> {
    let uid = header_val(headers, "X-Trim-Userid").map(|s| s.to_string());
    let is_admin = header_val(headers, "X-Trim-Isadmin") == Some("true");
    let username = header_val(headers, "X-Trim-Username").map(|s| s.to_string());
    if cfg().require_auth && uid.is_none() {
        return None;
    }
    let uid = uid.unwrap_or_else(|| "debug".to_string());
    let username = username.unwrap_or_else(|| uid.clone());
    Some(User {
        uid,
        username,
        is_admin,
    })
}

fn strip_prefix(path: &str) -> String {
    let path = path.split('?').next().unwrap_or("");
    let prefix = &cfg().gateway_prefix;
    let mut p = if !prefix.is_empty() && path.starts_with(prefix.as_str()) {
        &path[prefix.len()..]
    } else {
        path
    };
    if p.is_empty() {
        p = "/";
    }
    if !p.starts_with('/') {
        format!("/{}", p)
    } else {
        p.to_string()
    }
}

fn parse_query(url: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    if let Some(qs) = url.split('?').nth(1) {
        for pair in qs.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("");
            let v = it.next().unwrap_or("");
            if !k.is_empty() {
                let kd = urlencoding::decode(k).map(|c| c.into_owned()).unwrap_or_else(|_| k.to_string());
                let vd = urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string());
                m.insert(kd, vd);
            }
        }
    }
    m
}

fn json_response(value: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(&value).unwrap_or_default();
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap();
    Response::from_data(body).with_header(header)
}

fn error_response(code: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(msg.to_string()).with_status_code(code)
}

fn mime_for(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "json" | "map" => "application/json; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

// 静态资源防目录穿透
fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim_start_matches('/');
    let full = root.join(rel);
    let real = fs::canonicalize(&full).ok()?;
    let root_real = fs::canonicalize(root).ok()?;
    if real == root_real || real.starts_with(&root_real) {
        Some(real)
    } else {
        None
    }
}

// ----------------------------------------------------------------------------
// 请求处理
// ----------------------------------------------------------------------------
fn handle(request: tiny_http::Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let headers: Vec<Header> = request.headers().to_vec();
    let path = strip_prefix(&url);
    let query = parse_query(&url);

    // 读取 body（仅 POST 需要）
    let resp = route(&method, &path, &query, &headers, request);
    // route 内部已消费 request 并发送响应
    let _ = resp;
}

fn route(
    method: &Method,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &[Header],
    mut request: tiny_http::Request,
) {
    macro_rules! send {
        ($resp:expr) => {{
            let _ = request.respond($resp);
            return;
        }};
    }

    // ---------------- API ----------------
    if path == "/api/me" && *method == Method::Get {
        let user = match get_user(headers) {
            Some(u) => u,
            None => send!(error_response(403, "Forbidden: gateway auth required")),
        };
        send!(json_response(serde_json::json!({
            "uid": user.uid,
            "username": user.username,
            "isAdmin": user.is_admin,
        })));
    }

    if path == "/api/books" && *method == Method::Get {
        let user = match get_user(headers) {
            Some(u) => u,
            None => send!(error_response(403, "Forbidden: gateway auth required")),
        };
        let query_path = query.get("path").cloned().unwrap_or_default();
        let query_scan = query.get("scan").cloned().unwrap_or_default();
        let uid = &user.uid;

        let mut file_map = load_file_map(uid);
        if file_map.is_empty() || query_scan == "all" {
            file_map = scan_all(uid);
        }

        let roots = collect_roots();
        let real_root_ids: Vec<String> = if query_path.is_empty() {
            roots
                .iter()
                .filter_map(|r| fs::canonicalize(r).ok())
                .map(|rp| hash_id_for(&rp))
                .collect()
        } else {
            vec![query_path.clone()]
        };

        let progress = load_progress(uid);
        let mut books: Vec<serde_json::Value> = Vec::new();
        let mut history: Vec<serde_json::Value> = Vec::new();

        for (k, v) in &file_map {
            let base = serde_json::json!({
                "id": v.id,
                "fid": v.fid,
                "name": v.name,
                "size": v.size,
                "mtime": v.mtime,
                "type": v.type_,
            });
            if real_root_ids.contains(&v.fid) {
                books.push(base.clone());
            }
            if let Some(p) = progress.get(k) {
                if history.len() < 10 {
                    let mut hb = base.clone();
                    let obj = hb.as_object_mut().unwrap();
                    obj.insert(
                        "progress".to_string(),
                        serde_json::json!({
                            "page": p.get("page").cloned().unwrap_or(serde_json::json!(0)),
                            "totalPages": p.get("totalPages").cloned().unwrap_or(serde_json::json!(0)),
                            "percent": p.get("percent").cloned().unwrap_or(serde_json::json!(0)),
                            "updatedAt": p.get("updatedAt").cloned().unwrap_or(serde_json::json!(0)),
                        }),
                    );
                    history.push(hb);
                }
            }
        }
        history.sort_by(|a, b| {
            let ua = a["progress"]["updatedAt"].as_i64().unwrap_or(0);
            let ub = b["progress"]["updatedAt"].as_i64().unwrap_or(0);
            ub.cmp(&ua)
        });

        send!(json_response(serde_json::json!({
            "books": books,
            "history": history,
            "count": books.len(),
            "username": user.username,
        })));
    }

    if path == "/api/meta" && *method == Method::Get {
        let user = match get_user(headers) {
            Some(u) => u,
            None => send!(error_response(403, "Forbidden: gateway auth required")),
        };
        let bid = query.get("id").cloned().unwrap_or_default();
        let file_map = load_file_map(&user.uid);
        let entry = match file_map.get(&bid) {
            Some(e) if e.type_ == "file" => e,
            _ => send!(error_response(404, "book not found")),
        };
        match get_doc_meta(&entry.path) {
            Ok((cnt, pages)) => {
                let pages_json: Vec<serde_json::Value> = pages
                    .iter()
                    .map(|(w, h)| serde_json::json!({"w": w, "h": h}))
                    .collect();
                let prog = load_progress(&user.uid)
                    .get(&bid)
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                send!(json_response(serde_json::json!({
                    "pageCount": cnt,
                    "pages": pages_json,
                    "id": bid,
                    "name": entry.name,
                    "progress": prog,
                })));
            }
            Err(e) => {
                log(&format!("meta error {}: {}", entry.name, e));
                send!(json_response(serde_json::json!({
                    "error": "meta_failed", "detail": e
                })));
            }
        }
    }

    if path == "/api/pagepdf" && *method == Method::Get {
        let user = match get_user(headers) {
            Some(u) => u,
            None => send!(error_response(403, "Forbidden: gateway auth required")),
        };
        let bid = query.get("id").cloned().unwrap_or_default();
        // 页码：对外统一 0-based（第一页 = 0）
        let page: i64 = query.get("page").and_then(|s| s.parse().ok()).unwrap_or(0);
        if page < 0 {
            send!(error_response(400, "bad page"));
        }
        let file_map = load_file_map(&user.uid);
        let entry = match file_map.get(&bid) {
            Some(e) if e.type_ == "file" => e.clone(),
            _ => send!(error_response(404, "book not found")),
        };
        let start = std::time::Instant::now();
        match extract_page_pdf(&entry.path, page as usize) {
            Ok(data) => {
                log(&format!(
                    "{} 请求第 {} 页耗时：{:?}",
                    bid,
                    page,
                    start.elapsed()
                ));
                let header =
                    Header::from_bytes(&b"Content-Type"[..], &b"application/pdf"[..]).unwrap();
                send!(Response::from_data(data).with_header(header));
            }
            Err(e) => {
                log(&format!("slice error {} p{}: {}", entry.name, page, e));
                send!(error_response(404, "page out of range"));
            }
        }
    }

    if path == "/api/progress" {
        let user = match get_user(headers) {
            Some(u) => u,
            None => send!(error_response(403, "Forbidden: gateway auth required")),
        };
        let mut bid = query.get("id").cloned().unwrap_or_default();

        if *method == Method::Get {
            let prog = load_progress(&user.uid);
            let entry = prog.get(&bid).cloned().unwrap_or(serde_json::Value::Null);
            send!(json_response(serde_json::json!({"id": bid, "progress": entry})));
        }

        if *method == Method::Post {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let payload: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
            if let Some(pid) = payload.get("id").and_then(|v| v.as_str()) {
                if !pid.is_empty() {
                    bid = pid.to_string();
                }
            }
            if bid.is_empty() {
                let _ = request.respond(error_response(400, "missing id"));
                return;
            }
            let mut entry = serde_json::Map::new();
            for k in ["page", "frac", "name", "scale", "totalPages", "percent"] {
                if let Some(v) = payload.get(k) {
                    entry.insert(k.to_string(), v.clone());
                }
            }
            let saved = save_progress_entry(&user.uid, &bid, entry);
            let _ = request.respond(json_response(serde_json::json!({
                "ok": saved.is_some(),
                "progress": saved.unwrap_or(serde_json::Value::Null),
            })));
            return;
        }
        send!(error_response(405, "method not allowed"));
    }

    // ---------------- 静态文件 ----------------
    if *method == Method::Get || *method == Method::Head {
        let web_root = &cfg().web_root;
        // 根路径 → index.html
        if path == "/" {
            serve_file(request, &web_root.join("index.html"), "text/html; charset=utf-8");
            return;
        }
        // 尝试真实文件
        if let Some(full) = safe_join(web_root, path) {
            if full.is_file() {
                let mime = mime_for(&full.to_string_lossy());
                serve_file(request, &full, mime);
                return;
            }
        }
        // SPA 回退：无扩展名的路径回退到 index.html
        let last = path.rsplit('/').next().unwrap_or("");
        if !last.contains('.') {
            let idx = web_root.join("index.html");
            if idx.is_file() {
                serve_file(request, &idx, "text/html; charset=utf-8");
                return;
            }
        }
        send!(error_response(404, "not found"));
    }

    send!(error_response(404, "not found"));
}

fn serve_file(request: tiny_http::Request, path: &Path, mime: &str) {
    match fs::read(path) {
        Ok(data) => {
            let header = Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap();
            let _ = request.respond(Response::from_data(data).with_header(header));
        }
        Err(_) => {
            let _ = request.respond(error_response(404, "not found"));
        }
    }
}

// ----------------------------------------------------------------------------
// 启动
// ----------------------------------------------------------------------------
fn main() {
    init_config();

    // 解析 --port（TCP 调试模式）
    let mut port: u16 = 0;
    let mut host = "0.0.0.0".to_string();
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--port" | "-p" => {
                if i + 1 < argv.len() {
                    port = argv[i + 1].parse().unwrap_or(0);
                    i += 1;
                }
            }
            "--host" | "-H" => {
                if i + 1 < argv.len() {
                    host = argv[i + 1].clone();
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let _ = fs::create_dir_all(&cfg().data_dir);

    log(&format!(
        "=== pdfreader rust server boot === prefix={} webroot={} (index={})",
        cfg().gateway_prefix,
        cfg().web_root.display(),
        cfg().web_root.join("index.html").is_file()
    ));

    let server = if port > 0 {
        log(&format!("TCP debug mode on {}:{}", host, port));
        Server::http(format!("{}:{}", host, port)).expect("bind tcp failed")
    } else {
        let sock = &cfg().sock_path;
        let _ = fs::remove_file(sock);
        log(&format!("listening unix socket: {}", sock));
        let s = Server::http_unix(Path::new(sock)).expect("bind unix socket failed");
        // socket 权限 0666，网关(nginx worker)可读写
        chmod_0666(sock);
        s
    };

    let server = Arc::new(server);
    let mut handles = Vec::new();
    // 同步线程池：抽页是 IO 主导，8 线程足够，且每请求内存随函数返回释放
    for _ in 0..8 {
        let srv = server.clone();
        handles.push(std::thread::spawn(move || loop {
            match srv.recv() {
                Ok(req) => {
                    // 单请求 panic 不拖垮整个进程：捕获后由上层继续 recv
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(req)));
                    if let Err(e) = r {
                        let msg = e
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| e.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown panic".to_string());
                        log(&format!("request handler panic: {}", msg));
                    }
                }
                Err(_) => break,
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
}

fn chmod_0666(path: &str) {
    use std::ffi::CString;
    if let Ok(c) = CString::new(path) {
        unsafe {
            libc::chmod(c.as_ptr(), 0o666);
        }
    }
}
