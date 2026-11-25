//! PDF manipulation: flattening filled values / signatures onto a template, appending an
//! audit-trail page and merging multiple PDFs (used by the DocuSeal import).

use std::collections::{BTreeMap, HashSet};
use std::io::Write;

use anyhow::{anyhow, Context, Result};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};

use crate::models::Field;

pub enum DrawValue {
    Text(String),
    /// Encoded image (PNG/JPEG/...), drawn fitted & centered into the field box.
    Image(Vec<u8>),
}

pub struct DrawItem<'a> {
    pub field: &'a Field,
    pub value: DrawValue,
}

pub enum AuditLine {
    Title(String),
    Heading(String),
    Kv(String, String),
    Text(String),
    Spacer,
}

const FONT_REGULAR: &[u8] = b"FSigR";
const FONT_BOLD: &[u8] = b"FSigB";

pub fn load(bytes: &[u8]) -> Result<Document> {
    let mut doc = Document::load_mem(bytes).context("could not parse PDF")?;
    if doc.is_encrypted() {
        doc.decrypt("").map_err(|e| anyhow!("PDF is encrypted: {e}"))?;
    }
    Ok(doc)
}

pub fn page_count(bytes: &[u8]) -> Result<usize> {
    Ok(load(bytes)?.get_pages().len())
}

fn save(doc: &mut Document) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    doc.save_to(&mut out).context("could not write PDF")?;
    Ok(out)
}

/// Draws the given items onto the template and optionally appends audit pages.
pub fn render(template: &[u8], items: &[DrawItem], audit: Option<&[AuditLine]>) -> Result<Vec<u8>> {
    let mut doc = load(template)?;
    let pages = doc.get_pages();

    let font_r = add_font(&mut doc, "Helvetica");
    let font_b = add_font(&mut doc, "Helvetica-Bold");

    let mut by_page: BTreeMap<u32, Vec<&DrawItem>> = BTreeMap::new();
    for it in items {
        by_page.entry(it.field.page).or_default().push(it);
    }

    let mut img_counter = 0usize;
    for (page_idx, page_items) in by_page {
        let Some(&page_id) = pages.get(&(page_idx + 1)) else { continue };
        let geo = PageGeometry::read(&doc, page_id)?;
        let mut content = String::new();
        let mut xobjects: Vec<(Vec<u8>, ObjectId)> = Vec::new();

        content.push_str("q\n");
        let [a, b, c, d, e, f] = geo.display_to_user();
        content.push_str(&format!("{} {} {} {} {} {} cm\n", n(a), n(b), n(c), n(d), n(e), n(f)));

        for it in page_items {
            let fld = it.field;
            // box in display space (origin bottom-left, y up)
            let bw = fld.w * geo.dw;
            let bh = fld.h * geo.dh;
            let bx = fld.x * geo.dw;
            let by = geo.dh - (fld.y + fld.h) * geo.dh;
            match &it.value {
                DrawValue::Text(text) => {
                    content.push_str(&text_box(text, bx, by, bw, bh, fld.font_size));
                }
                DrawValue::Image(bytes) => {
                    let Ok((xobj, iw, ih)) = image_xobject(&mut doc, bytes) else {
                        tracing::warn!("skipping undecodable image for field {}", fld.id);
                        continue;
                    };
                    img_counter += 1;
                    let name = format!("ImSig{img_counter}").into_bytes();
                    // fit preserving aspect ratio, centered
                    let scale = (bw / iw as f64).min(bh / ih as f64);
                    let w = iw as f64 * scale;
                    let h = ih as f64 * scale;
                    let x = bx + (bw - w) / 2.0;
                    let y = by + (bh - h) / 2.0;
                    content.push_str(&format!(
                        "q {} 0 0 {} {} {} cm /{} Do Q\n",
                        n(w), n(h), n(x), n(y), String::from_utf8_lossy(&name)
                    ));
                    xobjects.push((name, xobj));
                }
            }
        }
        content.push_str("Q\n");

        let mut fonts = vec![(FONT_REGULAR.to_vec(), font_r)];
        fonts.push((FONT_BOLD.to_vec(), font_b));
        add_resources(&mut doc, page_id, &fonts, &xobjects)?;
        wrap_and_append_content(&mut doc, page_id, content.into_bytes())?;
    }

    if let Some(lines) = audit {
        let first = pages.values().next().copied();
        let (w, h) = match first {
            Some(id) => {
                let g = PageGeometry::read(&doc, id)?;
                if g.dw > g.dh { (595.0, 842.0) } else { (g.dw.max(300.0), g.dh.max(400.0)) }
            }
            None => (595.0, 842.0),
        };
        append_audit_pages(&mut doc, lines, w, h, font_r, font_b)?;
    }

    save(&mut doc)
}

fn n(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.to_string() }
}

fn add_font(doc: &mut Document, base: &str) -> ObjectId {
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => base,
        "Encoding" => "WinAnsiEncoding",
    })
}

struct PageGeometry {
    x0: f64,
    y0: f64,
    w: f64,
    h: f64,
    rotate: i64,
    /// displayed width / height (after rotation)
    dw: f64,
    dh: f64,
}

impl PageGeometry {
    fn read(doc: &Document, page_id: ObjectId) -> Result<Self> {
        let rect = |key: &[u8]| -> Option<[f64; 4]> {
            let obj = inherited(doc, page_id, key)?;
            let arr = doc.dereference(&obj).ok()?.1.as_array().ok()?.clone();
            if arr.len() != 4 {
                return None;
            }
            let mut v = [0.0; 4];
            for (i, o) in arr.iter().enumerate() {
                v[i] = doc.dereference(o).ok()?.1.as_float().ok()? as f64;
            }
            Some(v)
        };
        let media = rect(b"MediaBox").unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let crop = rect(b"CropBox").unwrap_or(media);
        // intersect crop with media (pdf.js does the same)
        let x0 = crop[0].min(crop[2]).max(media[0].min(media[2]));
        let y0 = crop[1].min(crop[3]).max(media[1].min(media[3]));
        let x1 = crop[0].max(crop[2]).min(media[0].max(media[2]));
        let y1 = crop[1].max(crop[3]).min(media[1].max(media[3]));
        let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
        let rotate = inherited(doc, page_id, b"Rotate")
            .and_then(|o| doc.dereference(&o).ok().and_then(|(_, o)| o.as_i64().ok()))
            .unwrap_or(0)
            .rem_euclid(360);
        let rotate = (rotate / 90) * 90;
        let (dw, dh) = if rotate == 90 || rotate == 270 { (h, w) } else { (w, h) };
        Ok(Self { x0, y0, w, h, rotate, dw, dh })
    }

    /// Matrix mapping displayed-page coordinates to PDF user space.
    fn display_to_user(&self) -> [f64; 6] {
        let (w, h, x0, y0) = (self.w, self.h, self.x0, self.y0);
        match self.rotate {
            90 => [0.0, 1.0, -1.0, 0.0, x0 + w, y0],
            180 => [-1.0, 0.0, 0.0, -1.0, x0 + w, y0 + h],
            270 => [0.0, -1.0, 1.0, 0.0, x0, y0 + h],
            _ => [1.0, 0.0, 0.0, 1.0, x0, y0],
        }
    }
}

fn inherited(doc: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return None;
        }
        let node = doc.get_dictionary(current).ok()?;
        if let Ok(v) = node.get(key) {
            if !matches!(doc.dereference(v), Ok((_, Object::Null)) | Err(_)) {
                return Some(v.clone());
            }
        }
        current = node.get(b"Parent").ok()?.as_reference().ok()?;
    }
}

/// Resolves `obj` to a (cloned) direct dictionary.
fn resolve_dict(doc: &Document, obj: &Object) -> Dictionary {
    match doc.dereference(obj) {
        Ok((_, Object::Dictionary(d))) => d.clone(),
        _ => Dictionary::new(),
    }
}

fn add_resources(
    doc: &mut Document,
    page_id: ObjectId,
    fonts: &[(Vec<u8>, ObjectId)],
    xobjects: &[(Vec<u8>, ObjectId)],
) -> Result<()> {
    let mut res = inherited(doc, page_id, b"Resources").map(|o| resolve_dict(doc, &o)).unwrap_or_default();
    let mut font_dict = res.get(b"Font").map(|o| resolve_dict(doc, o)).unwrap_or_default();
    for (name, id) in fonts {
        font_dict.set(name.clone(), Object::Reference(*id));
    }
    res.set("Font", Object::Dictionary(font_dict));
    if !xobjects.is_empty() {
        let mut xd = res.get(b"XObject").map(|o| resolve_dict(doc, o)).unwrap_or_default();
        for (name, id) in xobjects {
            xd.set(name.clone(), Object::Reference(*id));
        }
        res.set("XObject", Object::Dictionary(xd));
    }
    let page = doc.get_dictionary_mut(page_id)?;
    page.set("Resources", Object::Dictionary(res));
    Ok(())
}

/// Wraps the existing page content in q/Q (so its graphics state cannot leak into our
/// overlay) and appends the overlay stream.
fn wrap_and_append_content(doc: &mut Document, page_id: ObjectId, content: Vec<u8>) -> Result<()> {
    let existing: Vec<Object> = {
        let page = doc.get_dictionary(page_id)?;
        match page.get(b"Contents") {
            Ok(Object::Reference(id)) => match doc.get_object(*id) {
                Ok(Object::Array(arr)) => arr.clone(),
                _ => vec![Object::Reference(*id)],
            },
            Ok(Object::Array(arr)) => arr.clone(),
            _ => vec![],
        }
    };
    let q = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let mut overlay = Stream::new(Dictionary::new(), b"\nQ\n".iter().copied().chain(content).collect());
    let _ = overlay.compress();
    let overlay = doc.add_object(overlay);
    let mut list = vec![Object::Reference(q)];
    list.extend(existing);
    list.push(Object::Reference(overlay));
    doc.get_dictionary_mut(page_id)?.set("Contents", list);
    Ok(())
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).expect("in-memory write");
    enc.finish().expect("in-memory write")
}

/// Adds an image XObject (RGB + alpha SMask) and returns (id, width, height).
fn image_xobject(doc: &mut Document, bytes: &[u8]) -> Result<(ObjectId, u32, u32)> {
    let img = image::load_from_memory(bytes)?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let mut alpha = Vec::with_capacity((w * h) as usize);
    let mut has_alpha = false;
    for p in img.pixels() {
        rgb.extend_from_slice(&p.0[..3]);
        alpha.push(p.0[3]);
        has_alpha |= p.0[3] != 255;
    }
    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => w as i64,
        "Height" => h as i64,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
        "Filter" => "FlateDecode",
    };
    if has_alpha {
        let mut smask = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => w as i64,
                "Height" => h as i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
                "Filter" => "FlateDecode",
            },
            zlib(&alpha),
        );
        smask.allows_compression = false;
        let smask_id = doc.add_object(smask);
        dict.set("SMask", Object::Reference(smask_id));
    }
    let mut stream = Stream::new(dict, zlib(&rgb));
    stream.allows_compression = false;
    Ok((doc.add_object(stream), w, h))
}

// ---------------------------------------------------------------------------------------
// Text

/// Encodes text to WinAnsi (characters outside the code page become '?').
pub fn win_ansi(s: &str) -> Vec<u8> {
    s.chars()
        .filter(|c| *c != '\r')
        .map(|c| match c as u32 {
            0x20..=0x7E | 0xA0..=0xFF => c as u32 as u8,
            _ => match c {
                '€' => 0x80, '‚' => 0x82, 'ƒ' => 0x83, '„' => 0x84, '…' => 0x85, '†' => 0x86,
                '‡' => 0x87, 'ˆ' => 0x88, '‰' => 0x89, 'Š' => 0x8A, '‹' => 0x8B, 'Œ' => 0x8C,
                'Ž' => 0x8E, '‘' => 0x91, '’' => 0x92, '“' => 0x93, '”' => 0x94, '•' => 0x95,
                '–' => 0x96, '—' => 0x97, '˜' => 0x98, '™' => 0x99, 'š' => 0x9A, '›' => 0x9B,
                'œ' => 0x9C, 'ž' => 0x9E, 'Ÿ' => 0x9F, '\t' => b' ', '✓' | '✔' | '☑' => b'X',
                _ => b'?',
            },
        })
        .collect()
}

/// Helvetica advance widths (1/1000 em) for ASCII 32..126.
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

fn char_width(b: u8, bold: bool) -> f64 {
    let base = match b {
        32..=126 => HELV[(b - 32) as usize] as f64,
        0xC0..=0xC5 | 0xC8..=0xCB | 0xD2..=0xD6 | 0xD8..=0xDD => 667.0,
        0xE0..=0xFF => 556.0,
        _ => 556.0,
    };
    if bold { base * 1.06 } else { base }
}

pub fn text_width(bytes: &[u8], size: f64, bold: bool) -> f64 {
    bytes.iter().map(|b| char_width(*b, bold)).sum::<f64>() * size / 1000.0
}

fn hex_string(bytes: &[u8]) -> String {
    format!("<{}>", hex::encode(bytes))
}

fn wrap(text: &str, size: f64, max_w: f64) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut cur: Vec<u8> = Vec::new();
        for word in para.split(' ') {
            let w = win_ansi(word);
            let candidate: Vec<u8> = if cur.is_empty() {
                w.clone()
            } else {
                cur.iter().copied().chain([b' ']).chain(w.iter().copied()).collect()
            };
            if text_width(&candidate, size, false) <= max_w || cur.is_empty() {
                cur = candidate;
            } else {
                lines.push(std::mem::take(&mut cur));
                cur = w;
            }
        }
        lines.push(cur);
    }
    lines
}

/// Content-stream snippet drawing `text` into the given box (auto-shrinks & wraps).
fn text_box(text: &str, x: f64, y: f64, w: f64, h: f64, font_size: Option<f32>) -> String {
    let pad = 2.0_f64.min(w * 0.05);
    let avail_w = (w - 2.0 * pad).max(1.0);
    let start = font_size.map(|s| s as f64).unwrap_or_else(|| (h * 0.68).clamp(6.0, 12.0));
    let mut size = start;
    let mut lines;
    loop {
        lines = wrap(text, size, avail_w);
        let total_h = lines.len() as f64 * size * 1.15;
        let widest = lines.iter().map(|l| text_width(l, size, false)).fold(0.0, f64::max);
        if (total_h <= h + 0.5 && widest <= avail_w) || size <= 5.0 {
            break;
        }
        size -= 0.5;
    }
    let mut s = format!("q {} {} {} {} re W n BT /FSigR {} Tf 0 0 0 rg\n", n(x), n(y), n(w), n(h), n(size));
    let line_h = size * 1.15;
    let block_h = lines.len() as f64 * line_h;
    // vertically centered block; baseline offset ~ 0.25 * size from bottom of line box
    let mut base = y + (h + block_h) / 2.0 - line_h + size * 0.27;
    if lines.len() > 1 && block_h > h {
        base = y + h - size;
    }
    for line in &lines {
        s.push_str(&format!("1 0 0 1 {} {} Tm {} Tj\n", n(x + pad), n(base), hex_string(line)));
        base -= line_h;
    }
    s.push_str("ET Q\n");
    s
}

// ---------------------------------------------------------------------------------------
// Audit pages

fn append_audit_pages(
    doc: &mut Document,
    lines: &[AuditLine],
    w: f64,
    h: f64,
    font_r: ObjectId,
    font_b: ObjectId,
) -> Result<()> {
    let root_pages = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let margin = 50.0;
    let max_w = w - 2.0 * margin;
    let mut pages_content: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut y = h - margin;

    let emit = |cur: &mut String, y: &mut f64, pages: &mut Vec<String>, needed: f64| {
        if *y - needed < margin {
            pages.push(std::mem::take(cur));
            *y = h - margin;
        }
    };

    for line in lines {
        match line {
            AuditLine::Title(t) => {
                emit(&mut cur, &mut y, &mut pages_content, 30.0);
                y -= 18.0;
                cur.push_str(&text_line(FONT_BOLD, 16.0, margin, y, &win_ansi(t)));
                y -= 6.0;
                cur.push_str(&format!("0.6 G 0.5 w {} {} m {} {} l S\n", n(margin), n(y), n(w - margin), n(y)));
                y -= 8.0;
            }
            AuditLine::Heading(t) => {
                emit(&mut cur, &mut y, &mut pages_content, 40.0);
                y -= 16.0;
                cur.push_str(&text_line(FONT_BOLD, 11.0, margin, y, &win_ansi(t)));
                y -= 4.0;
            }
            AuditLine::Kv(k, v) => {
                let key_w = 130.0;
                let wrapped = wrap(v, 9.0, max_w - key_w);
                emit(&mut cur, &mut y, &mut pages_content, 12.0 * wrapped.len() as f64);
                y -= 12.0;
                cur.push_str(&text_line(FONT_BOLD, 9.0, margin, y, &win_ansi(k)));
                for (i, l) in wrapped.iter().enumerate() {
                    if i > 0 {
                        y -= 11.0;
                    }
                    cur.push_str(&text_line(FONT_REGULAR, 9.0, margin + key_w, y, l));
                }
            }
            AuditLine::Text(t) => {
                for l in wrap(t, 9.0, max_w) {
                    emit(&mut cur, &mut y, &mut pages_content, 12.0);
                    y -= 12.0;
                    cur.push_str(&text_line(FONT_REGULAR, 9.0, margin, y, &l));
                }
            }
            AuditLine::Spacer => y -= 8.0,
        }
    }
    pages_content.push(cur);

    let resources = dictionary! {
        "Font" => dictionary! { "FSigR" => font_r, "FSigB" => font_b },
    };
    let mut new_ids = Vec::new();
    for content in pages_content {
        let mut stream = Stream::new(Dictionary::new(), content.into_bytes());
        let _ = stream.compress();
        let cid = doc.add_object(stream);
        let pid = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => root_pages,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(w as f32), Object::Real(h as f32)],
            "Contents" => cid,
            "Resources" => resources.clone(),
        });
        new_ids.push(pid);
    }
    let pages = doc.get_dictionary_mut(root_pages)?;
    let mut kids = pages.get(b"Kids").and_then(|k| k.as_array()).cloned().unwrap_or_default();
    let count = pages.get(b"Count").and_then(|c| c.as_i64()).unwrap_or(kids.len() as i64);
    kids.extend(new_ids.iter().map(|id| Object::Reference(*id)));
    pages.set("Kids", kids);
    pages.set("Count", count + new_ids.len() as i64);
    Ok(())
}

fn text_line(font: &[u8], size: f64, x: f64, y: f64, bytes: &[u8]) -> String {
    format!(
        "BT /{} {} Tf 0 g {} {} Td {} Tj ET\n",
        String::from_utf8_lossy(font),
        n(size),
        n(x),
        n(y),
        hex_string(bytes)
    )
}

// ---------------------------------------------------------------------------------------
// Merge

/// Merges several PDFs into one. Returns the merged bytes and, for every input, the index
/// of its first page in the merged document.
pub fn merge(inputs: &[Vec<u8>]) -> Result<(Vec<u8>, Vec<u32>)> {
    if inputs.len() == 1 {
        return Ok((inputs[0].clone(), vec![0]));
    }
    let mut max_id = 1;
    let mut page_order: Vec<ObjectId> = Vec::new();
    let mut pages_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut all_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut offsets = Vec::new();

    for bytes in inputs {
        let mut doc = load(bytes)?;
        for page_id in doc.get_pages().into_values() {
            let mut page = doc.get_dictionary(page_id)?.clone();
            for key in [&b"MediaBox"[..], b"CropBox", b"Resources", b"Rotate"] {
                if let Some(v) = inherited(&doc, page_id, key) {
                    page.set(key.to_vec(), v);
                }
            }
            doc.objects.insert(page_id, Object::Dictionary(page));
        }
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        offsets.push(page_order.len() as u32);
        for page_id in doc.get_pages().into_values() {
            page_order.push(page_id);
            pages_objects.insert(page_id, doc.get_object(page_id)?.clone());
        }
        all_objects.extend(doc.objects);
    }

    let mut out = Document::with_version("1.7");
    for (id, obj) in all_objects {
        match obj.type_name().unwrap_or(b"") {
            b"Catalog" | b"Pages" | b"Page" | b"Outlines" | b"Outline" => {}
            _ => {
                out.objects.insert(id, obj);
            }
        }
    }
    out.max_id = max_id;
    let pages_id = out.new_object_id();
    for (id, obj) in pages_objects {
        if let Object::Dictionary(mut d) = obj {
            d.set("Parent", pages_id);
            out.objects.insert(id, Object::Dictionary(d));
        }
    }
    out.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => page_order.len() as i64,
            "Kids" => page_order.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(),
        }),
    );
    let catalog = out.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    out.trailer.set("Root", catalog);
    out.renumber_objects();
    Ok((save(&mut out)?, offsets))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::models::FieldKind;

    pub(crate) fn blank_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content = doc.add_object(Stream::new(Dictionary::new(), b"0 0 1 rg 10 10 50 50 re f".to_vec()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
        }));
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        save(&mut doc).unwrap()
    }

    fn field(kind: FieldKind) -> Field {
        Field {
            id: "f".into(), name: "f".into(), kind, required: true, section: 0, page: 0,
            x: 0.1, y: 0.1, w: 0.4, h: 0.05, options: vec![], font_size: None, format: None,
            autofill: None, min: None, max: None, placeholder: None, source_ref: None,
        }
    }

    #[test]
    fn render_text_image_audit_and_merge() {
        let pdf = blank_pdf();
        let f1 = field(FieldKind::Text);
        let f2 = field(FieldKind::Signature);
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(20, 10, image::Rgba([0, 0, 0, 128]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let items = vec![
            DrawItem { field: &f1, value: DrawValue::Text("Jürgen Müller – 42 €".into()) },
            DrawItem { field: &f2, value: DrawValue::Image(png) },
        ];
        let audit = vec![AuditLine::Title("Audit".into()), AuditLine::Kv("Key".into(), "Value".into())];
        let out = render(&pdf, &items, Some(&audit)).unwrap();
        assert_eq!(page_count(&out).unwrap(), 2);
        let (merged, offsets) = merge(&[out.clone(), pdf.clone()]).unwrap();
        assert_eq!(page_count(&merged).unwrap(), 3);
        assert_eq!(offsets, vec![0, 2]);
        std::fs::write(std::env::temp_dir().join("pdfsign-test.pdf"), &out).unwrap();
    }
}
