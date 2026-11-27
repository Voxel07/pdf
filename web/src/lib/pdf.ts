import * as pdfjs from 'pdfjs-dist/legacy/build/pdf.mjs';
import workerUrl from 'pdfjs-dist/legacy/build/pdf.worker.min.mjs?url';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

export type PdfDoc = pdfjs.PDFDocumentProxy;

export async function loadPdf(url: string, headers: Record<string, string> = {}): Promise<PdfDoc> {
  const res = await fetch(url, { headers });
  if (!res.ok) throw new Error(`could not load PDF (${res.status})`);
  const data = new Uint8Array(await res.arrayBuffer());
  return pdfjs.getDocument({ data }).promise;
}

/** Opens a PDF blob; destroy the returned task when done to release its worker. */
export async function openPdfBlob(blob: Blob): Promise<pdfjs.PDFDocumentLoadingTask> {
  return pdfjs.getDocument({ data: new Uint8Array(await blob.arrayBuffer()) });
}
