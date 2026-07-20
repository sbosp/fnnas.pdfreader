// PDF 阅读器 — Rust 服务端 (fnOS 统一网关版)
//
// 设计要点：
//   * 纯 Rust，零 C 依赖：用 pdf_oxide 把指定页抽成独立单页 PDF 切片返回
//     （交由前端 pdf.js 渲染）
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

use pdf_oxide::editor::DocumentEditor;
use pdf_oxide::api::Pdf;
use pdf_oxide::rendering::{render_page, ImageFormat, RenderOptions};

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
// PDF 处理（纯 Rust，pdf_oxide 0.3.74）
// ----------------------------------------------------------------------------
//
// 不做文档缓存：每次请求直接打开解析即可，无常驻内存、无锁竞争。

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

// 读元数据：页数 + 每页 (宽,高)。宽高由 get_page_media_box 计算得到
// （旋转 90/270 时交换宽高以匹配前端显示方向）。
// meta 结果缓存：path -> (mtime, page_count, dims)。
// meta 的输出（页数+每页尺寸）只随文件内容变化，同一本书算一次即可，
// 之后按 mtime 命中秒回，省掉每次遍历数百页读 MediaBox 的开销。
static META_CACHE: OnceLock<Mutex<BTreeMap<String, (i64, u32, Vec<(f64, f64)>)>>> = OnceLock::new();

fn meta_cache() -> &'static Mutex<BTreeMap<String, (i64, u32, Vec<(f64, f64)>)>> {
    META_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

// 读取文档元信息（总页数 + 每页尺寸）。
// 两层优化：
//   1) 结果缓存（META_CACHE，按 mtime 失效）——算一次后直接秒回，不再遍历所有页。
//   2) 首次计算时复用抽页那份共享 DocumentEditor（DOC_CACHE），而不是再
//      PdfDocument::open 整本 88MB——避免同一本书被 open 两遍、各占 ~182MB。
fn get_doc_meta(path: &str) -> Result<(u32, Vec<(f64, f64)>), String> {
    let mtime = file_mtime(path);
    // 1) 结果缓存命中：秒回。
    {
        let cache = meta_cache().lock().unwrap();
        if let Some((m, cnt, dims)) = cache.get(path) {
            if *m == mtime {
                return Ok((*cnt, dims.clone()));
            }
        }
    }
    let t0 = std::time::Instant::now();
    // 2) 复用共享 editor（首次 open 后 meta/pagepdf 共享同一份内存）。
    let cached = get_cached_doc(path)?;
    let t_open = t0.elapsed();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = cached.lock().unwrap();
        let ed = &mut guard.editor;
        let cnt = ed.current_page_count() as u32;
        let mut dims = Vec::with_capacity(cnt as usize);
        for i in 0..cnt as usize {
            // MediaBox: [x0, y0, x1, y1]，用户空间单位
            let bx = ed
                .get_page_media_box(i)
                .map_err(|e| format!("media_box {i}: {e:?}"))?;
            let mut w = (bx[2] - bx[0]).abs() as f64;
            let mut h = (bx[3] - bx[1]).abs() as f64;
            let rot = ed.get_page_rotation(i).unwrap_or(0).rem_euclid(360);
            if rot == 90 || rot == 270 {
                std::mem::swap(&mut w, &mut h);
            }
            dims.push((round1(w), round1(h)));
        }
        Ok::<(u32, Vec<(f64, f64)>), String>((cnt, dims))
    }));
    let (cnt, dims) = match result {
        Ok(r) => r?,
        Err(_) => return Err("pdf_oxide panic while reading meta".to_string()),
    };
    let t_dims = t0.elapsed();
    log(&format!(
        "meta 计时 [{}]: 取缓存editor={:?} dims={:?}(+{:?}) 共{}页 (首次，已写入结果缓存)",
        path,
        t_open,
        t_dims,
        t_dims - t_open,
        cnt
    ));
    // 写入结果缓存。
    meta_cache()
        .lock()
        .unwrap()
        .insert(path.to_string(), (mtime, cnt, dims.clone()));
    Ok((cnt, dims))
}

// ----------------------------------------------------------------------------
// 抽页：文档级缓存 + 并发信号量（本地基准测试驱动的设计）
// ----------------------------------------------------------------------------
//
// 【为什么需要缓存】本地实测（88MB / 383 页大书，release）：
//   - 单页抽取仅 0.02s，串行时进程峰值 RSS 稳定在 ~182MB（= 一次 open 把整本
//     文件读进内存 + 解析缓存）。
//   - 但 `DocumentEditor::open` = `std::fs::read` 整本文件；若每个 pagepdf 请求
//     都独立 open，8 个后端 worker 并发时峰值 RSS 线性叠成 ~1.4GB（实测 1419MB），
//     NAS 内存有限 → 内核 OOM-kill → 服务重启 → 后续请求瞬间 502。
//   - 这就是 NAS 上「前 2 个请求各卡 ~9.5s 后 502、其余 173ms 502」的真凶：
//     不是单页慢，是并发下内存耗尽拖垮整个进程。
//
// 【修法】同一本书只 open 一次，缓存 DocumentEditor（按 mtime 失效），所有并发
//   请求共享那一份 ~182MB。pdf_oxide 的 extract_pages_to_bytes 抽完会把内部
//   page_order/modified_objects 完全还原（源码 1195-1207「Always restore」），
//   editor 状态不变，故可安全复用抽任意页而不串页。缓存命中后不再重复 fs::read，
//   峰值内存 = 打开的书本数 × 182MB，与并发数解耦。
//
// 【并发信号量】再加一道保险：同时最多 MAX_EXTRACT 个抽页在跑。即便前端狂发、
//   或同时打开多本大书，也不会让内存无上限膨胀。

// 单本书的缓存条目：mtime 用于失效；editor 用 Mutex 串行化（extract 是 &mut self）。
// last_used 用于 LRU 淘汰——避免打开的文档 editor 永久驻留导致内存只增不减。
struct CachedDoc {
    mtime: i64,
    editor: DocumentEditor,
    last_used: std::time::Instant,
}

// path -> 缓存条目。外层 Mutex 只在“取/换条目”时短暂持有，不覆盖抽页耗时。
static DOC_CACHE: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<CachedDoc>>>>> = OnceLock::new();
// 抽页并发闸门：用一个「许可数」计数信号量控制同时抽页数。
static EXTRACT_GATE: OnceLock<(Mutex<usize>, std::sync::Condvar)> = OnceLock::new();
const MAX_EXTRACT: usize = 3;

// 【文档缓存容量上限 + 空闲淘汰】
// 之前 DOC_CACHE 只增不减：每打开一本书，其 DocumentEditor（单本大书 ~182MB，
// = fs::read 整本 + 解析缓存）就永久驻留内存。多开几本大书即累积到近 1GB，
// NAS 内存有限 → 疯狂 swap → 进程「休眠 / CPU 0% 却极慢」（慢的真凶是换页）。
// 对策：限制同时缓存的文档数，并淘汰空闲超时的条目，让内存有明确天花板。
//   PDFR_DOC_CACHE_MAX   最多同时缓存几本书的 editor（默认 2）
//   PDFR_DOC_IDLE_SECS   条目空闲多少秒后可被淘汰（默认 120s）
const DOC_CACHE_MAX_DEFAULT: usize = 2;
const DOC_IDLE_SECS_DEFAULT: u64 = 120;

fn doc_cache_max() -> usize {
    std::env::var("PDFR_DOC_CACHE_MAX")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&v| v >= 1)
        .unwrap_or(DOC_CACHE_MAX_DEFAULT)
}
fn doc_idle_secs() -> u64 {
    std::env::var("PDFR_DOC_IDLE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DOC_IDLE_SECS_DEFAULT)
}

// 【扫描版 PDF 降体积】
// 扫描版每页是一整张巨图（实测某教辅每页 4304×6142），直接抽 PDF 切片会把整张
// 原图（~5MB）原样搬给前端，NAS 慢网下必然超时、打满 CPU、OOM 重启。
// 对策：抽出的原始切片若超过 RASTER_THRESHOLD，判定为「图片主导页」，改用
// tiny-skia 把该页渲染成低 DPI JPEG，再包成单页 PDF 返回（前端 pdf.js 照常解析）。
// 实测：5MB 页 → ~30KB，缩小 ~160×，本机渲染 0.3s（NAS 留足余量不超时）。
// 矢量页（纯文字排版）切片本就很小，不触发栅格化，保持矢量清晰。
// 默认值（可被环境变量覆盖，方便运维在不重编译的前提下调参 / 排障）：
//   PDFR_RASTER_THRESHOLD  原始切片超过多少字节才栅格化（默认 800KB）
//   PDFR_RASTER_DPI        栅格化 DPI（默认 120）
//   PDFR_RASTER_QUALITY    JPEG 质量 1..=100（默认 75）
const RASTER_THRESHOLD_DEFAULT: usize = 800 * 1024;
const RASTER_DPI_DEFAULT: u32 = 120;
const RASTER_JPEG_QUALITY_DEFAULT: u8 = 75;

fn raster_threshold() -> usize {
    std::env::var("PDFR_RASTER_THRESHOLD")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(RASTER_THRESHOLD_DEFAULT)
}
fn raster_dpi() -> u32 {
    std::env::var("PDFR_RASTER_DPI")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(RASTER_DPI_DEFAULT)
}
fn raster_quality() -> u8 {
    std::env::var("PDFR_RASTER_QUALITY")
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .filter(|&v| v >= 1 && v <= 100)
        .unwrap_or(RASTER_JPEG_QUALITY_DEFAULT)
}

fn doc_cache() -> &'static Mutex<BTreeMap<String, Arc<Mutex<CachedDoc>>>> {
    DOC_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn extract_gate() -> &'static (Mutex<usize>, std::sync::Condvar) {
    EXTRACT_GATE.get_or_init(|| (Mutex::new(0), std::sync::Condvar::new()))
}

// RAII 许可：构造时占用一个抽页名额（不够就等），析构时归还并唤醒等待者。
struct ExtractPermit;
impl ExtractPermit {
    fn acquire() -> Self {
        let (lock, cv) = extract_gate();
        let mut n = lock.lock().unwrap();
        while *n >= MAX_EXTRACT {
            n = cv.wait(n).unwrap();
        }
        *n += 1;
        ExtractPermit
    }
}
impl Drop for ExtractPermit {
    fn drop(&mut self) {
        let (lock, cv) = extract_gate();
        let mut n = lock.lock().unwrap();
        *n = n.saturating_sub(1);
        cv.notify_one();
    }
}

fn file_mtime(path: &str) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// 取（或打开并缓存）某本书的 DocumentEditor。返回内层 Arc<Mutex<CachedDoc>>，
// 抽页时只锁这一本，不阻塞其他书。
fn get_cached_doc(path: &str) -> Result<Arc<Mutex<CachedDoc>>, String> {
    let mtime = file_mtime(path);
    {
        let map = doc_cache().lock().unwrap();
        if let Some(entry) = map.get(path) {
            // 命中且未过期：直接复用（不重新 fs::read），并刷新 last_used。
            let mut g = entry.lock().unwrap();
            if g.mtime == mtime {
                g.last_used = std::time::Instant::now();
                return Ok(entry.clone());
            }
        }
    }
    // 未命中或已过期：open 一次（可能内部 panic，用 catch_unwind 兜住）。
    let path_owned = path.to_string();
    let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        DocumentEditor::open(&path_owned).map_err(|e| format!("open: {e:?}"))
    }))
    .map_err(|_| "pdf_oxide panic on open".to_string())??;
    let entry = Arc::new(Mutex::new(CachedDoc {
        mtime,
        editor: opened,
        last_used: std::time::Instant::now(),
    }));
    {
        let mut map = doc_cache().lock().unwrap();
        map.insert(path.to_string(), entry.clone());
        evict_docs(&mut map, path);
    }
    Ok(entry)
}

// LRU + 空闲淘汰：把过期条目（空闲超时）和超出容量的最久未用条目移除。
// keep 是当前请求正在使用的 path，绝不淘汰它。被移除的 Arc 若无其他持有者，
// 其 DocumentEditor（连同整本文件缓存）随即析构，内存归还。
fn evict_docs(map: &mut BTreeMap<String, Arc<Mutex<CachedDoc>>>, keep: &str) {
    let now = std::time::Instant::now();
    let idle = std::time::Duration::from_secs(doc_idle_secs());
    let max = doc_cache_max();

    // 先按空闲超时淘汰（idle=0 表示不按时间淘汰，只按容量）。
    if !idle.is_zero() {
        let stale: Vec<String> = map
            .iter()
            .filter(|(p, e)| {
                p.as_str() != keep
                    && e.lock()
                        .map(|g| now.duration_since(g.last_used) > idle)
                        .unwrap_or(false)
            })
            .map(|(p, _)| p.clone())
            .collect();
        for p in stale {
            map.remove(&p);
            log(&format!(
                "doc cache evict (idle > {}s): {} ; now {} cached",
                idle.as_secs(),
                p,
                map.len()
            ));
        }
    }

    // 再按容量上限淘汰：超出部分，移除 last_used 最早的（keep 除外）。
    while map.len() > max {
        let victim = map
            .iter()
            .filter(|(p, _)| p.as_str() != keep)
            .min_by_key(|(_, e)| {
                e.lock()
                    .map(|g| g.last_used)
                    .unwrap_or_else(|_| std::time::Instant::now())
            })
            .map(|(p, _)| p.clone());
        match victim {
            Some(p) => {
                map.remove(&p);
                log(&format!(
                    "doc cache evict (over capacity, max={}): {} ; now {} cached",
                    max,
                    p,
                    map.len()
                ));
            }
            None => break, // 只剩 keep 了，停止
        }
    }
}

// 抽第 page 页（0-based）成独立单页 PDF，返回 bytes。
// 复用缓存的 DocumentEditor（多请求共享同一份内存），并受并发信号量限制。
//
// catch_unwind：pdf_oxide 对某些页可能内部 panic（unwrap/越界等）而非返回 Err。
// 若不拦截，panic 冒泡到 worker 的 catch_unwind 时请求已被 drop、未响应，
// 网关拿到裸断连接返回 502。这里把 panic 转成 Err，保证总能回正常 HTTP 响应。
fn extract_page_pdf(path: &str, page: usize) -> Result<Vec<u8>, String> {
    // 先过并发闸门：同时最多 MAX_EXTRACT 个抽页，防止内存无上限膨胀。
    let _permit = ExtractPermit::acquire();
    let cached = get_cached_doc(path)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = cached.lock().unwrap();
        // ① 先抽原始单页切片
        let raw = guard
            .editor
            .extract_pages_to_bytes(&[page])
            .map_err(|e| format!("extract_pages_to_bytes: {e:?}"))?;
        // ② 小切片（矢量页）直接返回，保持清晰
        if raw.len() <= raster_threshold() {
            return Ok(raw);
        }
        // ③ 大切片（扫描图主导页）→ 栅格化降体积。失败则回退返回原始切片，
        //    保证「宁可大也别不可读」。
        match rasterize_page(&mut guard.editor, page) {
            Ok(small) if !small.is_empty() && small.len() < raw.len() => {
                log(&format!(
                    "rasterize page {} : {} -> {} bytes ({:.1}x)",
                    page,
                    raw.len(),
                    small.len(),
                    raw.len() as f64 / small.len().max(1) as f64
                ));
                Ok(small)
            }
            _ => Ok(raw),
        }
    }));
    match result {
        Ok(r) => r,
        Err(e) => {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(format!("pdf_oxide panic on page {page}: {msg}"))
        }
    }
}

// 把第 page 页（0-based）用 tiny-skia 渲染成低 DPI JPEG，再包成单页 PDF。
// 复用同一个已缓存的 DocumentEditor（其 source() 即渲染所需的 PdfDocument），
// 不重新读盘。任何一步出错都返回 Err，由调用方回退到原始切片。
fn rasterize_page(editor: &mut DocumentEditor, page: usize) -> Result<Vec<u8>, String> {
    let mut opts = RenderOptions::default();
    opts.dpi = raster_dpi();
    opts.format = ImageFormat::Jpeg;
    opts.jpeg_quality = raster_quality();
    opts.background = Some([1.0, 1.0, 1.0, 1.0]);

    let img = render_page(editor.source(), page, &opts)
        .map_err(|e| format!("render_page: {e:?}"))?;
    let mut pdf = Pdf::from_image_bytes(&img.data)
        .map_err(|e| format!("from_image_bytes: {e:?}"))?;
    pdf.to_bytes().map_err(|e| format!("to_bytes: {e:?}"))
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
        let t_meta = std::time::Instant::now();
        let file_map = load_file_map(&user.uid);
        let t_map = t_meta.elapsed();
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
                log(&format!(
                    "meta 总耗时: {:?} (load_file_map={:?}) id={}",
                    t_meta.elapsed(),
                    t_map,
                    bid
                ));
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
                // 返回 500 而非 404：让前端与日志能区分「抽页失败」和「页码越界」，
                // 且始终发送一个正常 HTTP 响应，避免裸断连接导致网关 502。
                send!(error_response(500, "extract page failed"));
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

    let argv: Vec<String> = std::env::args().collect();

    // 解析 --port（TCP 调试模式）
    let mut port: u16 = 0;
    let mut host = "0.0.0.0".to_string();
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
