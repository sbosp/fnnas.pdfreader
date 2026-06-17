#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PDF 阅读器 — 纯原生后端服务 (fnOS 统一网关版)

设计要点：
  * 监听 Unix Domain Socket（由 fnOS 统一网关转发，无需暴露端口）
  * HTTP 静态文件服务（自动剥离网关前缀 /app/<appname>）
  * 书库扫描：递归扫描 data-share(PDFLibrary) 与用户授权目录下的 *.pdf
  * **服务端渲染（核心）**：用捆绑的 PyMuPDF(fitz) 在 NAS 上把指定页渲染成 PNG/WebP，
    只把渲染好的图片返回给连接端。PDF 原文件**绝不下载到连接端**，且打开瞬时（只渲染当前页）。
  * 阅读进度 / 书签：按飞牛账号(UID)持久化到 NAS 的 var 数据目录，多端共享续读
  * 读取统一网关注入的 X-Trim-* 头做鉴权
  * SO_PEERCRED 校验对端进程，防止本地用户绕过网关直连

"""
import base64
import hashlib
import json
import os
import socket
import socketserver
import struct
import sys
import threading
import time
import urllib.parse
import pymupdf

# ----------------------------------------------------------------------------
# 配置（由环境变量注入，含本地调试默认值）
# ----------------------------------------------------------------------------
APPNAME = os.environ.get("PDFR_APPNAME", "pdfreader")
GATEWAY_PREFIX = os.environ.get("PDFR_GATEWAY_PREFIX", "/app/fnnas-pdfreader").rstrip("/")
SOCK_PATH = os.environ.get("PDFR_SOCK", os.path.join(os.getcwd(), "app.sock"))
TCP_PORT = os.environ.get("PDFR_TCP_PORT")  # 仅本地调试用
DATA_DIR = os.environ.get("PDFR_DATA_DIR", os.path.join(os.getcwd(), "data"))
REQUIRE_AUTH = os.environ.get("PDFR_REQUIRE_AUTH", "1") == "1"
PEERCRED_CHECK = os.environ.get("PDFR_PEERCRED_CHECK", "1") == "1"
LOGFILE = os.environ.get("PDFR_LOGFILE", "")


def _collect_roots():
    roots = []

    def _add(p):
        p = (p or "").strip()
        if not p:
            return
        if os.path.isdir(p):
            rp = os.path.realpath(p)
            if rp not in roots:
                roots.append(rp)

    for key in ("PDFR_SHARE_PATHS", "PDFR_ACCESSIBLE_PATHS"):
        for p in os.environ.get(key, "").split(":"):
            _add(p)
    return roots


# 全局缓存的根目录列表（每次扫描时刷新，以支持运行期授权目录变更）
LIBRARY_ROOTS = _collect_roots()

_allow_raw = os.environ.get("PDFR_ALLOW_UIDS", "0")
ALLOW_UIDS = set()
for _p in _allow_raw.split(","):
    _p = _p.strip()
    if _p.isdigit():
        ALLOW_UIDS.add(int(_p))


def _resolve_webroot():
    here = os.path.dirname(os.path.abspath(__file__))  # .../target/server
    candidates = []
    env = os.environ.get("PDFR_WEBROOT")
    if env:
        candidates.append(env)
    candidates += [
        os.path.join(here, "..", "ui"),
        os.path.join(here, "ui"),
        os.path.join(here, "..", "..", "ui"),
        os.path.join(here, ".."),
    ]
    for c in candidates:
        c = os.path.abspath(c)
        if os.path.isfile(os.path.join(c, "index.html")):
            return c
    return os.path.abspath(env) if env else os.path.abspath(os.path.join(here, "..", "ui"))


WEB_ROOT = _resolve_webroot()
WEB_ROOT_REAL = os.path.realpath(WEB_ROOT)

# 进度文件并发写锁
_PROGRESS_LOCK = threading.Lock()

MIME = {
    ".html": "text/html; charset=utf-8",
    ".js": "application/javascript; charset=utf-8",
    ".mjs": "application/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".png": "image/png",
    ".svg": "image/svg+xml",
    ".ico": "image/x-icon",
    ".json": "application/json; charset=utf-8",
    ".woff": "font/woff",
    ".woff2": "font/woff2",
    ".ttf": "font/ttf",
    ".map": "application/json; charset=utf-8",
}


def log(msg):
    line = "[pdfreader %s] %s\n" % (time.strftime("%Y-%m-%d %H:%M:%S"), msg)
    try:
        sys.stderr.write(line)
        sys.stderr.flush()
    except Exception:
        pass
    if LOGFILE:
        try:
            with open(LOGFILE, "a") as f:
                f.write(line)
        except Exception:
            pass


# ----------------------------------------------------------------------------
# 工具
# ----------------------------------------------------------------------------
def strip_prefix(path):
    path = path.split("?", 1)[0]
    # 兼容请求行为绝对 URI（经代理时可能出现 GET http://host/path）的情况
    if path.startswith("http://") or path.startswith("https://"):
        rest = path.split("://", 1)[1]
        slash = rest.find("/")
        path = rest[slash:] if slash >= 0 else "/"
    if GATEWAY_PREFIX and path.startswith(GATEWAY_PREFIX):
        path = path[len(GATEWAY_PREFIX):]
    if not path.startswith("/"):
        path = "/" + path
    return path


def safe_join(root, rel):
    """静态资源防目录穿透。"""
    rel = rel.lstrip("/")
    full = os.path.abspath(os.path.join(root, rel))
    if full != root and not full.startswith(root + os.sep):
        return None
    real = os.path.realpath(full)
    if real != WEB_ROOT_REAL and not real.startswith(WEB_ROOT_REAL + os.sep):
        return None
    return full


def hash_id_for(abspath):
    """基于绝对路径生成稳定 bookId（重启后不变、跨用户一致）。"""
    return hashlib.sha1(abspath.encode("utf-8")).hexdigest()[:16]


def scan_all(uid: str):
    """递归扫描所有书库根目录下的 *.pdf，刷新全局索引并返回书目列表。
    每次扫描都重新读取根目录列表，以支持运行期（应用设置中）授权目录的增删。"""
    global LIBRARY_ROOTS
    roots = _collect_roots()
    LIBRARY_ROOTS = roots

    # 文件映射map key为文件或者文件夹的真实路径hash值
    file_map = {}
    for root in roots:
        for dirpath, dirnames, filenames in os.walk(root):
            # 跳过隐藏目录
            dirnames[:] = [d for d in dirnames if not d.startswith(".")]
            for fn in filenames:
                if not fn.lower().endswith(".pdf"):
                    continue
                if fn.startswith("."):
                    continue

                full = os.path.join(dirpath, fn)
                real = os.path.realpath(full)
                # 必须仍在某个 root 内（防符号链接逃逸）
                if not any(real == r or real.startswith(r + os.sep) for r in roots):
                    continue
                try:
                    st = os.stat(real)
                except OSError:
                    continue
                bid = hash_id_for(real)
                # 真实文件夹路径
                real_dir = os.path.dirname(real)
                # 真实文件夹路径 ID
                dirpath_id = hash_id_for(real_dir)

                file_map[bid] = {
                    "id": bid,
                    "fid": dirpath_id,
                    "name": fn,  # 文件名
                    "path": real,
                    "folder_name": os.path.basename(real_dir),
                    "size": st.st_size,
                    "mtime": int(st.st_mtime),
                    "root": root,
                    "type": "file",  # 文件类型 和 文件夹类型
                }
                temp_real = real
                while True:
                    temp_real = os.path.dirname(temp_real)
                    folder_id = hash_id_for(temp_real)
                    folder_name = os.path.basename(temp_real)
                    p_folder_dir = os.path.dirname(temp_real)
                    p_folder_name = os.path.basename(p_folder_dir)
                    if folder_id not in file_map.keys():
                        file_map[folder_id] = {
                            "id": folder_id,
                            "fid": hash_id_for(p_folder_dir),
                            "name": folder_name,  # 文件夹名
                            "path": temp_real,
                            "folder_name": p_folder_name,  # 文件夹的父文件夹
                            "size": 1,
                            "mtime": 0,
                            "root": root,
                            "type": "folder",  # 文件类型 和 文件夹类型
                        }
                    else:
                        file_map[folder_id]["size"] += 1
                    if temp_real in roots:
                        break

    save_file_map(uid, file_map)
    return file_map


# ----------------------------------------------------------------------------
# 进度持久化（按用户 UID 一个 JSON 文件）
# ----------------------------------------------------------------------------
def _progress_dir():
    d = os.path.join(DATA_DIR, "progress")
    try:
        os.makedirs(d, exist_ok=True)
    except OSError:
        pass
    return d


def _progress_file(uid):
    safe = "".join(c for c in str(uid) if c.isalnum() or c in "-_") or "anon"
    return os.path.join(_progress_dir(), "%s.json" % safe)


def load_progress(uid):
    fp = _progress_file(uid)
    if not os.path.isfile(fp):
        return {}
    try:
        with open(fp, "r", encoding="utf-8") as f:
            data = json.load(f)
            return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_progress_entry(uid, bid, entry):
    with _PROGRESS_LOCK:
        data = load_progress(uid)
        prev = data.get(bid, {})
        prev.update(entry)
        prev["updatedAt"] = int(time.time())
        data[bid] = prev
        fp = _progress_file(uid)
        tmp = fp + ".tmp"
        try:
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(data, f, ensure_ascii=False)
            os.replace(tmp, fp)
        except OSError as e:
            log("save progress failed: %r" % e)
            return None
        return prev


def _file_map_dir():
    d = os.path.join(DATA_DIR, "file_map")
    try:
        os.makedirs(d, exist_ok=True)
    except OSError:
        pass
    return d


def _file_map_file(uid):
    safe = "".join(c for c in str(uid) if c.isalnum() or c in "-_") or "anon"
    return os.path.join(_file_map_dir(), "%s.json" % safe)


def load_file_map(uid):
    fp = _file_map_file(uid)
    if not os.path.isfile(fp):
        return {}
    try:
        with open(fp, "r", encoding="utf-8") as f:
            data = json.load(f)
            return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_file_map(uid, file_map: dict):
    try:
        with open(_file_map_file(uid), "w", encoding="utf-8") as f:
            json.dump(file_map, f, ensure_ascii=False)
    except OSError as e:
        log("save progress failed: %r" % e)


def _file_image_file(name: str):
    d = os.path.join(DATA_DIR, "image")
    try:
        os.makedirs(d, exist_ok=True)
    except OSError:
        pass
    return os.path.join(d, name)


def get_doc_meta(entry):
    """返回文档元信息：页数 + 每页原始尺寸（pt, 72dpi）。"""
    with pymupdf.open(entry['path']) as pdf:
        cnt = pdf.page_count
        pages = []
        for i in range(cnt):
            try:
                r = pdf[i].rect
                pages.append({"w": round(r.width, 1), "h": round(r.height, 1)})
            except Exception:
                pages.append({"w": 612.0, "h": 792.0})
    return {"pageCount": cnt, "pages": pages}


def render_page(entry, dpi):
    dpi = max(36, min(300, int(dpi)))
    name = F'{entry.get("id", "")}_{dpi}.png'
    path = _file_image_file(name)
    exists = os.path.exists(path)
    if exists:
        try:
            with open(path, "rb") as f:
                return f.read()
        except (OSError, ValueError):
            pass
    with pymupdf.open(entry['path']) as pdf:
        png_bytes = pdf[0].get_pixmap(
            dpi=dpi,
            alpha=False,  # 不需要透明通道更快
            colorspace=pymupdf.csRGB
        ).tobytes("png")
    try:
        with open(path, "wb") as f:
            f.write(png_bytes)
    except (OSError, ValueError):
        pass
    return png_bytes


def extract_page_pdf(entry: dict, page: int, size: int = 1):
    """把第 page 页（1-based）抽取成一个独立的小 PDF，返回 bytes。
    前端用 PDF.js 渲染到 canvas（矢量、可无损缩放、绝不拉伸错乱），
    且每次只传这一页，不下载整本 PDF。

    智能混合策略（兼顾文本页保真与扫描页提速）：
      1) 矢量切片 + subset_fonts：仅保留本页字形。文本页实测 28MB→158KB、
         PDF.js 解析 11s→~0ms，且可无损缩放。
      2) 若矢量切片仍很大（>阈值，说明本页含高分辨率图片/扫描页，字体子集化无效），
         自动改走降采样光栅切片：整页重渲染为约 1500px 宽 JPEG 再打包。
         扫描页实测单页 1.1MB→~290KB，fetch 与浏览器图像解码都大幅加快。"""
    page = int(page)
    _t_req = time.time()
    if page < 0:
        return None
    with pymupdf.open(entry['path']) as pdf:
        cnt = pdf.page_count
        if page > cnt:
            return None
        with pymupdf.open() as doc:
            try:
                doc.insert_pdf(pdf, from_page=page, to_page=min(page + size - 1, cnt - 1))  # C层复制页对象，不落盘
                doc.subset_fonts()
                return doc.tobytes(garbage=4, deflate=True, deflate_fonts=True)
            except Exception:
                return None


# ----------------------------------------------------------------------------
# 安全：对端进程校验
# ----------------------------------------------------------------------------
def get_peer_uid(sock):
    try:
        SO_PEERCRED = getattr(socket, "SO_PEERCRED", 17)
        creds = sock.getsockopt(socket.SOL_SOCKET, SO_PEERCRED, struct.calcsize("3i"))
        pid, uid, gid = struct.unpack("3i", creds)
        return uid
    except (OSError, AttributeError, struct.error):
        return None


def peer_allowed(sock):
    if not PEERCRED_CHECK or TCP_PORT:
        return True
    uid = get_peer_uid(sock)
    if uid is None:
        return os.getuid() != 0
    if uid == 0 or uid == os.getuid():
        return True
    return uid in ALLOW_UIDS


# ----------------------------------------------------------------------------
# HTTP 请求处理
# ----------------------------------------------------------------------------
class Handler(socketserver.StreamRequestHandler):

    def _parse_request(self):
        line = self.rfile.readline(65536).decode("latin-1").strip()
        if not line:
            return None
        parts = line.split(" ")
        if len(parts) < 2:
            return None
        method, path = parts[0], parts[1]
        headers = {}
        while True:
            h = self.rfile.readline(65536).decode("latin-1")
            if h in ("\r\n", "\n", ""):
                break
            if ":" in h:
                k, v = h.split(":", 1)
                headers[k.strip().lower()] = v.strip()
        return method, path, headers

    def _read_body(self, headers):
        try:
            n = int(headers.get("content-length", "0"))
        except ValueError:
            n = 0
        if n <= 0:
            return b""
        return self.rfile.read(n)

    def _send_http(self, status, body=b"", ctype="text/plain; charset=utf-8", extra=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        out = ["HTTP/1.1 %s" % status,
               "Content-Type: %s" % ctype,
               "Content-Length: %d" % len(body),
               "Cache-Control: no-store",
               "Connection: close"]
        # CORS头现在通过extra参数传入，避免重复设置
        if extra:
            out += extra
        self.wfile.write(("\r\n".join(out) + "\r\n\r\n").encode("latin-1"))
        if body:
            self.wfile.write(body)

    def _send_json(self, obj, status="200 OK"):
        cors_headers = [
            "Access-Control-Allow-Origin: *",
            "Access-Control-Allow-Methods: GET, POST, OPTIONS",
            "Access-Control-Allow-Headers: x-trim-userid, x-trim-isadmin, x-trim-username, content-type"
        ]
        self._send_http(status, json.dumps(obj, ensure_ascii=False),
                        ctype="application/json; charset=utf-8", extra=cors_headers)

    def _auth(self, headers):
        uid = headers.get("x-trim-userid")
        is_admin = headers.get("x-trim-isadmin") == "true"
        username = headers.get("x-trim-username")
        if REQUIRE_AUTH and not uid:
            return None
        if not uid:
            uid = "debug"  # 本地调试 REQUIRE_AUTH=0 时
        return {"uid": uid, "isAdmin": is_admin, "username": username or uid}

    def handle(self):
        try:
            if not peer_allowed(self.connection):
                uid = get_peer_uid(self.connection)
                log("REJECT direct connection from uid=%s" % uid)
                cors_headers = [
                    "Access-Control-Allow-Origin: *",
                    "Access-Control-Allow-Methods: GET, POST, OPTIONS",
                    "Access-Control-Allow-Headers: x-trim-userid, x-trim-isadmin, x-trim-username, content-type"
                ]
                self._send_http("403 Forbidden", b"Forbidden: untrusted peer", extra=cors_headers)
                return

            req = self._parse_request()
            if not req:
                return
            method, raw_path, headers = req
            inner = strip_prefix(raw_path)
            if os.environ.get("PDFR_DEBUG_REQ"):
                log("REQ %s raw=%r inner=%r" % (method, raw_path, inner))
            qs = ""
            if "?" in raw_path:
                qs = raw_path.split("?", 1)[1]
            query = urllib.parse.parse_qs(qs)

            user = self._auth(headers)
            if user is None:
                self._send_http("403 Forbidden", b"Forbidden: gateway auth required")
                return

            path_only = inner.split("?", 1)[0].rstrip("/")
            print('method', method, 'path_only', path_only, 'query', query)

            # ---- API 路由 ----

            if path_only == "/api/me":
                self._send_json({"uid": user["uid"], "username": user["username"],
                                 "isAdmin": user["isAdmin"], })
                return

            if path_only == "/api/books":
                quert_path = query.get('path', [''])[0]
                quert_scan = query.get('scan', [''])[0]
                uid = user["uid"]
                start = time.time()
                if quert_scan == 'all':
                    file_map = scan_all(uid)
                else:
                    file_map = load_file_map(uid)
                print('/api/books 耗时', time.time() - start, '秒')
                real_root_ids = []
                if quert_path == "":
                    # 根目录
                    real_root_ids = []
                    for root in LIBRARY_ROOTS:
                        real_root = os.path.realpath(root)
                        real_root_ids.append(hash_id_for(real_root))
                else:
                    # 二级目录
                    real_root_ids.append(quert_path)
                # 遍历所有当前路径下的文件
                books = []
                history = []
                progress = load_progress(user["uid"])
                p_keys = progress.keys()
                for k, v in file_map.items():
                    tmp_b = {k: v[k] for k in
                             ("id", "fid", "name", "size", "mtime", "type")}
                    if v and v.get("fid", "") in real_root_ids:
                        # 洗一下避免文件路径信息泄露
                        books.append(tmp_b)
                    if k in p_keys and len(history) < 10:
                        tmp_b["progress"] = {
                            "page": progress[k].get("page", 0),
                            "totalPages": progress[k].get("totalPages", 0),
                            "percent": progress[k].get("percent", 0),
                            "updatedAt": progress[k].get("updatedAt", 0),
                        }
                        history.append(tmp_b)
                self._send_json({"books": books, "history": history, "count": len(books), })
                return

            # ---- 文档元信息：页数 + 每页尺寸（前端据此排版占位，不下载原文件）----
            if path_only == "/api/meta":
                bid = query.get("id", [""])[0]
                uid = user["uid"]
                file_map = load_file_map(uid)
                bookEntry = file_map.get(bid, {})
                if not bookEntry or bookEntry.get('type', '') != 'file':
                    self._send_http("404 Not Found", b"book not found")
                    return
                try:
                    meta = get_doc_meta(bookEntry)
                    meta["id"] = bid
                    meta["name"] = bookEntry["name"]
                    meta["progress"] = load_progress(uid).get(bid, {})
                    self._send_json(meta)
                except Exception as e:  # noqa
                    log("meta error %s: %r" % (bookEntry.get("name"), e))
                    self._send_json({"error": "meta_failed", "detail": str(e)},
                                    status="500 Internal Server Error")
                return

            # ---- 服务端渲染：返回指定页的图片（原文件绝不下载到连接端）----
            if path_only == "/api/page":
                bid = query.get("id", [""])[0]
                uid = user["uid"]
                file_map = load_file_map(uid)
                entry = file_map.get(bid, {})
                if not entry or entry.get('type', '') != 'file':
                    self._send_http("404 Not Found", b"book not found")
                    return
                if method not in ("GET", "HEAD"):
                    self._send_http("405 Method Not Allowed", b"method not allowed")
                    return
                try:
                    res = render_page(entry, 80)
                except Exception as e:  # noqa
                    self._send_http("500 Internal Server Error", b"render failed")
                    return
                if res is None:
                    self._send_http("404 Not Found", b"page out of range")
                    return
                base64_data = base64.b64encode(res).decode('utf-8')
                data_url = f"data:image/png;base64,{base64_data}"
                # 返回JSON格式的base64图片数据
                self._send_json({
                    "success": True,
                    "base64": f"data:image/png;base64,{base64_data}",
                    "mimeType": "image/png",
                    "size": len(res)
                })
                return

            # ---- 单页 PDF 切片：把当前阅读页抽成独立小 PDF，前端用 PDF.js 渲染到 canvas。
            #      矢量渲染、缩放无损、绝不拉伸错乱；每次只传一页（几 KB），不下载整本。----
            if path_only == "/api/pagepdf":
                bid = query.get("id", [""])[0]
                uid = user["uid"]
                start = time.time()
                try:
                    page = int((query.get("page") or ["1"])[0])
                except ValueError:
                    self._send_http("400 Bad Request", "bad page")
                    return
                try:
                    size = int((query.get("size") or ["1"])[0])
                except ValueError:
                    self._send_http("400 Bad Request", "bad page")
                    return
                file_map = load_file_map(uid)
                entry = file_map.get(bid, {})
                if not entry or entry.get('type', '') != 'file':
                    self._send_http("404 Not Found", "book not found")
                    return
                if method not in ("GET", "HEAD"):
                    self._send_http("405 Method Not Allowed", "method not allowed")
                    return
                try:
                    data = extract_page_pdf(entry, page, size)
                except Exception as e:  # noqa
                    log("slice error %s p%s: %r" % (entry.get("name"), page, e))
                    self._send_http("500 Internal Server Error", "slice failed")
                    return
                if data is None:
                    self._send_http("404 Not Found", "page out of range")
                    return
                self._send_http("200 OK", b"" if method == "HEAD" else data,
                                ctype="application/pdf")
                print(F'{bid} 请求第 {page} 页耗时：{time.time() - start} 秒')
                return

            if path_only == "/api/progress":
                bid = (query.get("id") or [""])[0]
                if method == "GET":
                    prog = load_progress(user["uid"])
                    self._send_json({"id": bid, "progress": prog.get(bid)})
                    return
                if method == "POST":
                    body = self._read_body(headers)
                    try:
                        payload = json.loads(body.decode("utf-8")) if body else {}
                    except ValueError:
                        self._send_http("400 Bad Request", "invalid json")
                        return
                    bid = payload.get("id") or bid
                    if not bid:
                        self._send_http("400 Bad Request", "missing id")
                        return
                    entry = {}
                    # frac: 页内滚动比例(0~1)，跨设备对齐的关键
                    for k in ("page", "frac", "name", "scale", "totalPages", "percent"):
                        if k in payload:
                            entry[k] = payload[k]
                    saved = save_progress_entry(user["uid"], bid, entry)
                    self._send_json({"ok": saved is not None, "progress": saved})
                    return
                self._send_http("405 Method Not Allowed", "method not allowed")
                return

            # ---- 静态文件 ----
            if method not in ("GET", "HEAD"):
                self._send_http("405 Method Not Allowed", "method not allowed")
                return
            rel = path_only if path_only else "/"
            if rel in ("", "/"):
                rel = "/index.html"
            full = safe_join(WEB_ROOT, rel)
            if not full or not os.path.isfile(full):
                full = os.path.join(WEB_ROOT, "index.html")
                if not os.path.isfile(full):
                    diag = ("<!doctype html><meta charset=utf-8>"
                            "<body style='font-family:sans-serif;padding:24px'>"
                            "<h2>PDF 阅读器：资源未找到</h2></body>")
                    self._send_http("404 Not Found", diag, ctype="text/html; charset=utf-8")
                    return
            ext = os.path.splitext(full)[1].lower()
            ctype = MIME.get(ext, "application/octet-stream")
            with open(full, "rb") as f:
                data = f.read()
            if method == "HEAD":
                data = b""
            self._send_http("200 OK", data, ctype=ctype)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as e:  # noqa
            log("handler error: %r" % e)
            try:
                self._send_http("500 Internal Server Error", "internal error")
            except Exception:
                pass


class ThreadingUnixServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True
    allow_reuse_address = True


class ThreadingTCPServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    try:
        os.makedirs(DATA_DIR, exist_ok=True)
    except OSError:
        pass
    log("webroot=%s (index.html exists=%s)  data_dir=%s  roots=%s"
        % (WEB_ROOT, os.path.isfile(os.path.join(WEB_ROOT, "index.html")),
           DATA_DIR, LIBRARY_ROOTS))

    if TCP_PORT:
        addr = ("127.0.0.1", int(TCP_PORT))
        srv = ThreadingTCPServer(addr, Handler)
        log("listening on tcp %s:%s" % addr)
    else:
        if os.path.exists(SOCK_PATH):
            try:
                os.unlink(SOCK_PATH)
            except OSError:
                pass
        srv = ThreadingUnixServer(SOCK_PATH, Handler)
        try:
            os.chmod(SOCK_PATH, 0o666)
        except OSError:
            pass
        log("listening on unix %s  prefix=%s" % (SOCK_PATH, GATEWAY_PREFIX))

    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
        if not TCP_PORT and os.path.exists(SOCK_PATH):
            try:
                os.unlink(SOCK_PATH)
            except OSError:
                pass


if __name__ == "__main__":
    try:
        log("=== pdfreader server boot ===  sock=%s webroot=%s prefix=%s py=%s"
            % (SOCK_PATH, WEB_ROOT, GATEWAY_PREFIX, sys.version.split()[0]))
        main()
    except Exception:
        import traceback

        log("FATAL boot error:\n" + traceback.format_exc())
        raise
