#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PDF 阅读器 — Flask 版本 (fnOS 统一网关版)

设计要点：
  * Flask 应用，同时托管前端静态文件 + API 接口
  * 开发和生产环境使用相同路径前缀 /app/fnnas-pdfreader/
  * 书库扫描：递归扫描 data-share(PDFLibrary) 与用户授权目录下的 *.pdf
  * **服务端渲染（核心）**：用 PyMuPDF(fitz) 在 NAS 上把指定页渲染成 PDF 切片，
    只把渲染好的切片返回给连接端。PDF 原文件**绝不下载到连接端**。
  * 阅读进度 / 书签：按飞牛账号(UID)持久化到 NAS 的 var 数据目录，多端共享续读
  * 读取统一网关注入的 X-Trim-* 头做鉴权

启动方式：
  开发环境：python pdfserver.py --port 5173
  生产环境：由 fnOS 统一网关通过 Unix Socket 转发
"""
import argparse
import base64
import ctypes
import ctypes.util
import functools
import gc
import hashlib
import json
import os
import sys
import threading
import time
from PIL import Image
import io

from flask import Flask, request, jsonify, send_from_directory, send_file, abort
import pymupdf

# ----------------------------------------------------------------------------
# 配置（由环境变量注入，含本地调试默认值）
# ----------------------------------------------------------------------------
APPNAME = os.environ.get("PDFR_APPNAME", "pdfreader")
GATEWAY_PREFIX = os.environ.get("PDFR_GATEWAY_PREFIX", "/app/fnnas-pdfreader").rstrip("/")
SOCK_PATH = os.environ.get("PDFR_SOCK", os.path.join(os.getcwd(), "app.sock"))
DATA_DIR = os.environ.get("PDFR_DATA_DIR", os.path.join(os.getcwd(), "data"))
REQUIRE_AUTH = os.environ.get("PDFR_REQUIRE_AUTH", "0") == "1"  # 本地调试默认关闭
PEERCRED_CHECK = os.environ.get("PDFR_PEERCRED_CHECK", "0") == "1"
LOGFILE = os.environ.get("PDFR_LOGFILE", "")

# 解析命令行参数
parser = argparse.ArgumentParser(description="PDF Reader Flask Server")
parser.add_argument("--port", "-p", type=int, default=0, help="TCP 端口（默认 0，由 fnOS 统一网关通过 Unix Socket 转发）")
parser.add_argument("--host", "-H", type=str, default="0.0.0.0", help="TCP 监听地址（仅 --port > 0 时生效）")
parser.add_argument("--debug", "-d", action="store_true", help="开启调试模式")
args, unknown = parser.parse_known_args()


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
    """查找前端构建产物目录"""
    here = os.path.dirname(os.path.abspath(__file__))
    candidates = [
        os.environ.get("PDFR_WEBROOT"),
        os.path.join(here, "..", "vueapp", "dist"),
        os.path.join(here, "..", "ui"),
        os.path.join(here, "ui"),
    ]
    for c in candidates:
        if c and os.path.isfile(os.path.join(c, "index.html")):
            return os.path.abspath(c)
    # 没找到构建产物，返回 vueapp/dist
    return os.path.abspath(os.path.join(here, "..", "vueapp", "dist"))


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
# C 层内存回收：PDF 关闭后主动把 MuPDF 缓存 + glibc 空闲堆还给 OS
# ----------------------------------------------------------------------------
@functools.lru_cache(maxsize=1)
def _get_libc():
    """返回带 malloc_trim 的 libc 句柄；不可用时返回 None。

    仅 Linux/glibc 存在 malloc_trim；macOS/musl 上不存在。用 lru_cache 保证
    探测只跑一次（首次调用时），避免每次回收都 find_library，且无需模块级全局变量。
    """
    try:
        libc_name = ctypes.util.find_library("c")
        if libc_name:
            c = ctypes.CDLL(libc_name)
            if hasattr(c, "malloc_trim"):
                return c
    except Exception:
        pass
    return None


def reclaim_c_memory():
    """PDF 关闭后调用：清 MuPDF C 层 store 缓存 + gc + 催 glibc 归还空闲堆给内核。

    这三层是 Python GC 管不到的内存：
      1) pymupdf.TOOLS.store_shrink(100) —— 清空 MuPDF 全局 store（字体/图形缓存）
      2) gc.collect() —— 回收可能存在的 Python 循环引用
      3) libc.malloc_trim(0) —— 让 glibc 把空闲的物理页真正还给操作系统（Linux 生效，
         macOS/musl 上 _get_libc() 为 None，自动跳过）
    全部包在 try 里，任何一步失败都不影响主流程。
    """
    try:
        pymupdf.TOOLS.store_shrink(100)
    except Exception:
        pass
    try:
        gc.collect()
    except Exception:
        pass
    libc = _get_libc()
    if libc is not None:
        try:
            libc.malloc_trim(0)
        except Exception:
            pass


# ----------------------------------------------------------------------------
# Flask 应用
# ----------------------------------------------------------------------------
app = Flask(__name__, static_folder=WEB_ROOT, static_url_path=f"{GATEWAY_PREFIX}")


# ----------------------------------------------------------------------------
# 工具函数
# ----------------------------------------------------------------------------
def strip_prefix(path):
    """剥离网关前缀"""
    path = path.split("?", 1)[0]
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
    """静态资源防目录穿透"""
    rel = rel.lstrip("/")
    full = os.path.abspath(os.path.join(root, rel))
    if full != root and not full.startswith(root + os.sep):
        return None
    real = os.path.realpath(full)
    if real != WEB_ROOT_REAL and not real.startswith(WEB_ROOT_REAL + os.sep):
        return None
    return full


def hash_id_for(abspath):
    """基于绝对路径生成稳定 bookId"""
    return hashlib.sha1(abspath.encode("utf-8")).hexdigest()[:16]


def get_user_from_request():
    """从请求头获取用户信息"""
    headers = request.headers
    uid = headers.get("X-Trim-Userid")
    is_admin = headers.get("X-Trim-Isadmin") == "true"
    username = headers.get("X-Trim-Username")
    if REQUIRE_AUTH and not uid:
        return None
    if not uid:
        uid = "debug"
    return {"uid": uid, "isAdmin": is_admin, "username": username or uid}


# ----------------------------------------------------------------------------
# 书库扫描
# ----------------------------------------------------------------------------
def scan_all(uid: str):
    """递归扫描所有书库根目录下的 *.pdf"""
    global LIBRARY_ROOTS
    roots = _collect_roots()
    LIBRARY_ROOTS = roots

    file_map = {}
    for root in roots:
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames[:] = [d for d in dirnames if not d.startswith(".")]
            for fn in filenames:
                if not fn.lower().endswith(".pdf"):
                    continue
                if fn.startswith("."):
                    continue

                full = os.path.join(dirpath, fn)
                real = os.path.realpath(full)
                if not any(real == r or real.startswith(r + os.sep) for r in roots):
                    continue
                try:
                    st = os.stat(real)
                except OSError:
                    continue
                bid = hash_id_for(real)
                real_dir = os.path.dirname(real)
                dirpath_id = hash_id_for(real_dir)

                file_map[bid] = {
                    "id": bid,
                    "fid": dirpath_id,
                    "name": fn,
                    "path": real,
                    "folder_name": os.path.basename(real_dir),
                    "size": st.st_size,
                    "mtime": int(st.st_mtime),
                    "root": root,
                    "type": "file",
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
                            "name": folder_name,
                            "path": temp_real,
                            "folder_name": p_folder_name,
                            "size": 1,
                            "mtime": 0,
                            "root": root,
                            "type": "folder",
                        }
                    else:
                        file_map[folder_id]["size"] += 1
                    if temp_real in roots:
                        break

    save_file_map(uid, file_map)
    return file_map


# ----------------------------------------------------------------------------
# 进度持久化
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


# ----------------------------------------------------------------------------
# PDF 处理
# ----------------------------------------------------------------------------
def get_doc_meta(entry):
    """返回文档元信息：页数 + 每页原始尺寸"""
    with pymupdf.open(entry['path']) as pdf:
        cnt = pdf.page_count
        pages = []
        for i in range(cnt):
            try:
                r = pdf[i].rect
                pages.append({"w": round(r.width, 1), "h": round(r.height, 1)})
            except Exception:
                pages.append({"w": 612.0, "h": 792.0})
    # PDF 已关闭，回收 C 层内存
    reclaim_c_memory()
    return {"pageCount": cnt, "pages": pages}


def extract_page_pdf(entry: dict, page: int, size: int = 1):
    """把第 page 页（1-based）抽取成一个独立的小 PDF，返回 bytes。"""
    page = int(page)
    _t_req = time.time()
    if page < 0:
        return None
    try:
        with pymupdf.open(entry['path']) as pdf:
            cnt = pdf.page_count
            if page > cnt:
                return None
            with pymupdf.open() as doc:
                try:
                    doc.insert_pdf(pdf, from_page=page, to_page=min(page + size - 1, cnt - 1))
                    doc.subset_fonts()
                    return doc.tobytes(garbage=4, deflate=True, deflate_fonts=True)
                except Exception:
                    return None
    finally:
        # 两个 PDF 均已关闭，回收 C 层内存
        reclaim_c_memory()


# ----------------------------------------------------------------------------
# API 路由
# ----------------------------------------------------------------------------
@app.route(f"{GATEWAY_PREFIX}/api/me", methods=["GET"])
def api_me():
    """获取当前用户信息"""
    user = get_user_from_request()
    if user is None:
        abort(403, "Forbidden: gateway auth required")
    return jsonify({
        "uid": user["uid"],
        "username": user["username"],
        "isAdmin": user["isAdmin"],
    })


@app.route(f"{GATEWAY_PREFIX}/api/books", methods=["GET"])
def api_books():
    """获取书库列表"""
    user = get_user_from_request()
    if user is None:
        abort(403, "Forbidden: gateway auth required")

    query_path = request.args.get('path', '')
    query_scan = request.args.get('scan', '')
    uid = user["uid"]
    username = user["username"]

    start = time.time()
    file_map = load_file_map(uid)
    if len(file_map) == 0 or query_scan == 'all':
        file_map = scan_all(uid)
    log('/api/books 耗时 %s 秒' % (time.time() - start))

    real_root_ids = []
    if query_path == "":
        for root in LIBRARY_ROOTS:
            real_root = os.path.realpath(root)
            real_root_ids.append(hash_id_for(real_root))
    else:
        real_root_ids.append(query_path)

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
    history.sort(key=lambda x: x["progress"].get("updatedAt", 0), reverse=True)
    return jsonify({"books": books, "history": history, "count": len(books), "username": username})


@app.route(f"{GATEWAY_PREFIX}/api/meta", methods=["GET"])
def api_meta():
    """获取文档元信息"""
    user = get_user_from_request()
    if user is None:
        abort(403, "Forbidden: gateway auth required")

    bid = request.args.get('id', '')
    uid = user["uid"]
    file_map = load_file_map(uid)
    book_entry = file_map.get(bid, {})

    if not book_entry or book_entry.get('type', '') != 'file':
        abort(404, "book not found")

    try:
        meta = get_doc_meta(book_entry)
        meta["id"] = bid
        meta["name"] = book_entry["name"]
        meta["progress"] = load_progress(uid).get(bid, {})
        return jsonify(meta)
    except Exception as e:
        log("meta error %s: %r" % (book_entry.get("name"), e))
        return jsonify({"error": "meta_failed", "detail": str(e)}), 500


@app.route(f"{GATEWAY_PREFIX}/api/pagepdf", methods=["GET"])
def api_pagepdf():
    """获取单页 PDF 切片"""
    user = get_user_from_request()
    if user is None:
        abort(403, "Forbidden: gateway auth required")

    bid = request.args.get('id', '')
    uid = user["uid"]
    start = time.time()

    try:
        page = int(request.args.get('page', 1))
    except ValueError:
        abort(400, "bad page")

    try:
        size = int(request.args.get('size', 1))
    except ValueError:
        abort(400, "bad size")

    file_map = load_file_map(uid)
    entry = file_map.get(bid, {})

    if not entry or entry.get('type', '') != 'file':
        abort(404, "book not found")

    try:
        data = extract_page_pdf(entry, page, size)
    except Exception as e:
        log("slice error %s p%s: %r" % (entry.get("name"), page, e))
        abort(500, "slice failed")

    if data is None:
        abort(404, "page out of range")

    log(f"{bid} 请求第 {page} 页耗时：{time.time() - start} 秒")
    return send_file(
        io.BytesIO(data),
        mimetype="application/pdf",
        as_attachment=False,
    )


def render_page(entry, page: int, dpi: int):
    """渲染PDF页面为图片，并进行纯Python优化"""
    dpi = max(36, min(500, int(dpi)))
    name = f'{entry.get("id", "")}_{page}_{dpi}.png'
    path = os.path.join(DATA_DIR, "image", name)

    # 检查缓存
    exists = os.path.exists(path)
    if exists:
        try:
            with open(path, "rb") as f:
                cached_data = f.read()
                # 如果缓存文件是优化后的，直接返回
                if len(cached_data) > 0:
                    return cached_data
        except (OSError, ValueError):
            pass

    try:
        with pymupdf.open(entry['path']) as pdf:
            cnt = pdf.page_count
            if page > cnt:
                return None
            png_bytes = pdf[page].get_pixmap(
                dpi=dpi,
                alpha=False,
                colorspace=pymupdf.csRGB,
            ).tobytes("png")

        # 应用纯Python优化
        try:
            try:
                # 使用智能优化
                img = Image.open(io.BytesIO(png_bytes))
                # --- PNG 量化压缩 ---
                if img.mode in ("RGBA", "LA"):
                    alpha = img.getchannel("A")
                    p_img = img.convert("RGB").convert("P", palette=Image.ADAPTIVE, colors=192)
                    p_img.putalpha(alpha)
                else:
                    p_img = img.convert("P", palette=Image.ADAPTIVE, colors=192)

                png_buf = io.BytesIO()
                p_img.save(png_buf, "PNG", optimize=True)
                png_bytes = png_buf.getvalue()
            except ImportError:
                # 如果优化模块不可用，使用原始PNG
                log(f"页面{page}: 优化模块不可用，使用原始PNG")
        except Exception as e:
            # 任何异常都回退到原始PNG
            log(f"页面{page}优化异常: {e}")

        # 缓存优化后的图片
        try:
            os.makedirs(os.path.join(DATA_DIR, "image"), exist_ok=True)
            with open(path, "wb") as f:
                f.write(png_bytes)
        except (OSError, ValueError) as e:
            log(f"缓存写入失败: {e}")

        return png_bytes
    finally:
        # PDF 已关闭、pixmap/Pillow 中间对象已出作用域，回收 C 层内存
        reclaim_c_memory()


@app.route(f"{GATEWAY_PREFIX}/api/page", methods=["GET"])
def api_page():
    """获取指定页的 PNG 图片（返回 base64 JSON）"""
    user = get_user_from_request()
    if user is None:
        abort(403, "Forbidden: gateway auth required")

    bid = request.args.get('id', '')
    uid = user["uid"]
    file_map = load_file_map(uid)
    entry = file_map.get(bid, {})

    if not entry or entry.get('type', '') != 'file':
        abort(404, "book not found")

    if request.method not in ("GET", "HEAD"):
        abort(405, "method not allowed")

    # 支持可选的页面和DPI参数
    try:
        page = int(request.args.get('page', 0))
    except ValueError:
        page = 0

    try:
        dpi = int(request.args.get('dpi', 80))
    except ValueError:
        dpi = 80
    res = None
    try:
        res = render_page(entry, page, dpi)
    except Exception:
        abort(500, "render failed")

    if res is None:
        abort(404, "page out of range")

    # 直接返回二进制图片数据
    return send_file(
        io.BytesIO(res),
        mimetype="image/png",
        as_attachment=False,
        max_age=86400 * 3  # 缓存 24 * 3 小时
    )


@app.route(f"{GATEWAY_PREFIX}/api/progress", methods=["GET", "POST"])
def api_progress():
    """获取/保存阅读进度"""
    user = get_user_from_request()
    if user is None:
        abort(403, "Forbidden: gateway auth required")

    bid = request.args.get('id', '')

    if request.method == "GET":
        prog = load_progress(user["uid"])
        return jsonify({"id": bid, "progress": prog.get(bid)})

    if request.method == "POST":
        payload = request.get_json(silent=True) or {}
        bid = payload.get("id") or bid
        if not bid:
            abort(400, "missing id")

        entry = {}
        for k in ("page", "frac", "name", "scale", "totalPages", "percent"):
            if k in payload:
                entry[k] = payload[k]

        saved = save_progress_entry(user["uid"], bid, entry)
        return jsonify({"ok": saved is not None, "progress": saved})

    abort(405, "method not allowed")


# ----------------------------------------------------------------------------
# 静态文件路由
# ----------------------------------------------------------------------------
@app.route(f"{GATEWAY_PREFIX}/")
def serve_index():
    """返回 index.html"""
    if os.path.isfile(os.path.join(WEB_ROOT, "index.html")):
        return send_from_directory(WEB_ROOT, "index.html")
    return "PDF 阅读器：前端未构建", 404


def main():
    try:
        os.makedirs(DATA_DIR, exist_ok=True)
    except OSError:
        pass

    log(f"webroot={WEB_ROOT} (index.html exists={os.path.isfile(os.path.join(WEB_ROOT, 'index.html'))})")
    log(f"data_dir={DATA_DIR}")
    log(f"roots={LIBRARY_ROOTS}")
    log(f"prefix={GATEWAY_PREFIX}")
    log(f"sock_path={SOCK_PATH}")

    # 仅保留本地TCP调试模式
    if args.port > 0:
        log(f"Using TCP debug mode on {args.host}:{args.port}")
        app.run(host=args.host, port=args.port, debug=args.debug, threaded=True)
    else:
        from waitress import serve
        import stat
        sock_file = SOCK_PATH
        # 清理残留socket文件
        if os.path.exists(sock_file):
            os.unlink(sock_file)
        log(f"Waitress WSGI listening unix socket: {sock_file}")
        # 启动WSGI服务，原生支持Unix Socket，自带超时防504
        serve(
            app,
            unix_socket=sock_file,
            threads=16,
            connection_limit=128,
            channel_timeout=180,  # 单次请求最大180秒，解决PDF扫描渲染超时
            cleanup_interval=10
        )
        # 设置socket权限0666，nginx网关可读写
        os.chmod(
            sock_file,
            stat.S_IRUSR | stat.S_IWUSR |
            stat.S_IRGRP | stat.S_IWGRP |
            stat.S_IROTH | stat.S_IWOTH
        )


if __name__ == "__main__":
    try:
        mode = "TCP Debug" if args.port > 0 else "Production(use gunicorn)"
        log(f"=== pdfreader flask server boot === mode={mode} prefix={GATEWAY_PREFIX} py={sys.version.split()[0]}")
        main()
    except Exception:
        import traceback

        log("FATAL boot error:\n" + traceback.format_exc())
        raise
