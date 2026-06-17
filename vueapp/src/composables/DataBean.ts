import {PDFPageProxy} from "pdfjs-dist/types/src/display/api";

export interface PdfPageData {
    pageNum: number
    width: number
    height: number
    page: (pageNum: number) => Promise<PDFPageProxy>
    render: (canvas: HTMLCanvasElement, scale: number, page: PDFPageProxy) => Promise<void>
}