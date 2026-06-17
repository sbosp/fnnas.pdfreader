# 先设置环境变量
import os
os.environ["PDFR_SHARE_PATHS"] = '/Users/king/Downloads:'
os.environ["PDFR_ACCESSIBLE_PATHS"] = '/Users/king/Github:'
os.environ["PDFR_GATEWAY_PREFIX"] = '/app/fnnas-pdfreader'
os.environ['PDFR_TCP_PORT'] = '6002'
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

# 现在导入pdfserver
import pdfserver as pdfserver

if __name__ == '__main__':
    # 再次检查导入后的值
    print("=== 导入后检查 ===")
    print(f"TCP_PORT in pdfserver: {repr(pdfserver.TCP_PORT)}")
    print(f"Should use TCP mode: {bool(pdfserver.TCP_PORT)}")
    print("==================")

    try:
        pdfserver.main()
    except Exception:
        import traceback
        raise