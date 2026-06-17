# 先设置环境变量
import os
from pathlib import Path

import fitz

os.environ["PDFR_SHARE_PATHS"] = '/Users/king/Downloads:'
os.environ["PDFR_ACCESSIBLE_PATHS"] = '/Users/king/Github:'
os.environ["PDFR_GATEWAY_PREFIX"] = '/app/fnnas-pdfreader'
os.environ["PDFR_SOCK"] = 'app.sock'
os.environ["PDFR_TCP_PORT"] = '6004'
os.environ["PDFR_DATA_DIR"] = '/Users/king/Downloads/pdfread/data'
os.environ["PDFR_REQUIRE_AUTH"] = '0'
os.environ["PDFR_PEERCRED_CHECK"] = '0'  # TCP模式下禁用对等凭据检查
os.environ["PDFR_LOGFILE"] = 'pdfreader_log.log'
os.environ["PDFR_APPNAME"] = 'pdfreader'
os.environ["PDFR_DEBUG"] = '1'

# 调试：检查环境变量是否设置正确
print("=== 环境变量调试信息 ===")
print(f"PDFR_TCP_PORT: {repr(os.environ.get('PDFR_TCP_PORT'))}")
print(f"PDFR_SOCK: {os.environ.get('PDFR_SOCK')}")
print("========================")

# 现在导入pdfserver
import pdfserver as pdfserver

import pymupdf

if __name__ == '__main__':
    # 再次检查导入后的值
    print("=== 导入后检查 ===")
    print(f"TCP_PORT in pdfserver: {repr(pdfserver.TCP_PORT)}")
    print(f"Should use TCP mode: {bool(pdfserver.TCP_PORT)}")
    print("==================")

    # print(pdfserver.scan_all("10086"))
    file_map = pdfserver.load_file_map('10086')
    entry = file_map.get('5f80d64419e317ba', {})
    entry = file_map.get('aa830730abe80033', {})
    print(entry)
    print(len(pdfserver.extract_page_pdf(entry, 50,1)) / 1024)
    print(len(pdfserver.render_page(entry, 50)) / 1024)
    # print(fitz.open('/Users/king/Downloads/OEC_OECT_OEA 昔映NAS与网心云 进SSH与装Docker-京东云、网心云、玩客云等PCDN云设备-恩山无线论坛 - Powered by Discuz!.pdf'))
    from pdf_oxide import PdfDocument

    # doc = PdfDocument(entry['path'])
    # dst_doc = PdfDocument()
    # print('pdf_oxide',doc[15])

    # doc = pymupdf.open()  # 空 PDF
    # doc.insert_pdf(pymupdf.open(entry['path']), from_page=13, to_page=13)  # C层复制页对象，不落盘
    # doc.subset_fonts()
    # data = doc.tobytes(garbage=0)  # 纯内存 → bytes
    # doc.close()
    # print(len(data) / 1024)
