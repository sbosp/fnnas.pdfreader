# 先设置环境变量
import os
import time

os.environ["PDFR_SHARE_PATHS"] = '/Users/king/Downloads:'
os.environ["PDFR_ACCESSIBLE_PATHS"] = '/Users/king/Github:'
os.environ["PDFR_GATEWAY_PREFIX"] = '/app/fnnas-pdfreader'
os.environ['PDFR_TCP_PORT'] = '0'  # Flask 版本默认使用 Unix Socket，设为 0
os.environ['PDFR_SOCK'] = 'app.sock'
os.environ["PDFR_DATA_DIR"] = '/Users/king/Downloads/pdfread/data'
os.environ["PDFR_REQUIRE_AUTH"] = '0'
os.environ["PDFR_PEERCRED_CHECK"] = '0'  # TCP模式下禁用对等凭据检查
os.environ["PDFR_LOGFILE"] = 'pdfreader_log.log'
os.environ["PDFR_APPNAME"] = 'pdfreader'
os.environ["PDFR_DEBUG"] = '1'

# 调试：检查环境变量是否设置正确
print("=== 环境变量调试信息 ===")
print(f"PDFR_TCP_PORT: '{os.environ.get('PDFR_TCP_PORT')}'")
print(f"PDFR_SOCK: '{os.environ.get('PDFR_SOCK')}'")
print("========================")

# 现在导入 Flask 版本
import pdfserver as pdfserver

if __name__ == '__main__':
    try:
        # 使用 TCP 模式启动（--port 5173）
        pdfserver.args.port = 5173
        pdfserver.args.host = '127.0.0.1'
        pdfserver.args.debug = True
        pdfserver.main()
    except Exception:
        import traceback
        traceback.print_exc()
        raise
    start = time.time()
    print(len(pdfserver.extract_page_pdf(pdfserver.load_file_map('1213123213')['e995d9129a4b8a74'], 30, 1)),
          time.time() - start)
    start = time.time()
    print(len(pdfserver.render_page(pdfserver.load_file_map('1213123213')['e995d9129a4b8a74'], 30, 180)),
          time.time() - start)
    start = time.time()