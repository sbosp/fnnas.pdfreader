// 本地复现 NAS 上「pagepdf 卡满 CPU 后 OOM 重启」的基准测试。
//
// 复刻 main.rs 里 extract_page_pdf 的真实逻辑（DocumentEditor::open + extract_pages_to_bytes），
// 对一本大 PDF 逐页抽取，逐次打印：本页耗时 + 进程当前/峰值 RSS 内存。
//
// 用法：
//   cargo run --release --example bench_extract -- <pdf路径> [起始页] [页数]
// 例：
//   cargo run --release --example bench_extract -- /Users/king/Downloads/test.pdf 75 8
//
// 关注点：
//   1) 单页耗时是否真有十几秒（NAS 日志里前两个请求各 ~9.5s）
//   2) 每抽一页 RSS 是否阶梯式上涨（== open 把整本 88MB 读进来 + 解析缓存膨胀）
//   3) 峰值 RSS 有多大 —— 乘以前端/后端并发数就是 OOM 触发点

use std::time::Instant;

/// 取当前进程的内存占用（RSS）与历史峰值，单位统一为 MB。
/// macOS: ru_maxrss 单位是字节；Linux: 单位是 KB。这里只用峰值做量级判断。
fn peak_rss_mb() -> f64 {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            let maxrss = usage.ru_maxrss as f64;
            // macOS 返回字节，Linux 返回 KB
            if cfg!(target_os = "macos") {
                maxrss / (1024.0 * 1024.0)
            } else {
                maxrss / 1024.0
            }
        } else {
            0.0
        }
    }
}

/// 读 /proc/self/statm 或用 mach 拿当前实时 RSS（macOS 用 task_info）。
/// 简化：macOS 下用 `ps` 太重，这里直接用峰值近似「当前」的上界即可，
/// 因为我们关心的是「会不会越来越大 / 有没有到 OOM 量级」。
fn extract_one(path: &str, page: usize) -> Result<Vec<u8>, String> {
    use pdf_oxide::editor::DocumentEditor;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut editor = DocumentEditor::open(path).map_err(|e| format!("open: {e:?}"))?;
        editor
            .extract_pages_to_bytes(&[page])
            .map_err(|e| format!("extract_pages_to_bytes: {e:?}"))
    }));
    match result {
        Ok(r) => r,
        Err(e) => {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(format!("panic on page {page}: {msg}"))
        }
    }
}

/// 复用模式：只 open 一次，复用同一个 DocumentEditor 连续抽多页，
/// 验证「文档级缓存」后峰值 RSS 是否封顶在单本量级（~182MB），不随抽页数上涨。
fn run_reuse(path: &str, start_page: usize, count: usize) {
    use pdf_oxide::editor::DocumentEditor;
    let file_mb = std::fs::metadata(path).map(|m| m.len() as f64 / 1048576.0).unwrap_or(0.0);
    println!("========================================================");
    println!("文件: {path}  ({file_mb:.1} MB)");
    println!("复用模式: 只 open 一次，同一 editor 连抽 {count} 页（模拟文档级缓存命中）");
    println!("========================================================");
    let t = Instant::now();
    let mut editor = match DocumentEditor::open(path) {
        Ok(e) => e,
        Err(e) => { println!("open 失败: {e:?}"); return; }
    };
    println!("open 一次: {:.2}s  峰值RSS={:.1}MB", t.elapsed().as_secs_f64(), peak_rss_mb());
    println!("--------------------------------------------------------");
    for p in start_page..start_page + count {
        let t = Instant::now();
        match editor.extract_pages_to_bytes(&[p]) {
            Ok(b) => println!(
                "  page {:>3}: {:>6.3}s 产出 {:>6.1}KB  峰值RSS={:.1}MB",
                p, t.elapsed().as_secs_f64(), b.len() as f64 / 1024.0, peak_rss_mb()
            ),
            Err(e) => println!("  page {p}: ❌ {e:?}"),
        }
    }
    println!("--------------------------------------------------------");
    println!("复用同一 editor 连抽 {count} 页后峰值 RSS: {:.0} MB", peak_rss_mb());
    println!("（对比：若每页各 open，{count} 页并发会到 ~{:.0}MB）", 182.0 * count as f64);
    println!("========================================================");
}

/// 并发模式：n 个线程「同时」各自 open 整本文件 + 抽一页，
/// 复刻后端多 worker 并发抽页的真实场景，观测峰值 RSS 是否叠成 N×单本。
fn run_concurrent(path: &str, start_page: usize, n: usize) {
    let file_mb = std::fs::metadata(path).map(|m| m.len() as f64 / 1048576.0).unwrap_or(0.0);
    println!("========================================================");
    println!("文件: {path}  ({file_mb:.1} MB)");
    println!("并发模式: {n} 个线程同时各自 open 整本 + 抽 1 页（模拟 {n} 个后端 worker 并发）");
    println!("========================================================");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(n));
    let t = Instant::now();
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let path = path.to_string();
            let barrier = barrier.clone();
            let page = start_page + i;
            std::thread::spawn(move || {
                barrier.wait(); // 让所有线程尽量同一瞬间开抽，峰值最大
                let t = Instant::now();
                let r = extract_one(&path, page);
                (page, r.map(|b| b.len()), t.elapsed())
            })
        })
        .collect();
    for h in handles {
        let (page, r, dt) = h.join().unwrap();
        match r {
            Ok(sz) => println!("  线程 page {page}: {:.2}s 产出 {:.1}KB", dt.as_secs_f64(), sz as f64 / 1024.0),
            Err(e) => println!("  线程 page {page}: {:.2}s ❌ {e}", dt.as_secs_f64()),
        }
    }
    println!("--------------------------------------------------------");
    println!("{n} 并发总耗时: {:.2}s", t.elapsed().as_secs_f64());
    println!("峰值 RSS: {:.0} MB  (单本约 182MB → {n} 并发理论 ~{:.0}MB)", peak_rss_mb(), 182.0 * n as f64);
    println!("========================================================");
}

/// 精确拆解 open 的耗时构成：到底慢在 IO（fs::read 88MB）、还是解析（xref/parse）、
/// 还是 page_count 遍历页树。每项跑多次取中位数，排除首次冷启动（磁盘 cache 未热）的干扰。
fn run_profile_open(path: &str) {
    use pdf_oxide::PdfDocument;
    use pdf_oxide::editor::DocumentEditor;
    let file_mb = std::fs::metadata(path).map(|m| m.len() as f64 / 1048576.0).unwrap_or(0.0);
    println!("========================================================");
    println!("文件: {path}  ({file_mb:.1} MB)");
    println!("PROFILE_OPEN: 拆解 open 各阶段耗时（每项 5 次，报首次/中位数）");
    println!("========================================================");

    let runs = 5;
    let median = |mut v: Vec<f64>| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] };
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;

    // ① 纯 IO：只 std::fs::read 整本，什么都不解析
    let mut io = Vec::new();
    let mut first_io = 0.0;
    for i in 0..runs {
        let t = Instant::now();
        let data = std::fs::read(path).unwrap();
        let dt = ms(t.elapsed());
        std::hint::black_box(&data);
        if i == 0 { first_io = dt; }
        io.push(dt);
    }
    println!("① fs::read 纯IO      : 首次 {:>8.2}ms | 中位 {:>8.2}ms  ({:.1} MB → {:.0} MB/s)",
        first_io, median(io.clone()), file_mb, file_mb / (median(io) / 1000.0));

    // ② PdfDocument::open 总耗时（fs::read + parse header + parse xref）
    let mut pdo = Vec::new();
    let mut first_pdo = 0.0;
    for i in 0..runs {
        let t = Instant::now();
        let doc = PdfDocument::open(path).unwrap();
        let dt = ms(t.elapsed());
        std::hint::black_box(&doc);
        if i == 0 { first_pdo = dt; }
        pdo.push(dt);
    }
    println!("② PdfDocument::open  : 首次 {:>8.2}ms | 中位 {:>8.2}ms  (= IO + header + xref解析)",
        first_pdo, median(pdo));

    // ③ 在已 open 的 doc 上单独测 page_count（走 /Count，理论 O(1)）
    {
        let doc = PdfDocument::open(path).unwrap();
        let mut pc = Vec::new();
        for _ in 0..runs {
            let t = Instant::now();
            let c = doc.page_count().unwrap_or(0);
            std::hint::black_box(c);
            pc.push(ms(t.elapsed()));
        }
        println!("③ page_count 单独    :            | 中位 {:>8.2}ms  (在已open的doc上)", median(pc));
    }

    // ④ DocumentEditor::open 总耗时（= PdfDocument::open + page_count + find_max_object_id）
    let mut de = Vec::new();
    let mut first_de = 0.0;
    for i in 0..runs {
        let t = Instant::now();
        let ed = DocumentEditor::open(path).unwrap();
        let dt = ms(t.elapsed());
        std::hint::black_box(&ed);
        if i == 0 { first_de = dt; }
        de.push(dt);
    }
    println!("④ DocumentEditor::open: 首次 {:>8.2}ms | 中位 {:>8.2}ms  (抽页/前端实际走这条)",
        first_de, median(de));

    println!("--------------------------------------------------------");
    println!("结论：④减①的差就是「解析开销」；若①(IO)占大头，说明瓶颈在读盘，Rust侧解析并不慢。");
    println!("对照：pymupdf(MuPDF) open 走 mmap+lazy xref，不预读整本 → 可低至个位数ms。");
    println!("========================================================");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/Users/king/Downloads/test.pdf".to_string());
    let start_page: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(75);
    let count: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(8);

    // 第 4 个参数：并发线程数。给了就走「并发 open」模式，模拟后端 N 个 worker 同时抽页。
    if let Some(n) = args.get(4).and_then(|s| s.parse::<usize>().ok()) {
        run_concurrent(&path, start_page, n);
        return;
    }

    // 环境变量 REUSE=1：走「复用同一 editor 抽多页」模式，验证缓存后内存是否封顶（不叠加）。
    if std::env::var("REUSE").as_deref() == Ok("1") {
        run_reuse(&path, start_page, count);
        return;
    }

    // 环境变量 PROFILE_OPEN=1：精确拆解 open 到底慢在哪（IO vs 解析 vs page_count）。
    if std::env::var("PROFILE_OPEN").as_deref() == Ok("1") {
        run_profile_open(&path);
        return;
    }

    let file_mb = std::fs::metadata(&path).map(|m| m.len() as f64 / 1048576.0).unwrap_or(0.0);
    println!("========================================================");
    println!("文件: {path}");
    println!("大小: {file_mb:.1} MB");
    println!("测试: 从第 {start_page} 页开始，抽 {count} 页（每页独立 open + extract）");
    println!("起始峰值 RSS: {:.1} MB", peak_rss_mb());
    println!("========================================================");

    // ---- meta 阶段：open + page_count + 逐页 media_box（复刻 get_doc_meta）----
    {
        use pdf_oxide::PdfDocument;
        let t = Instant::now();
        match PdfDocument::open(&path) {
            Ok(doc) => {
                let t_open = t.elapsed();
                let cnt = doc.page_count().unwrap_or(0);
                let t_count = t.elapsed();
                let mut ok_dims = 0usize;
                for i in 0..cnt {
                    if doc.get_page_media_box(i).is_ok() {
                        ok_dims += 1;
                    }
                }
                let t_dims = t.elapsed();
                println!(
                    "[meta] open={:?} page_count={}({:?}) 全{}页尺寸={:?}(+{:?})  峰值RSS={:.1}MB",
                    t_open, cnt, t_count - t_open, ok_dims, t_dims, t_dims - t_count, peak_rss_mb()
                );
            }
            Err(e) => println!("[meta] open 失败: {e:?}"),
        }
    }
    println!("--------------------------------------------------------");

    // ---- 抽页阶段：逐页测耗时 + 峰值 RSS ----
    let mut total = std::time::Duration::ZERO;
    let mut ok = 0usize;
    let mut fail = 0usize;
    for p in start_page..start_page + count {
        let t = Instant::now();
        let r = extract_one(&path, p);
        let dt = t.elapsed();
        total += dt;
        match r {
            Ok(bytes) => {
                ok += 1;
                println!(
                    "  page {:>3}: {:>8.2}s  产出={:>6.1}KB  峰值RSS={:.1}MB",
                    p,
                    dt.as_secs_f64(),
                    bytes.len() as f64 / 1024.0,
                    peak_rss_mb()
                );
            }
            Err(e) => {
                fail += 1;
                println!(
                    "  page {:>3}: {:>8.2}s  ❌ {}  峰值RSS={:.1}MB",
                    p,
                    dt.as_secs_f64(),
                    e,
                    peak_rss_mb()
                );
            }
        }
    }

    println!("========================================================");
    println!(
        "结果: 成功 {ok} / 失败 {fail}，抽页总耗时 {:.2}s，平均 {:.2}s/页",
        total.as_secs_f64(),
        total.as_secs_f64() / count.max(1) as f64
    );
    println!("最终峰值 RSS: {:.1} MB", peak_rss_mb());
    println!("========================================================");
    println!("换算 OOM 触发点（前端并发2~3 × 后端8worker，每请求独立 open）:");
    let peak = peak_rss_mb();
    println!("  并发 2 个抽页 ≈ {:.0} MB", peak * 2.0);
    println!("  并发 4 个抽页 ≈ {:.0} MB", peak * 4.0);
    println!("  并发 8 个抽页 ≈ {:.0} MB  <- NAS 内存有限时在此附近 OOM", peak * 8.0);
}
