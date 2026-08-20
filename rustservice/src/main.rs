// PDF 阅读器 — Rust 后端（fnOS 统一网关版）
//
// 设计要点：
//   - 纯 Rust：axum + tokio + pdf_oxide（tiny-skia 光栅化），单二进制、可交叉编译 aarch64。
//   - 路径化 API：前端传书库内真实路径，后端严格校验路径在书库根内。
//   - 并发：每个请求一个 task；渲染/读盘走 spawn_blocking，互不堵塞 accept。
//   - 双层缓存：图片与 meta 缓存在 {书库根}/.pdfreader-cache/。
//   - 文档句柄 Arc 共享：热缓存命中后同书多页可并行渲染。
//   - 与 fnOS cmd/main 注入的 PDFR_* 环境变量对齐，监听 Unix Socket(生产)/TCP(调试)。
mod config;
mod library;
mod pathsafe;
mod pdfdoc;
mod progress;
mod trimapi;

use axum::body::{Body, Bytes};
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use config::{cfg, collect_roots, init_config, refresh_live_roots, with_request_uid};
use pathsafe::{file_name_of, resolve_in_roots};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::Semaphore;

// ----------------------------------------------------------------------------
// 鉴权：读取网关注入的 X-Trim-* 头
// ----------------------------------------------------------------------------
#[derive(Clone)]
struct User {
    uid: String,
    username: String,
    is_admin: bool,
}

fn header_val<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn get_user(headers: &HeaderMap) -> Option<User> {
    let uid = header_val(headers, "x-trim-userid").map(|s| s.to_string());
    let is_admin = header_val(headers, "x-trim-isadmin") == Some("true");
    let username = header_val(headers, "x-trim-username").map(|s| s.to_string());
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

impl<S: Send + Sync> FromRequestParts<S> for User {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        get_user(&parts.headers).ok_or_else(|| {
            (StatusCode::FORBIDDEN, "Forbidden: gateway auth required").into_response()
        })
    }
}

// ----------------------------------------------------------------------------
// HTTP 工具
// ----------------------------------------------------------------------------
fn json_ok(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body,
    )
        .into_response()
}

fn json_nocache(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
        ],
        body,
    )
        .into_response()
}

fn text_status(code: StatusCode, msg: &'static str) -> Response {
    (code, msg).into_response()
}

fn jpeg_ok(data: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        data,
    )
        .into_response()
}

fn query_map(uri: &Uri) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Some(q) = uri.query() else {
        return m;
    };
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.find('=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, ""),
        };
        m.insert(
            urlencoding::decode(k).unwrap_or_default().into_owned(),
            urlencoding::decode(v).unwrap_or_default().into_owned(),
        );
    }
    m
}

fn json_esc(s: &str) -> String {
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

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, Response> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| text_status(StatusCode::INTERNAL_SERVER_ERROR, "task join failed"))
}

/// 同时渲染的上限，避免书架一次涌入几十张封面把内存打爆。
/// PDFR_RENDER_CONCURRENCY 可配，默认 2..=4（跟 CPU 走）。
fn render_sem() -> &'static Semaphore {
    static S: std::sync::OnceLock<Semaphore> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let n = std::env::var("PDFR_RENDER_CONCURRENCY")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
                    .clamp(2, 4)
            });
        Semaphore::new(n.max(1))
    })
}

// ----------------------------------------------------------------------------
// 静态资源（网关前缀剥离 + 防目录穿透 + SPA 回退）
// ----------------------------------------------------------------------------
fn mime_of(p: &Path) -> &'static str {
    match p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "json" | "map" => "application/json; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn strip_prefix<'a>(path: &'a str) -> &'a str {
    path.strip_prefix(cfg().gateway_prefix.as_str()).unwrap_or(path)
}

fn serve_static_sync(rel: &str) -> Response {
    let rel = rel.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let root = &cfg().web_root;
    let full = root.join(rel);
    let ok = full
        .canonicalize()
        .ok()
        .map(|p| p.starts_with(root))
        .unwrap_or(false);
    let target = if ok && full.is_file() {
        full
    } else {
        root.join("index.html")
    };
    match std::fs::read(&target) {
        Ok(data) => {
            let ctype = mime_of(&target);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ctype)
                .body(Body::from(data))
                .unwrap_or_else(|_| text_status(StatusCode::INTERNAL_SERVER_ERROR, "response"))
        }
        Err(_) => text_status(StatusCode::NOT_FOUND, "前端未构建"),
    }
}

async fn fallback(uri: Uri) -> Response {
    let path = strip_prefix(uri.path());
    if path.starts_with("/api/") {
        return text_status(StatusCode::NOT_FOUND, "not found");
    }
    let rel = path.to_string();
    match blocking(move || serve_static_sync(&rel)).await {
        Ok(r) => r,
        Err(e) => e,
    }
}

// ----------------------------------------------------------------------------
// API handlers
// ----------------------------------------------------------------------------

async fn handle_me(u: User) -> Response {
    json_ok(format!(
        r#"{{"uid":"{}","username":"{}","isAdmin":{}}}"#,
        json_esc(&u.uid),
        json_esc(&u.username),
        u.is_admin
    ))
}

fn entry_json(e: &library::Entry, prog: Option<&progress::Progress>) -> String {
    let segs = pathsafe::rel_segments(Path::new(&e.path));
    let segs_json = segs
        .iter()
        .map(|s| format!("\"{}\"", json_esc(s)))
        .collect::<Vec<_>>()
        .join(",");
    let prog_json = match prog {
        Some(p) => format!(",\"progress\":{}", progress::progress_to_json(p)),
        None => String::new(),
    };
    format!(
        r#"{{"name":"{}","path":"{}","type":"{}","size":{},"mtime":{},"count":{},"segments":[{}]{}}}"#,
        json_esc(&e.name),
        json_esc(&e.path),
        e.kind,
        e.size,
        e.mtime,
        e.count,
        segs_json,
        prog_json
    )
}

fn list_body(u: &User, raw: String) -> Result<String, Response> {
    // 只在书库根问一次开放 API（增量并进缓存）；封面/翻页走 collect_roots 不再打 RPC
    if raw.is_empty() {
        refresh_live_roots();
    }
    let roots = collect_roots();
    let (entries, crumbs, cur_path) = if raw.is_empty() {
        if roots.len() == 1 {
            let r = roots[0].clone();
            (
                library::list_dir(&r),
                library::breadcrumb(&r),
                r.to_string_lossy().to_string(),
            )
        } else {
            (library::roots_as_entries(), Vec::new(), String::new())
        }
    } else {
        match library::resolve_dir(&raw) {
            Some(dir) => {
                let e = library::list_dir(&dir);
                let c = library::breadcrumb(&dir);
                (e, c, dir.to_string_lossy().to_string())
            }
            None => return Err(text_status(StatusCode::NOT_FOUND, "directory not found")),
        }
    };

    let all = progress::load_all(&u.uid);
    let items = entries
        .iter()
        .map(|e| entry_json(e, all.get(&e.path)))
        .collect::<Vec<_>>()
        .join(",");
    let crumbs_json = crumbs
        .iter()
        .map(|(n, p)| {
            format!(
                r#"{{"name":"{}","path":"{}"}}"#,
                json_esc(n),
                json_esc(p)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        r#"{{"path":"{}","breadcrumb":[{}],"items":[{}],"count":{},"username":"{}"}}"#,
        json_esc(&cur_path),
        crumbs_json,
        items,
        entries.len(),
        json_esc(&u.username)
    ))
}

async fn handle_list(u: User, uri: Uri) -> Response {
    let raw = query_map(&uri).get("path").cloned().unwrap_or_default();
    match blocking(move || with_request_uid(&u.uid, || list_body(&u, raw))).await {
        Ok(Ok(body)) => json_ok(body),
        Ok(Err(e)) => e,
        Err(e) => e,
    }
}

fn recent_body(u: &User) -> String {
    let items = progress::recent(&u.uid, 12)
        .into_iter()
        .map(|(path, p)| {
            let pb = PathBuf::from(&path);
            let segs = pathsafe::rel_segments(&pb);
            let segs_json = segs
                .iter()
                .map(|s| format!("\"{}\"", json_esc(s)))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"{{"name":"{}","path":"{}","segments":[{}],"progress":{}}}"#,
                json_esc(&file_name_of(&pb)),
                json_esc(&path),
                segs_json,
                progress::progress_to_json(&p)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"items":[{}]}}"#, items)
}

async fn handle_recent(u: User) -> Response {
    match blocking(move || with_request_uid(&u.uid, || recent_body(&u))).await {
        Ok(body) => json_ok(body),
        Err(e) => e,
    }
}

fn meta_body(raw: String) -> Result<String, Response> {
    let p = match resolve_in_roots(&raw) {
        Some(p) if p.is_file() => p,
        _ => return Err(text_status(StatusCode::NOT_FOUND, "book not found")),
    };
    match pdfdoc::get_meta(&p) {
        Ok(m) => Ok(format!(
            r#"{{"path":"{}","name":"{}","pageCount":{},"width":{:.1},"height":{:.1}}}"#,
            json_esc(&p.to_string_lossy()),
            json_esc(&file_name_of(&p)),
            m.page_count,
            m.width,
            m.height
        )),
        Err(e) => {
            logf!("meta 失败 {}: {}", p.display(), e);
            Err(text_status(StatusCode::INTERNAL_SERVER_ERROR, "meta failed"))
        }
    }
}

async fn handle_meta(u: User, uri: Uri) -> Response {
    let raw = query_map(&uri).get("path").cloned().unwrap_or_default();
    match blocking(move || with_request_uid(&u.uid, || meta_body(raw))).await {
        Ok(Ok(body)) => json_nocache(body),
        Ok(Err(e)) => e,
        Err(e) => e,
    }
}

fn pageimg_body(raw: String, page: usize, dpi: u32) -> Result<Vec<u8>, Response> {
    let p = match resolve_in_roots(&raw) {
        Some(p) if p.is_file() => p,
        _ => return Err(text_status(StatusCode::NOT_FOUND, "book not found")),
    };
    let t = std::time::Instant::now();
    match pdfdoc::render_page_jpeg(&p, page, dpi) {
        Ok(data) => {
            logf!(
                "渲染 {} 第{}页(dpi={}) {}KB 耗时 {:.3}s",
                file_name_of(&p),
                page,
                dpi,
                data.len() / 1024,
                t.elapsed().as_secs_f64()
            );
            Ok(data)
        }
        Err(e) => {
            logf!("渲染失败 {} p{}: {}", file_name_of(&p), page, e);
            Err(text_status(StatusCode::INTERNAL_SERVER_ERROR, "render failed"))
        }
    }
}

async fn handle_pageimg(u: User, uri: Uri) -> Response {
    let q = query_map(&uri);
    let raw = q.get("path").cloned().unwrap_or_default();
    let page: usize = match q.get("page").and_then(|s| s.parse().ok()) {
        Some(p) => p,
        None => return text_status(StatusCode::BAD_REQUEST, "bad page"),
    };
    let dpi: u32 = q
        .get("dpi")
        .and_then(|s| s.parse().ok())
        .map(|d: u32| d.clamp(36, 400))
        .unwrap_or(pdfdoc::RENDER_DPI);

    let Ok(_permit) = render_sem().acquire().await else {
        return text_status(StatusCode::SERVICE_UNAVAILABLE, "server shutting down");
    };
    match blocking(move || with_request_uid(&u.uid, || pageimg_body(raw, page, dpi))).await {
        Ok(Ok(data)) => jpeg_ok(data),
        Ok(Err(e)) => e,
        Err(e) => e,
    }
}

fn progress_get(uid: &str, key: &str) -> String {
    match progress::get_one(uid, key) {
        Some(p) => format!(
            r#"{{"path":"{}","progress":{}}}"#,
            json_esc(key),
            progress::progress_to_json(&p)
        ),
        None => format!(r#"{{"path":"{}","progress":null}}"#, json_esc(key)),
    }
}

fn progress_post(uid: &str, key: &str, buf: &str) -> String {
    let incoming = progress::parse_progress_obj(buf);
    let saved = progress::save_one(uid, key, &incoming);
    format!(
        r#"{{"ok":true,"progress":{}}}"#,
        progress::progress_to_json(&saved)
    )
}

async fn handle_progress(u: User, method: Method, uri: Uri, body: Bytes) -> Response {
    let raw = query_map(&uri).get("path").cloned().unwrap_or_default();
    if raw.is_empty() {
        return text_status(StatusCode::BAD_REQUEST, "missing path");
    }
    match method {
        Method::GET => {
            match blocking(move || {
                with_request_uid(&u.uid, || {
                    let key = resolve_in_roots(&raw)
                        .ok_or_else(|| text_status(StatusCode::NOT_FOUND, "book not found"))?;
                    Ok(progress_get(&u.uid, &key.to_string_lossy()))
                })
            })
            .await
            {
                Ok(Ok(body)) => json_ok(body),
                Ok(Err(e)) => e,
                Err(e) => e,
            }
        }
        Method::POST => {
            let buf = String::from_utf8_lossy(&body).into_owned();
            match blocking(move || {
                with_request_uid(&u.uid, || {
                    let key = resolve_in_roots(&raw)
                        .ok_or_else(|| text_status(StatusCode::NOT_FOUND, "book not found"))?;
                    Ok(progress_post(&u.uid, &key.to_string_lossy(), &buf))
                })
            })
            .await
            {
                Ok(Ok(body)) => json_ok(body),
                Ok(Err(e)) => e,
                Err(e) => e,
            }
        }
        _ => text_status(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

// ----------------------------------------------------------------------------
// 路由
// ----------------------------------------------------------------------------
fn api_router() -> Router {
    Router::new()
        .route("/api/me", get(handle_me))
        .route("/api/list", get(handle_list))
        .route("/api/recent", get(handle_recent))
        .route("/api/meta", get(handle_meta))
        .route("/api/pageimg", get(handle_pageimg))
        .route("/api/progress", get(handle_progress).post(handle_progress))
        .fallback(fallback)
}

fn build_app() -> Router {
    let inner = api_router();
    let prefix = cfg().gateway_prefix.clone();
    // 网关带前缀；TCP 调试不带。两套挂同一份路由。
    Router::new().nest(&prefix, inner.clone()).merge(inner)
}

// ----------------------------------------------------------------------------
// main
// ----------------------------------------------------------------------------
fn parse_args() -> (u16, String) {
    let args: Vec<String> = std::env::args().collect();
    let mut port: u16 = 0;
    let mut host = "0.0.0.0".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--host" if i + 1 < args.len() => {
                host = args[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }
    (port, host)
}

fn main() {
    let (port, host) = parse_args();
    init_config(port, host);
    logf!(
        "=== pdfreader rust server boot === prefix={} webroot={} data={}",
        cfg().gateway_prefix,
        cfg().web_root.display(),
        cfg().data_dir.display()
    );
    // 启动阶段只用文件/环境变量列根目录，绝不能在 listen 之前打开放 API：
    // cmd/main 等 app.sock 大约 5s，RPC 一卡住就会被判启动失败，应用打不开。
    logf!("roots(offline)={:?}", collect_roots());
    logf!(
        "http=axum/tokio  render_concurrency={}",
        render_sem().available_permits()
    );

    pdfdoc::start_doc_idle_reaper();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(serve()) {
        logf!("FATAL {e}");
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), String> {
    let app = build_app();
    let make = app.into_make_service();

    if cfg().port > 0 {
        let addr = format!("{}:{}", cfg().host, cfg().port);
        logf!("TCP debug mode on {}", addr);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("listen {addr}: {e}"))?;
        axum::serve(listener, make)
            .await
            .map_err(|e| format!("serve: {e}"))?;
        return Ok(());
    }

    let sock = cfg().sock_path.clone();
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).map_err(|e| format!("listen unix {sock}: {e}"))?;
    unsafe {
        let c = std::ffi::CString::new(sock.clone()).unwrap();
        libc::chmod(c.as_ptr(), 0o666);
    }
    logf!("listening unix socket: {}", sock);
    axum::serve(listener, make)
        .await
        .map_err(|e| format!("serve: {e}"))?;
    Ok(())
}
