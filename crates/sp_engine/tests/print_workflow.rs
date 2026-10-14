//! Printing workflow driven through the JSON control protocol: page setup, print titles,
//! header/footer, then PDF export; the PDF is checked structurally and page by page.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use serde_json::{Value as Json, json};
use sp_engine::Session;
use sp_engine::control::handle_line;
use std::path::PathBuf;

fn exec(s: &mut Session, id: &str, params: Json) -> Json {
    let req = json!({"op": "exec", "id": id, "params": params});
    let (reply, _) = handle_line(s, &req.to_string());
    let r: Json = serde_json::from_str(&reply).expect("reply is JSON");
    assert_eq!(r["ok"], true, "{id} {params}: {r}");
    r["result"].clone()
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The decoded content stream of every page, in page order (our writer emits
/// `/Contents n 0 R` on each page object and uncompressed streams).
fn page_streams(pdf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = find(pdf, b"<< /Type /Page /Parent", at) {
        let c = find(pdf, b"/Contents ", p).unwrap() + 10;
        let e = find(pdf, b" 0 R", c).unwrap();
        let n = std::str::from_utf8(&pdf[c..e]).unwrap();
        let obj = find(pdf, format!("\n{n} 0 obj\n").as_bytes(), 0).unwrap();
        let s = find(pdf, b"stream\n", obj).unwrap() + 7;
        let end = find(pdf, b"\nendstream", s).unwrap();
        out.push(String::from_utf8_lossy(&pdf[s..end]).into_owned());
        at = p + 1;
    }
    out
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sp_print_wf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn page_setup_titles_header_footer_to_pdf() {
    let mut s = Session::new();
    for (c, h) in ["Region", "Units", "Price", "Revenue"].iter().enumerate() {
        let cell = format!("{}1", ["A", "B", "C", "D"][c]);
        exec(&mut s, "cell.set", json!({"cell": cell, "input": h}));
    }
    for r in 2..=200 {
        exec(
            &mut s,
            "cell.set",
            json!({"cell": format!("A{r}"), "input": format!("Region {r}")}),
        );
        exec(
            &mut s,
            "cell.set",
            json!({"cell": format!("B{r}"), "input": format!("{}", r * 7)}),
        );
        exec(
            &mut s,
            "cell.set",
            json!({"cell": format!("C{r}"), "input": "12.5"}),
        );
        exec(
            &mut s,
            "cell.set",
            json!({"cell": format!("D{r}"), "input": format!("=B{r}*C{r}")}),
        );
    }
    exec(
        &mut s,
        "home.number.format",
        json!({"range": "C2:D200", "code": "$#,##0.00"}),
    );
    exec(&mut s, "home.font.bold", json!({"range": "A1:D1"}));
    // Page setup: A4 landscape, narrow margins, print titles, a manual break, headings.
    exec(&mut s, "layout.size", json!({"size": "a4"}));
    exec(
        &mut s,
        "layout.orientation",
        json!({"orientation": "landscape"}),
    );
    exec(&mut s, "layout.margins", json!({"preset": "narrow"}));
    exec(&mut s, "layout.print_titles", json!({"rows": "1:1"}));
    exec(&mut s, "layout.page_break.insert", json!({"cell": "A50"}));
    exec(&mut s, "layout.headings_print", json!({"on": true}));
    let hf = exec(
        &mut s,
        "insert.header_footer",
        json!({"header": "&LQuarterly report&R&A", "footer": "&CPage &P of &N"}),
    );
    assert_eq!(hf["header"], "&LQuarterly report&R&A");

    let path = tmp("report.pdf");
    let out = exec(
        &mut s,
        "file.export.pdf",
        json!({"path": path.display().to_string()}),
    );
    let pages = out["pages"].as_u64().unwrap() as usize;
    // Landscape A4 fits ~36 rows a page; the manual break at row 50 adds a page.
    assert!(pages >= 4, "{out}");
    assert_eq!(
        out["sheets"][0]["paper"][0].as_f64().unwrap().round(),
        842.0
    );

    let pdf = std::fs::read(&path).unwrap();
    let sum = sp_io::pdf::validate(&pdf).unwrap_or_else(|e| panic!("invalid PDF: {e}"));
    assert_eq!(sum.pages, pages);
    let streams = page_streams(&pdf);
    assert_eq!(streams.len(), pages);
    for (i, st) in streams.iter().enumerate() {
        assert!(
            st.contains("(Quarterly report) Tj"),
            "page {} lacks the header",
            i + 1
        );
        assert!(st.contains("(Sheet1) Tj"), "page {} lacks &A", i + 1);
        assert!(
            st.contains(&format!("(Page {} of {pages}) Tj", i + 1)),
            "page {} footer",
            i + 1
        );
        // Print titles: the heading row repeats on every page.
        assert!(
            st.contains("(Revenue) Tj"),
            "page {} lacks the title row",
            i + 1
        );
    }
    // Number formats are applied to printed text; the manual break at row 50 ends a page.
    assert!(streams[0].contains("($12.50) Tj"));
    assert!(streams[0].contains("($175.00) Tj"));
    let k = streams
        .iter()
        .position(|t| t.contains("(Region 49) Tj"))
        .unwrap();
    assert!(!streams[k].contains("(Region 50) Tj"));
    assert!(streams[k + 1].contains("(Region 50) Tj"));
    assert!(!streams[k + 1].contains("(Region 48) Tj"));

    // file.print paginates identically (headless: preview state + optional PDF).
    let p = exec(&mut s, "file.print", json!({}));
    assert_eq!(p["pages"].as_u64().unwrap() as usize, pages);
    assert!(s.view.print.preview);
    let _ = std::fs::remove_file(&path);
}
