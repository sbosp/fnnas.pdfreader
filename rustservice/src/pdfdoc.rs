// PDF 渲染与元信息（pdf_oxide 纯 Rust 实现）
//
// 设计要点：
//   - 渲染：pdf_oxide::rendering（tiny-skia 光栅化）把页面渲染成 JPEG。
//   - 文档句柄缓存：同一本书只 open 一次并复用（避免每页重复解析整本，CPU 防爆）；
//     空闲超时后台清理，释放内存。
//   - 双层缓存：图片 + meta 都缓存到 {书库根}/.pdfreader-cache/，与书籍同盘、隐藏。
use crate::config::cfg;
use crate::logf;
use crate::pathsafe::root_of;
use pdf_oxide::document::PdfDocument;
use pdf_oxide::object::Object;
use pdf_oxide::rendering::{render_page, render_page_region, ImageFormat, RenderOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// RENDER_DPI 正文渲染分辨率（300 清晰度高，手机/电脑都不错）
pub const RENDER_DPI: u32 = 300;
/// JPEG 质量
const JPEG_QUALITY: u8 = 82;

// ----------------------------------------------------------------------------
// 缓存目录：{书库根}/.pdfreader-cache/{相对路径的安全名}/...
// 与书籍同盘、隐藏目录不干扰浏览；无书库根时回退 DATA_DIR/cache。
// ----------------------------------------------------------------------------
const CACHE_DIR_NAME: &str = ".pdfreader-cache";

/// cache_key 用「相对书库根的路径」生成稳定且可读的缓存目录名。
/// 路径分隔符与特殊字符替换为 _，避免嵌套过深与非法字符。
fn cache_key(pdf_path: &Path) -> String {
    let rel = root_of(pdf_path)
        .and_then(|r| pdf_path.strip_prefix(&r).ok().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from(crate::pathsafe::file_name_of(pdf_path)));
    let s: String = rel
        .to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect();
    // 附加 mtime+size 摘要，PDF 被替换后缓存自动失效
    let sig = std::fs::metadata(pdf_path)
        .map(|m| {
            let mt = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{}-{}", mt, m.len())
        })
        .unwrap_or_else(|_| "0-0".to_string());
    format!("{s}.{sig}")
}

/// cache_root 缓存根目录（书库根下的隐藏目录）
fn cache_root(pdf_path: &Path) -> PathBuf {
    match root_of(pdf_path) {
        Some(r) => r.join(CACHE_DIR_NAME),
        None => cfg().data_dir.join("cache"),
    }
}

/// 图片缓存路径：{cacheRoot}/{key}/{dpi}/page-N.jpg
fn image_cache_path(pdf_path: &Path, page1: usize, dpi: u32) -> PathBuf {
    cache_root(pdf_path)
        .join(cache_key(pdf_path))
        .join(dpi.to_string())
        .join(format!("page-{page1}.jpg"))
}

/// meta 缓存路径：{cacheRoot}/{key}/meta.json
fn meta_cache_path(pdf_path: &Path) -> PathBuf {
    cache_root(pdf_path).join(cache_key(pdf_path)).join("meta.json")
}

// ----------------------------------------------------------------------------
// 文档元信息
// ----------------------------------------------------------------------------
#[derive(Clone)]
pub struct DocMeta {
    pub page_count: usize,
    pub width: f32,
    pub height: f32,
}

impl DocMeta {
    fn to_json(&self) -> String {
        format!(
            r#"{{"pageCount":{},"width":{:.1},"height":{:.1}}}"#,
            self.page_count, self.width, self.height
        )
    }

    fn from_json(s: &str) -> Option<DocMeta> {
        // 极简解析（自产自销的固定结构，不引入额外依赖）
        let get = |k: &str| -> Option<f64> {
            let pat = format!("\"{k}\":");
            let i = s.find(&pat)? + pat.len();
            let rest = &s[i..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .unwrap_or(rest.len());
            rest[..end].parse::<f64>().ok()
        };
        let pc = get("pageCount")? as usize;
        if pc == 0 {
            return None;
        }
        Some(DocMeta {
            page_count: pc,
            width: get("width").unwrap_or(595.0) as f32,
            height: get("height").unwrap_or(842.0) as f32,
        })
    }
}

// ----------------------------------------------------------------------------
// 文档句柄缓存（同书复用，空闲超时清理）
//
// 直接用 PdfDocument（只读）而非 DocumentEditor：后者的 open() 内部除了
// PdfDocument::open 还会 find_max_object_id（遍历全部对象）+ 建 page_order/
// ResourceManager/多个 HashMap —— 这些是为「编辑」准备的，对只读渲染纯属开销。
// render_page / page_count / media_box 都只要 &PdfDocument。
// ----------------------------------------------------------------------------
struct CachedDoc {
    path: PathBuf,
    doc: Arc<PdfDocument>,
    last_used: Instant,
}

fn doc_cache() -> &'static Mutex<Option<CachedDoc>> {
    static C: OnceLock<Mutex<Option<CachedDoc>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// doc 空闲超时（秒），环境变量 PDFR_DOC_IDLE_SECS 可配，默认 120s
fn doc_idle_secs() -> u64 {
    std::env::var("PDFR_DOC_IDLE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(120)
}

/// start_doc_idle_reaper 后台清理空闲文档，释放内存（客户端离开后自动生效）。
/// 检查间隔取 idle/2（最小 2s），保证超时后能及时清理。
pub fn start_doc_idle_reaper() {
    std::thread::spawn(|| {
        let idle = doc_idle_secs();
        let interval = std::cmp::max(2, idle / 2);
        loop {
            std::thread::sleep(Duration::from_secs(interval));
            let mut guard = match doc_cache().lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            let expired = guard
                .as_ref()
                .map(|d| d.last_used.elapsed().as_secs() > idle)
                .unwrap_or(false);
            if expired {
                if let Some(d) = guard.take() {
                    logf!("doc 空闲超 {}s，自动清理释放内存: {}", idle, d.path.display());
                }
            }
        }
    });
}

/// with_doc 取得（或打开并缓存）该 PDF 的文档句柄，执行闭包。
///
/// 锁只保护「查表 / 换书 / 更新 last_used」，打开文件和渲染都在锁外。
/// PdfDocument 是 Send+Sync，热缓存命中后多线程可并行 render_page。
/// 同书复用避免重复解析整本；切换书时替换缓存（旧 Arc 在在途渲染结束后释放）。
fn with_doc<T>(
    pdf_path: &Path,
    f: impl FnOnce(&PdfDocument) -> Result<T, String>,
) -> Result<T, String> {
    {
        let mut guard = doc_cache()
            .lock()
            .map_err(|_| "doc cache poisoned".to_string())?;
        if let Some(d) = guard.as_mut() {
            if d.path == pdf_path {
                d.last_used = Instant::now();
                let doc = Arc::clone(&d.doc);
                drop(guard);
                return f(&doc);
            }
        }
    }

    let opened = Arc::new(PdfDocument::open(pdf_path).map_err(|e| format!("open: {e:?}"))?);
    {
        let mut guard = doc_cache()
            .lock()
            .map_err(|_| "doc cache poisoned".to_string())?;
        if let Some(d) = guard.as_mut() {
            if d.path == pdf_path {
                d.last_used = Instant::now();
                let doc = Arc::clone(&d.doc);
                drop(guard);
                return f(&doc);
            }
        }
        *guard = Some(CachedDoc {
            path: pdf_path.to_path_buf(),
            doc: Arc::clone(&opened),
            last_used: Instant::now(),
        });
    }
    f(&opened)
}

// ----------------------------------------------------------------------------
// 对外 API
// ----------------------------------------------------------------------------

/// get_meta 取文档元信息（页数 + 首页尺寸），带书库磁盘缓存。
/// 只取首页尺寸、所有页共用（前端 img onLoad 会按真实图片比例校正每页）。
pub fn get_meta(pdf_path: &Path) -> Result<DocMeta, String> {
    let mp = meta_cache_path(pdf_path);
    if let Ok(s) = std::fs::read_to_string(&mp) {
        if let Some(m) = DocMeta::from_json(&s) {
            return Ok(m);
        }
    }

    let meta = with_doc(pdf_path, |doc| {
        let pc = doc.page_count().map_err(|e| format!("page_count: {e:?}"))?;
        let (w, h) = page_size(doc, 0).unwrap_or((595.0, 842.0));
        Ok(DocMeta {
            page_count: pc,
            width: w,
            height: h,
        })
    })?;

    if let Some(dir) = mp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&mp, meta.to_json());
    Ok(meta)
}

/// page_box 取第 index 页的**可见区域**（CropBox，缺失时回退 MediaBox），返回 (x0,y0,w,h) pt。
///
/// 【为什么必须用 CropBox】ISO 32000-1 §14.11.2：查看器显示页面时应使用 CropBox 裁剪，
/// MediaBox 只是物理纸张尺寸。双页对开扫描的书很常见这种结构：
///   MediaBox = 1048x737（整张摊开的纸，两页并排）
///   CropBox  =  524x737（本页实际内容，左半或右半）
/// pdf_oxide 的 render_page 内部只用 MediaBox（rendering/mod.rs），
/// 于是把整张双页都画出来 → 用户看到「两页渲染到同一页」。
/// 这里自己读 CropBox 并用 render_page_region 裁出正确区域。
fn page_box(doc: &PdfDocument, index: usize) -> Option<(f32, f32, f32, f32)> {
    let media = doc.get_page_media_box(index).ok()?;
    let crop = crop_box_of(doc, index);

    // CropBox 需与 MediaBox 求交（规范要求），且必须是有效矩形
    let (mx0, my0, mx1, my1) = (
        media.0.min(media.2),
        media.1.min(media.3),
        media.0.max(media.2),
        media.1.max(media.3),
    );
    let (x0, y0, x1, y1) = match crop {
        Some((cx0, cy0, cx1, cy1)) => (
            cx0.min(cx1).max(mx0),
            cy0.min(cy1).max(my0),
            cx0.max(cx1).min(mx1),
            cy0.max(cy1).min(my1),
        ),
        None => (mx0, my0, mx1, my1),
    };
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 1.0 || h <= 1.0 {
        // CropBox 异常（空/退化）时回退整页，避免渲染出 0 尺寸
        return Some((mx0, my0, mx1 - mx0, my1 - my0));
    }
    Some((x0, y0, w, h))
}

/// crop_box_of 从页面字典读 /CropBox（可能是间接引用；元素也可能各自是引用）。
/// pdf_oxide 没有公开的 CropBox 访问器，故照其 get_page_media_box 的套路自行解析。
fn crop_box_of(doc: &PdfDocument, index: usize) -> Option<(f32, f32, f32, f32)> {
    use pdf_oxide::object::Object;
    fn num(o: &Object) -> Option<f32> {
        match o {
            Object::Integer(v) => Some(*v as f32),
            Object::Real(v) => Some(*v as f32),
            _ => None,
        }
    }
    // 单层解引用（resolve_obj_ref 是私有的，用公开的 resolve_object）
    let deref = |doc: &PdfDocument, o: &Object| -> Object {
        match o {
            Object::Reference(_) => doc.resolve_object(o).unwrap_or_else(|_| o.clone()),
            other => other.clone(),
        }
    };

    let page = doc.get_page(index).ok()?;
    let dict = page.as_dict()?;
    let resolved = deref(doc, dict.get("CropBox")?);
    let arr = resolved.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    Some((
        num(&deref(doc, &arr[0]))?,
        num(&deref(doc, &arr[1]))?,
        num(&deref(doc, &arr[2]))?,
        num(&deref(doc, &arr[3]))?,
    ))
}

/// page_size 取第 index 页可见区域尺寸（pt），考虑旋转
fn page_size(doc: &PdfDocument, index: usize) -> Option<(f32, f32)> {
    let (_, _, mut w, mut h) = page_box(doc, index)?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // 旋转 90/270 时宽高互换
    if let Ok(rot) = doc.get_page_rotation(index) {
        let r = ((rot % 360) + 360) % 360;
        if r == 90 || r == 270 {
            std::mem::swap(&mut w, &mut h);
        }
    }
    Some((w, h))
}

// ----------------------------------------------------------------------------
// CCITT 扫描页兜底渲染
//
// pdf_oxide 解 CCITTFaxDecode 时，/DecodeParms 没写 /K 就按 -1（Group 4）解
// （object::extract_ccitt_params_with_width，源码注释写着 "Default: Group 4"）。
// 但 ISO 32000-1 Table 11 规定 /K 缺省是 0，即 Group 3 一维。用 G4 解 G3 一维流
// 不会报错、解出的位图尺寸也对，但像素全白 —— 整页白屏，且不产生任何告警。
// 复印机直出的扫描书常这么写（实测 RICOH Aficio MP 7502 整本 383 页都不带 /K）。
//
// 对这类页面自己接管：把 /K 补成规范默认值 0，仍用 pdf_oxide 的解码器重解，
// 再按 CropBox 裁切、按 /Rotate 摆正、缩到目标 DPI，输出灰度 JPEG。
// ----------------------------------------------------------------------------

/// 走兜底路径的门槛：只接管「铺满整页的大扫描图」，
/// 避免把「小 logo + 正文文字」的页面整页换成一张拉伸的小图。
const CCITT_MIN_SIDE: u32 = 800;
const CCITT_RATIO_TOL: f32 = 0.05;

/// deref_obj 单层解引用（间接对象 → 实际对象）
fn deref_obj(doc: &PdfDocument, o: &Object) -> Object {
    match o {
        Object::Reference(_) => doc.resolve_object(o).unwrap_or_else(|_| o.clone()),
        other => other.clone(),
    }
}

/// decode_inverts 判断图像字典的 /Decode 是否为 [1 0]（黑白反转）
fn decode_inverts(doc: &PdfDocument, xdict: &HashMap<String, Object>) -> bool {
    let d = match xdict.get("Decode") {
        Some(v) => deref_obj(doc, v),
        None => return false,
    };
    match d.as_array() {
        Some(a) if a.len() >= 2 => {
            let first = deref_obj(doc, &a[0]);
            let n = first.as_integer().map(|v| v as f64).or_else(|| first.as_real());
            n.map(|v| v > 0.5).unwrap_or(false)
        }
        _ => false,
    }
}

/// 从页面 Resources 里挑出「铺满整页、且 DecodeParms 缺 /K」的 CCITT 扫描图。
/// 同页上的 Form（Pdftools 拼版戳、水印）和小图忽略；有多张候选时取最大的一张。
fn find_ccitt_fullpage<'a>(
    doc: &PdfDocument,
    xobjs: &'a HashMap<String, Object>,
    mw: f32,
    mh: f32,
) -> Option<(&'a Object, HashMap<String, Object>, HashMap<String, Object>, u32, u32)> {
    let mut best: Option<(&Object, HashMap<String, Object>, HashMap<String, Object>, u32, u32, u64)> = None;
    for rref in xobjs.values() {
        let xobj = deref_obj(doc, rref);
        let xdict = match &xobj {
            Object::Stream { dict, .. } => dict,
            _ => continue,
        };
        let subtype = xdict
            .get("Subtype")
            .and_then(|o| deref_obj(doc, o).as_name().map(|s| s.to_string()));
        if subtype.as_deref() != Some("Image") {
            continue;
        }
        let is_ccitt = match xdict.get("Filter").map(|o| deref_obj(doc, o)) {
            Some(Object::Name(n)) => n == "CCITTFaxDecode",
            Some(Object::Array(a)) => a.iter().any(|f| deref_obj(doc, f).as_name() == Some("CCITTFaxDecode")),
            _ => false,
        };
        if !is_ccitt {
            continue;
        }
        // 写了 /K 的话 pdf_oxide 读得到，常规渲染就是对的，不必接管
        let parms = match xdict.get("DecodeParms") {
            Some(v) => deref_obj(doc, v),
            None => continue,
        };
        let pd = match parms.as_dict() {
            Some(d) => d,
            None => continue,
        };
        if pd.contains_key("K") {
            continue;
        }
        let iw = match xdict.get("Width").and_then(|o| deref_obj(doc, o).as_integer()) {
            Some(v) if v > 0 => v as u32,
            _ => continue,
        };
        let ih = match xdict.get("Height").and_then(|o| deref_obj(doc, o).as_integer()) {
            Some(v) if v > 0 => v as u32,
            _ => continue,
        };
        if iw < CCITT_MIN_SIDE || ih < CCITT_MIN_SIDE {
            continue;
        }
        let ratio_img = ih as f32 / iw as f32;
        let ratio_page = mh / mw;
        if (ratio_img / ratio_page - 1.0).abs() > CCITT_RATIO_TOL {
            continue;
        }
        let area = iw as u64 * ih as u64;
        let better = best.as_ref().map(|b| area > b.5).unwrap_or(true);
        if better {
            best = Some((rref, xdict.clone(), pd.clone(), iw, ih, area));
        }
    }
    best.map(|(rref, xdict, pd, iw, ih, _)| (rref, xdict, pd, iw, ih))
}

/// ccitt_page_jpeg 若本页是「/DecodeParms 缺 /K 的整页 CCITT 扫描图」，
/// 按规范默认值 K=0 重解并输出 JPEG；不满足条件返回 None 交回常规渲染。
fn ccitt_page_jpeg(doc: &PdfDocument, page0: usize, dpi: u32) -> Option<Vec<u8>> {
    use image::imageops::{self, FilterType};
    use image::GrayImage;
    use pdf_oxide::decoders::CcittParams;
    use pdf_oxide::extractors::images::extract_image_from_xobject;

    let page = doc.get_page(page0).ok()?;
    let res = deref_obj(doc, page.as_dict()?.get("Resources")?);
    let xobjs = deref_obj(doc, res.as_dict()?.get("XObject")?);
    let xobjs = xobjs.as_dict()?;

    let media = doc.get_page_media_box(page0).ok()?;
    let mw = (media.2 - media.0).abs();
    let mh = (media.3 - media.1).abs();
    if mw <= 1.0 || mh <= 1.0 {
        return None;
    }

    let (rref, xdict, pd, iw, ih) = find_ccitt_fullpage(doc, xobjs, mw, mh)?;
    let objref = rref.as_reference();
    let xobj = deref_obj(doc, rref);

    // 用 pdf_oxide 的提取器拿到图，只把 CCITT 参数换成规范默认值后重解
    let mut img = extract_image_from_xobject(Some(doc), &xobj, objref, None).ok()?;
    let mut params = CcittParams {
        k: 0, // ← 规范默认值；pdf_oxide 在这里默认成了 -1
        columns: pd
            .get("Columns")
            .and_then(|o| o.as_integer())
            .map(|v| v as u32)
            .unwrap_or(iw),
        rows: pd
            .get("Rows")
            .and_then(|o| o.as_integer())
            .map(|v| v as u32)
            .or(Some(ih)),
        black_is_1: pd.get("BlackIs1").and_then(|o| o.as_bool()).unwrap_or(false),
        end_of_line: pd.get("EndOfLine").and_then(|o| o.as_bool()).unwrap_or(false),
        encoded_byte_align: pd
            .get("EncodedByteAlign")
            .and_then(|o| o.as_bool())
            .unwrap_or(false),
        end_of_block: pd.get("EndOfBlock").and_then(|o| o.as_bool()).unwrap_or(true),
    };
    // /BlackIs1 与 /Decode [1 0] 都能翻转黑白，两者独立且可叠加（同时出现相互抵消）
    if decode_inverts(doc, &xdict) {
        params.black_is_1 = !params.black_is_1;
    }
    img.set_ccitt_params(params);
    let mut gray: GrayImage = img.to_dynamic_image().ok()?.to_luma8();

    // CropBox 裁切：在未旋转的图像坐标系里做。
    // PDF 原点在左下、图像原点在左上，纵向需翻转。
    if let Some((cx, cy, cw, ch)) = page_box(doc, page0) {
        let sx = gray.width() as f32 / mw;
        let sy = gray.height() as f32 / mh;
        let my0 = media.1.min(media.3);
        let px = (((cx - media.0.min(media.2)) * sx).round().max(0.0)) as u32;
        let py = ((((my0 + mh) - (cy + ch)) * sy).round().max(0.0)) as u32;
        let pw = ((cw * sx).round().max(1.0) as u32).min(gray.width().saturating_sub(px));
        let ph = ((ch * sy).round().max(1.0) as u32).min(gray.height().saturating_sub(py));
        if pw > 0 && ph > 0 && (pw != gray.width() || ph != gray.height()) {
            gray = imageops::crop_imm(&gray, px, py, pw, ph).to_image();
        }
    }

    let rot = ((doc.get_page_rotation(page0).unwrap_or(0) % 360) + 360) % 360;
    gray = match rot {
        90 => imageops::rotate90(&gray),
        180 => imageops::rotate180(&gray),
        270 => imageops::rotate270(&gray),
        _ => gray,
    };

    // 缩到目标 DPI。扫描图分辨率通常远高于阅读需要（本例 4304px → 300dpi 只需 2150px）；
    // 反过来分辨率不足时不放大，放大只会更糊且体积更大。
    let (pt_w, _pt_h) = page_size(doc, page0).map(|(w, h)| (w, h)).unwrap_or((mw, mh));
    let tw = ((pt_w * dpi as f32 / 72.0).round() as u32).max(1);
    if tw < gray.width() {
        let th = ((gray.height() as f32 * tw as f32 / gray.width() as f32).round() as u32).max(1);
        gray = imageops::resize(&gray, tw, th, FilterType::Triangle);
    }

    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&gray)
        .ok()?;
    Some(out)
}

/// render_page_jpeg 渲染单页为 JPEG（0-based 页号）。命中书库磁盘缓存直接读，
/// 未命中则渲染并写盘缓存。
pub fn render_page_jpeg(pdf_path: &Path, page0: usize, dpi: u32) -> Result<Vec<u8>, String> {
    let ip = image_cache_path(pdf_path, page0 + 1, dpi);
    if let Ok(data) = std::fs::read(&ip) {
        if !data.is_empty() {
            return Ok(data);
        }
    }

    let data = with_doc(pdf_path, |doc| {
        // 缺 /K 的 CCITT 扫描页 pdf_oxide 会解成全白，先走自己的兜底路径
        if let Some(d) = ccitt_page_jpeg(doc, page0, dpi) {
            return Ok(d);
        }

        let mut opts = RenderOptions::default();
        opts.dpi = dpi;
        opts.format = ImageFormat::Jpeg;
        opts.jpeg_quality = JPEG_QUALITY;
        opts.background = Some([1.0, 1.0, 1.0, 1.0]);

        // 判断是否需要按 CropBox 裁剪：CropBox 明显小于 MediaBox 时
        // （典型是双页对开扫描书：MediaBox 是整张纸，CropBox 是本页那一半），
        // 必须用 render_page_region 只输出可见区域，否则会把两页画进一张图。
        let media = doc.get_page_media_box(page0).ok();
        let cbox = page_box(doc, page0);
        let need_crop = match (media, cbox) {
            (Some(m), Some((cx, cy, cw, ch))) => {
                let mw = (m.2 - m.0).abs();
                let mh = (m.3 - m.1).abs();
                // 面积差超过 1% 才裁，避免浮点误差导致无谓的二次编码
                let shrunk = cw < mw - mw * 0.01 || ch < mh - mh * 0.01;
                let moved = cx.abs() > 0.5 || cy.abs() > 0.5;
                shrunk || moved
            }
            _ => false,
        };

        // pdf_oxide 内部对异常 PDF 可能 panic，用 catch_unwind 兜底避免整个服务崩溃
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match (need_crop, cbox) {
                (true, Some(rect)) => render_page_region(doc, page0, rect, &opts),
                _ => render_page(doc, page0, &opts),
            }
        }));
        match res {
            Ok(Ok(img)) => Ok(img.data),
            Ok(Err(e)) => {
                // 裁剪路径失败时回退整页渲染，至少让用户看到内容
                if need_crop {
                    logf!("page {} 裁剪渲染失败({e:?})，回退整页", page0);
                    let fb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render_page(doc, page0, &opts)
                    }));
                    match fb {
                        Ok(Ok(img)) => return Ok(img.data),
                        _ => {}
                    }
                }
                Err(format!("render_page: {e:?}"))
            }
            Err(_) => Err(format!("render panic on page {page0}")),
        }
    })?;

    if let Some(dir) = ip.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&ip, &data);
    Ok(data)
}

/// clear_doc_cache 主动释放文档缓存（预留：切换书/退出阅读时可调用）
#[allow(dead_code)]
pub fn clear_doc_cache() {
    if let Ok(mut g) = doc_cache().lock() {
        let _ = g.take();
    }
}
