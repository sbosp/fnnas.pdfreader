# 先设置环境变量
import os
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
import pdfserver_flask as pdfserver

if __name__ == '__main__':
    # Flask 版本本地调试：使用 TCP 模式，端口 5173
    print("=== Flask 版本本地调试启动 ===")
    print("访问地址: http://127.0.0.1:5173/app/fnnas-pdfreader/")
    print("==============================")

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