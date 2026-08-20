//! Rasterize scanned PDFs to JPEG pages via Windows.Data.Pdf.

use std::path::Path;

use crate::llm::files::IMAGE_MAX_BYTES;

pub const MAX_PDF_OCR_PAGES: u32 = 20;
pub const MAX_PDF_BYTES: u64 = 80 * 1024 * 1024;
const PDF_RENDER_DPI: f32 = 150.0;
const PDF_DIP_DPI: f32 = 96.0;
const PDF_MAX_EDGE_PX: f32 = 1600.0;
const PDF_MIN_EDGE_PX: u32 = 320;

#[derive(Debug, Clone)]
pub struct RasterPage {
    pub page_no: u32,
    pub jpeg: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RasterizePdf {
    pub pages: Vec<RasterPage>,
    pub total_pages: u32,
    pub truncated: bool,
}

pub fn pdf_page_paragraph_id(page_no: u32) -> String {
    format!("pdf-page:{page_no}")
}

pub fn pdf_page_title(filename: &str, page_no: u32) -> String {
    format!("{filename}（{page_no}ページ目）")
}

pub fn truncation_warning(kept: u32, total: u32) -> String {
    format!("先頭{kept}ページだけ読み取ります（全{total}ページ）。")
}

pub fn dest_size(width: f32, height: f32, scale: f32) -> (u32, u32) {
    let w = (width * PDF_RENDER_DPI / PDF_DIP_DPI).max(1.0) * scale;
    let h = (height * PDF_RENDER_DPI / PDF_DIP_DPI).max(1.0) * scale;
    let long = w.max(h);
    let fit = if long > PDF_MAX_EDGE_PX {
        PDF_MAX_EDGE_PX / long
    } else {
        1.0
    };
    (
        (w * fit).round().max(1.0) as u32,
        (h * fit).round().max(1.0) as u32,
    )
}

#[cfg(not(windows))]
pub fn rasterize_pdf(_path: &Path) -> Result<RasterizePdf, String> {
    Err("スキャンPDFの画像化は Windows のみです。".into())
}

#[cfg(windows)]
pub fn rasterize_pdf(path: &Path) -> Result<RasterizePdf, String> {
    win::rasterize_pdf(path)
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::fs;

    use windows::core::Interface;
    use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
    use windows::Foundation::IClosable;
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    fn win_err(e: windows::core::Error) -> String {
        format!("PDF を画像化できませんでした（{e}）。")
    }

    fn ensure_mta() {
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    }

    fn bytes_to_stream(bytes: &[u8]) -> Result<InMemoryRandomAccessStream, String> {
        let stream = InMemoryRandomAccessStream::new().map_err(win_err)?;
        let writer = DataWriter::CreateDataWriter(&stream).map_err(win_err)?;
        writer.WriteBytes(bytes).map_err(win_err)?;
        writer
            .StoreAsync()
            .map_err(win_err)?
            .join()
            .map_err(win_err)?;
        let _ = writer.DetachStream();
        stream.Seek(0).map_err(win_err)?;
        Ok(stream)
    }

    fn stream_to_bytes(stream: &InMemoryRandomAccessStream) -> Result<Vec<u8>, String> {
        stream.Seek(0).map_err(win_err)?;
        let n = stream.Size().map_err(win_err)?;
        if n == 0 {
            return Err("ページ画像を作れませんでした。".into());
        }
        if n > u32::MAX as u64 {
            return Err("ページ画像が大きすぎます。".into());
        }
        let reader = DataReader::CreateDataReader(stream).map_err(win_err)?;
        let loaded = reader
            .LoadAsync(n as u32)
            .map_err(win_err)?
            .join()
            .map_err(win_err)?;
        let mut buf = vec![0u8; loaded as usize];
        reader.ReadBytes(&mut buf).map_err(win_err)?;
        let _ = reader.DetachStream();
        Ok(buf)
    }

    fn close_page(page: &PdfPage) {
        if let Ok(c) = page.cast::<IClosable>() {
            let _ = c.Close();
        }
    }

    fn render_page_jpeg(page: &PdfPage, dest_w: u32, dest_h: u32) -> Result<Vec<u8>, String> {
        let out = InMemoryRandomAccessStream::new().map_err(win_err)?;
        let opts = PdfPageRenderOptions::new().map_err(win_err)?;
        opts.SetDestinationWidth(dest_w).map_err(win_err)?;
        opts.SetDestinationHeight(dest_h).map_err(win_err)?;
        let jpeg_id = BitmapEncoder::JpegEncoderId().map_err(win_err)?;
        opts.SetBitmapEncoderId(jpeg_id).map_err(win_err)?;
        page.RenderWithOptionsToStreamAsync(&out, &opts)
            .map_err(win_err)?
            .join()
            .map_err(win_err)?;
        stream_to_bytes(&out)
    }

    fn render_page_fitting(page: &PdfPage) -> Result<Vec<u8>, String> {
        let size = page.Size().map_err(win_err)?;
        if size.Width <= 0.0 || size.Height <= 0.0 {
            return Err("ページサイズが不正です。".into());
        }
        let mut scale = 1.0f32;
        loop {
            let (dw, dh) = dest_size(size.Width, size.Height, scale);
            let jpeg = render_page_jpeg(page, dw, dh)?;
            if jpeg.is_empty() {
                return Err("ページ画像を作れませんでした。".into());
            }
            if (jpeg.len() as u64) <= IMAGE_MAX_BYTES {
                return Ok(jpeg);
            }
            if dw <= PDF_MIN_EDGE_PX && dh <= PDF_MIN_EDGE_PX {
                return Err(format!(
                    "ページ画像が大きすぎます（{} MBまで）。",
                    IMAGE_MAX_BYTES / (1024 * 1024)
                ));
            }
            scale *= 0.7;
        }
    }

    pub fn rasterize_pdf(path: &Path) -> Result<RasterizePdf, String> {
        ensure_mta();
        let meta = fs::metadata(path).map_err(|e| format!("PDF を読めません（{e}）。"))?;
        if meta.len() == 0 {
            return Err("PDF が空です。".into());
        }
        if meta.len() > MAX_PDF_BYTES {
            return Err(format!(
                "PDF が大きすぎます（{} MBまで）。",
                MAX_PDF_BYTES / (1024 * 1024)
            ));
        }
        let bytes = fs::read(path).map_err(|e| format!("PDF を読めません（{e}）。"))?;
        let stream = bytes_to_stream(&bytes)?;
        let doc = PdfDocument::LoadFromStreamAsync(&stream)
            .map_err(win_err)?
            .join()
            .map_err(|_| {
                "PDF を開けませんでした。パスワード付きや非対応の形式の可能性があります。"
                    .to_string()
            })?;
        if doc.IsPasswordProtected().unwrap_or(false) {
            return Err("パスワード付き PDF は未対応です。".into());
        }
        let total = doc.PageCount().map_err(win_err)?;
        if total == 0 {
            return Err("ページがありません。".into());
        }
        let take = total.min(MAX_PDF_OCR_PAGES);
        let mut pages = Vec::with_capacity(take as usize);
        for i in 0..take {
            let page = doc.GetPage(i).map_err(win_err)?;
            let jpeg = match render_page_fitting(&page) {
                Ok(j) => j,
                Err(e) => {
                    close_page(&page);
                    return Err(e);
                }
            };
            close_page(&page);
            pages.push(RasterPage {
                page_no: i + 1,
                jpeg,
            });
        }
        if let Ok(c) = doc.cast::<IClosable>() {
            let _ = c.Close();
        }
        Ok(RasterizePdf {
            pages,
            total_pages: total,
            truncated: total > MAX_PDF_OCR_PAGES,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_id_and_title() {
        assert_eq!(pdf_page_paragraph_id(1), "pdf-page:1");
        assert_eq!(pdf_page_paragraph_id(20), "pdf-page:20");
        assert_eq!(pdf_page_title("契約.pdf", 3), "契約.pdf（3ページ目）");
        assert_eq!(
            truncation_warning(20, 31),
            "先頭20ページだけ読み取ります（全31ページ）。"
        );
    }

    #[test]
    fn dest_size_caps_long_edge() {
        let (w, h) = dest_size(2000.0, 1000.0, 1.0);
        assert!(w.max(h) <= PDF_MAX_EDGE_PX as u32);
        assert!(w > 0 && h > 0);
        let (w2, h2) = dest_size(200.0, 100.0, 1.0);
        assert_eq!(w2 / h2, 2);
    }

    fn minimal_blank_pdf() -> Vec<u8> {
        let o1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let o2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
        let o3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n";
        let header = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n";
        let mut data = Vec::new();
        data.extend_from_slice(header);
        let p1 = data.len();
        data.extend_from_slice(o1);
        let p2 = data.len();
        data.extend_from_slice(o2);
        let p3 = data.len();
        data.extend_from_slice(o3);
        let xref_pos = data.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{p1:010} 00000 n \n{p2:010} 00000 n \n{p3:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n"
        );
        data.extend_from_slice(xref.as_bytes());
        data
    }

    #[cfg(windows)]
    #[test]
    fn rasterize_blank_pdf_makes_jpeg() {
        let dir = std::env::temp_dir().join(format!(
            "argos-pdf-win-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blank.pdf");
        std::fs::write(&path, minimal_blank_pdf()).unwrap();
        let out = rasterize_pdf(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let out = out.expect("rasterize blank pdf");
        assert_eq!(out.pages.len(), 1);
        assert_eq!(out.total_pages, 1);
        assert!(!out.truncated);
        assert!(out.pages[0].jpeg.len() > 20);
        assert_eq!(&out.pages[0].jpeg[0..2], &[0xFF, 0xD8]);
    }
}
