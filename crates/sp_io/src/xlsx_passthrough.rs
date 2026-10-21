//! Pass-through of package content we don't model.
//!
//! On load, every relationship / root child element / part that SheetPro does not model is
//! captured verbatim into `sp_model::passthrough::Passthrough` (on the workbook, or on the
//! sheet that references it); on save it is re-emitted with consistent relationship ids,
//! part names and `[Content_Types].xml` overrides.
//!
//! **Hook for new first-class features.** When a feature becomes modelled (conditional
//! formatting, tables, charts…), add its worksheet/workbook element name to
//! [`HANDLED_SHEET_ELEMENTS`] / [`HANDLED_WORKBOOK_ELEMENTS`] and, for parts that are only
//! reachable through an implicit relationship (not an `r:id` in a handled element), its
//! relationship type to [`HANDLED_SHEET_REL_TYPES`] / [`HANDLED_WORKBOOK_REL_TYPES`]. The
//! loader then stops capturing them; relationships referenced only from handled elements are
//! dropped automatically. As a safety net the writer also skips any captured element whose
//! name it has just emitted itself.
//!
//! Rules that keep edits safe:
//! - Parts referenced from one sheet travel with that sheet (delete drops them, reorder and
//!   rename need nothing). Parts reachable from the workbook, or from several sheets, live
//!   on the workbook.
//! - Part names are re-allocated on save when they collide with a part we generate or with a
//!   part of another owner (a duplicated sheet); a duplicated sheet's table parts are dropped
//!   (table names must be unique) together with its `tableParts` element.
//! - Relationships whose internal target is not written are dropped, and so is every
//!   captured element that references a dropped relationship id.
//! - Formulas inside captured parts (chart series, pivot sources) are not rewritten.

use crate::IoError;
use crate::xml::esc;
use quick_xml::Reader;
use quick_xml::events::Event;
use sp_model::passthrough::{OpaqueSheet, Passthrough, RawElement, RawPart, RawRel};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Seek};
use std::sync::Arc;

pub const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
pub const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// ECMA-376 CT_Worksheet child sequence.
pub const WORKSHEET_ORDER: &[&str] = &[
    "sheetPr",
    "dimension",
    "sheetViews",
    "sheetFormatPr",
    "cols",
    "sheetData",
    "sheetCalcPr",
    "sheetProtection",
    "protectedRanges",
    "scenarios",
    "autoFilter",
    "sortState",
    "dataConsolidate",
    "customSheetViews",
    "mergeCells",
    "phoneticPr",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "customProperties",
    "cellWatches",
    "ignoredErrors",
    "smartTags",
    "drawing",
    "legacyDrawing",
    "legacyDrawingHF",
    "drawingHF",
    "picture",
    "oleObjects",
    "controls",
    "webPublishItems",
    "tableParts",
    "extLst",
];

/// ECMA-376 CT_Workbook child sequence.
pub const WORKBOOK_ORDER: &[&str] = &[
    "fileVersion",
    "fileSharing",
    "workbookPr",
    "workbookProtection",
    "bookViews",
    "sheets",
    "functionGroups",
    "externalReferences",
    "definedNames",
    "calcPr",
    "oleSize",
    "customWorkbookViews",
    "pivotCaches",
    "smartTagPr",
    "smartTagTypes",
    "webPublishing",
    "fileRecoveryPr",
    "webPublishObjects",
    "extLst",
];

/// Worksheet children the writer generates from the model (never captured).
pub const HANDLED_SHEET_ELEMENTS: &[&str] = &[
    "sheetPr",
    "dimension",
    "sheetViews",
    "sheetFormatPr",
    "cols",
    "sheetData",
    "sheetProtection",
    "protectedRanges",
    "autoFilter",
    "mergeCells",
    "conditionalFormatting",
    "dataValidations",
    "hyperlinks",
    "printOptions",
    "pageMargins",
    "pageSetup",
    "headerFooter",
    "rowBreaks",
    "colBreaks",
    "tableParts",
];

/// Workbook children the writer generates from the model.
pub const HANDLED_WORKBOOK_ELEMENTS: &[&str] = &[
    "workbookProtection",
    "bookViews",
    "sheets",
    "definedNames",
    "calcPr",
];

/// Sheet relationship types (last URI segment) that are modelled even when no element
/// references them: legacy comments (notes) and the threaded comments tied to them.
pub const HANDLED_SHEET_REL_TYPES: &[&str] = &["comments", "threadedComment", "table"];

/// Workbook relationship types (last URI segment) the writer generates or deliberately drops
/// (`calcChain` is rebuilt by recalculation; rich values only back `vm` cell attributes,
/// which are not written).
pub const HANDLED_WORKBOOK_REL_TYPES: &[&str] = &[
    "worksheet",
    "chartsheet",
    "dialogsheet",
    "macrosheet",
    "xlMacrosheet",
    "xlIntlMacrosheet",
    "styles",
    "sharedStrings",
    "sheetMetadata",
    "calcChain",
];

/// Package relationship types the writer generates.
const HANDLED_PACKAGE_REL_TYPES: &[&str] =
    &["officeDocument", "core-properties", "extended-properties"];

/// Content types of parts the writer generates; never captured as opaque parts.
const GENERATED_CONTENT_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
    "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
    "application/vnd.ms-excel.template.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheetMetadata+xml",
    "application/vnd.openxmlformats-package.core-properties+xml",
    "application/vnd.openxmlformats-officedocument.extended-properties+xml",
];

pub const TABLE_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml";

/// Caps on captured content (zip-bomb guards on top of the per-part cap).
const MAX_PARTS: usize = 20_000;
const MAX_TOTAL_BYTES: u64 = 1 << 30;
const MAX_PART_BYTES: u64 = 1 << 30;

/// Last segment of a relationship type URI (`…/relationships/drawing` → `drawing`).
pub fn rel_kind(t: &str) -> &str {
    t.rsplit('/').next().unwrap_or(t)
}

fn workbook_rel_handled(t: &str) -> bool {
    let k = rel_kind(t);
    // Rich data parts (rdRichValue*, rdArray, richValueRel, rdSupportingPropertyBag*).
    HANDLED_WORKBOOK_REL_TYPES.contains(&k) || k.starts_with("rd") || k.starts_with("richValue")
}

pub fn is_opaque_sheet_rel(t: &str) -> bool {
    matches!(
        rel_kind(t),
        "chartsheet" | "dialogsheet" | "macrosheet" | "xlMacrosheet" | "xlIntlMacrosheet"
    )
}

/// The `.rels` part holding `part`'s relationships.
pub fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn dir_of(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(d, _)| d)
}

// ---------------------------------------------------------------------------------------
// Package primitives

/// `[Content_Types].xml`.
#[derive(Debug, Default, Clone)]
pub struct ContentTypes {
    pub defaults: HashMap<String, String>,
    pub overrides: HashMap<String, String>,
}

impl ContentTypes {
    pub fn parse(xml: &str) -> Result<Self, IoError> {
        let root = crate::xml::parse_dom(xml)?;
        let mut ct = ContentTypes::default();
        for n in &root.children {
            match (n.name.as_str(), n.attr("ContentType")) {
                ("Default", Some(t)) => {
                    if let Some(e) = n.attr("Extension") {
                        ct.defaults.insert(e.to_ascii_lowercase(), t.to_string());
                    }
                }
                ("Override", Some(t)) => {
                    if let Some(p) = n.attr("PartName") {
                        ct.overrides.insert(
                            p.trim_start_matches('/').to_ascii_lowercase(),
                            t.to_string(),
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(ct)
    }

    /// Content type of a part (`None` when the package declares none).
    pub fn get(&self, path: &str) -> Option<&str> {
        let p = path.trim_start_matches('/').to_ascii_lowercase();
        if let Some(t) = self.overrides.get(&p) {
            return Some(t);
        }
        let ext = p.rsplit_once('.').map(|(_, e)| e)?;
        self.defaults.get(ext).map(String::as_str)
    }
}

/// Parse a `.rels` part; internal targets are resolved against `base_dir`.
pub fn parse_raw_rels(xml: &str, base_dir: &str) -> Result<Vec<RawRel>, IoError> {
    let root = crate::xml::parse_dom(xml)?;
    Ok(root
        .children_named("Relationship")
        .take(100_000)
        .filter_map(|r| {
            let id = r.attr("Id")?.to_string();
            let target = r.attr("Target")?;
            let external = r.attr("TargetMode") == Some("External");
            let target = if external {
                target.to_string()
            } else {
                crate::xlsx_read::resolve_target_pub(base_dir, target)
            };
            Some(RawRel {
                id,
                rel_type: r.attr("Type").unwrap_or("").to_string(),
                target,
                external,
            })
        })
        .collect())
}

fn read_bytes<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    cap: u64,
) -> Result<Option<Vec<u8>>, IoError> {
    let Some(real) = crate::entry_name(zip, name) else {
        return Ok(None);
    };
    let file = match zip.by_name(&real) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(IoError::Zip(e.to_string())),
    };
    let mut buf = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| IoError::Format(format!("{name}: {e}")))?;
    if buf.len() as u64 > cap {
        return Err(IoError::Format(format!("{name} exceeds size limit")));
    }
    Ok(Some(buf))
}

pub fn read_raw_rels<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    part: &str,
) -> Result<Vec<RawRel>, IoError> {
    match read_bytes(zip, &rels_path(part), 64 << 20)? {
        Some(b) => parse_raw_rels(&String::from_utf8_lossy(&b), dir_of(part)),
        None => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------------------------------
// XML scanning

/// Root attributes of a part plus its root's children.
pub type Scanned<'a> = (Vec<(String, String)>, Vec<Child<'a>>);

/// One child element of a part's root, as a slice of the source text.
#[derive(Debug, Clone)]
pub struct Child<'a> {
    pub qname: &'a str,
    pub xml: &'a str,
}

impl Child<'_> {
    pub fn local(&self) -> &str {
        crate::xml::local(self.qname)
    }
}

/// Top-level elements of `xml`. With `skip_root`, the first start tag is the root: its
/// attributes are returned and its children listed (an unclosed root is fine). Without,
/// the fragment's own top-level elements are listed.
pub fn scan_children(xml: &str, skip_root: bool) -> Result<Scanned<'_>, IoError> {
    // Positions below index `xml`; the reader would skip a BOM and count without it.
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut root_attrs = Vec::new();
    let mut out = Vec::new();
    let base: usize = usize::from(skip_root);
    let mut depth: usize = 0;
    let mut start_at = 0usize;
    let mut start_name: &str = "";
    loop {
        let before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let ev = reader
            .read_event()
            .map_err(|e| IoError::Xml(e.to_string()))?;
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match ev {
            Event::Start(e) => {
                if skip_root && depth == 0 {
                    root_attrs = raw_attrs(&e);
                } else if depth == base {
                    start_at = before;
                    start_name = qname_in(xml, before);
                }
                depth += 1;
                if depth > 512 {
                    return Err(IoError::Xml("document nested too deeply".into()));
                }
            }
            Event::Empty(e) => {
                if skip_root && depth == 0 {
                    root_attrs = raw_attrs(&e);
                } else if depth == base {
                    out.push(Child {
                        qname: qname_in(xml, before),
                        xml: xml.get(before..after).unwrap_or(""),
                    });
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                if depth == base && !(skip_root && depth == 0) {
                    out.push(Child {
                        qname: start_name,
                        xml: xml.get(start_at..after).unwrap_or(""),
                    });
                } else if skip_root && depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((root_attrs, out))
}

/// The tag name of the start tag at byte `at` (`<name …`).
fn qname_in(xml: &str, at: usize) -> &str {
    let s = xml.get(at..).unwrap_or("");
    let s = s.trim_start();
    let s = s.strip_prefix('<').unwrap_or(s);
    let end = s
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(s.len());
    s.get(..end).unwrap_or("")
}

fn raw_attrs(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .map(|a| (a.key.as_ref().to_string(), a.value.to_string()))
        .collect()
}

/// Prefixes bound to `ns` by the root's `xmlns:*` attributes (`""` for the default).
fn prefixes_for(root_attrs: &[(String, String)], ns: &str) -> Vec<String> {
    root_attrs
        .iter()
        .filter(|(_, v)| v == ns)
        .filter_map(|(k, _)| {
            if k == "xmlns" {
                Some(String::new())
            } else {
                k.strip_prefix("xmlns:").map(str::to_string)
            }
        })
        .collect()
}

/// Relationship ids referenced (`r:id`, `r:embed`, …) anywhere inside `xml`.
pub fn rel_ids_in(xml: &str, rel_prefixes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut local_prefixes: Vec<String> = Vec::new();
    while let Ok(ev) = reader.read_event() {
        let e = match ev {
            Event::Start(e) | Event::Empty(e) => e,
            Event::Eof => break,
            _ => continue,
        };
        let attrs = raw_attrs(&e);
        for (k, v) in &attrs {
            if v == REL_NS
                && let Some(p) = k.strip_prefix("xmlns:")
            {
                local_prefixes.push(p.to_string());
            }
        }
        for (k, v) in attrs {
            if let Some((p, _)) = k.split_once(':')
                && (rel_prefixes.iter().any(|x| x == p) || local_prefixes.iter().any(|x| x == p))
                && p != "xmlns"
            {
                out.push(v);
            }
        }
    }
    out
}

/// Is `qname` a main-namespace (schema) element?
fn is_main(qname: &str, main_prefixes: &[String]) -> bool {
    match qname.split_once(':') {
        None => main_prefixes.iter().any(String::is_empty) || main_prefixes.is_empty(),
        Some((p, _)) => main_prefixes.iter().any(|x| x == p),
    }
}

// ---------------------------------------------------------------------------------------
// Reading

/// Load-time capture state shared across the workbook and its sheets.
pub struct Capture {
    ct: ContentTypes,
    cache: HashMap<String, RawPart>,
    total_bytes: u64,
    /// Parts owned by the workbook (reachable from workbook / package relationships).
    wb_parts: Vec<String>,
    /// Per sheet (in workbook order): the parts reachable from it.
    sheet_parts: Vec<Vec<String>>,
}

impl Capture {
    pub fn new(ct: ContentTypes) -> Self {
        Capture {
            ct,
            cache: HashMap::new(),
            total_bytes: 0,
            wb_parts: Vec::new(),
            sheet_parts: Vec::new(),
        }
    }

    /// Parts reachable from `rels` (transitively), excluding generated parts and parts
    /// already owned by the workbook.
    fn closure<R: Read + Seek>(
        &mut self,
        zip: &mut zip::ZipArchive<R>,
        rels: &[RawRel],
    ) -> Result<Vec<String>, IoError> {
        let wb_owned: HashSet<String> = self.wb_parts.iter().cloned().collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        let mut queue: Vec<String> = rels
            .iter()
            .filter(|r| !r.external)
            .map(|r| r.target.clone())
            .collect();
        queue.reverse();
        while let Some(path) = queue.pop() {
            if !seen.insert(path.clone()) || wb_owned.contains(&path) {
                continue;
            }
            if !self.cache.contains_key(&path) {
                if self.cache.len() >= MAX_PARTS {
                    return Err(IoError::Format("too many parts".into()));
                }
                let ct = self.ct.get(&path).unwrap_or("").to_string();
                if GENERATED_CONTENT_TYPES.contains(&ct.as_str()) || path.ends_with(".rels") {
                    continue;
                }
                let Some(data) = read_bytes(zip, &path, MAX_PART_BYTES)? else {
                    continue;
                };
                self.total_bytes = self.total_bytes.saturating_add(data.len() as u64);
                if self.total_bytes > MAX_TOTAL_BYTES {
                    return Err(IoError::Format("unmodelled parts exceed size limit".into()));
                }
                let prels = read_raw_rels(zip, &path)?;
                let content_type = if ct.is_empty() {
                    "application/octet-stream".to_string()
                } else {
                    ct
                };
                self.cache.insert(
                    path.clone(),
                    RawPart {
                        path: path.clone(),
                        content_type,
                        data: Arc::from(data),
                        rels: prels,
                    },
                );
            }
            if let Some(p) = self.cache.get(&path) {
                for r in p.rels.iter().rev().filter(|r| !r.external) {
                    queue.push(r.target.clone());
                }
                out.push(path);
            }
        }
        Ok(out)
    }

    /// Workbook-level capture: unmodelled `workbook.xml` children, workbook and package
    /// relationships, and every part reachable from them.
    pub fn workbook<R: Read + Seek>(
        &mut self,
        zip: &mut zip::ZipArchive<R>,
        wb_xml: &str,
        wb_rels: &[RawRel],
    ) -> Result<Passthrough, IoError> {
        let mut pt = Passthrough::default();
        let (root_attrs, children) = scan_children(wb_xml, true)?;
        let main = prefixes_for(&root_attrs, MAIN_NS);
        let mut after = String::new();
        for c in &children {
            let main_el = is_main(c.qname, &main);
            if main_el && HANDLED_WORKBOOK_ELEMENTS.contains(&c.local()) {
                after = c.local().to_string();
                continue;
            }
            pt.elements.push(RawElement {
                name: c.qname.to_string(),
                after: after.clone(),
                xml: c.xml.to_string(),
            });
            if main_el && WORKBOOK_ORDER.contains(&c.local()) {
                after = c.local().to_string();
            }
        }
        pt.root_attrs = keep_root_attrs(root_attrs);
        pt.rels = wb_rels
            .iter()
            .filter(|r| !workbook_rel_handled(&r.rel_type))
            .cloned()
            .collect();
        pt.package_rels = read_raw_rels(zip, "")
            .or_else(|_| Ok::<_, IoError>(Vec::new()))?
            .into_iter()
            .filter(|r| !HANDLED_PACKAGE_REL_TYPES.contains(&rel_kind(&r.rel_type)))
            .collect();
        let mut start = pt.rels.clone();
        start.extend(pt.package_rels.iter().cloned());
        self.wb_parts = self.closure(zip, &start)?;
        Ok(pt)
    }

    /// Worksheet capture: unmodelled root children (with their namespaces) and the
    /// relationships / parts they need. `head` / `tail` are the text around `sheetData`.
    pub fn worksheet<R: Read + Seek>(
        &mut self,
        zip: &mut zip::ZipArchive<R>,
        path: &str,
        head: &str,
        tail: &str,
    ) -> Result<Passthrough, IoError> {
        let mut pt = Passthrough::default();
        let rels = read_raw_rels(zip, path)?;
        let (root_attrs, head_children) = scan_children(head, true)?;
        let tail_body = match tail.rfind("</") {
            Some(i) => tail.get(..i).unwrap_or(""),
            None => tail,
        };
        let (_, tail_children) = scan_children(tail_body, false)?;
        let main = prefixes_for(&root_attrs, MAIN_NS);
        let relp = prefixes_for(&root_attrs, REL_NS);
        let mut referenced_all: HashSet<String> = HashSet::new();
        let mut referenced_kept: HashSet<String> = HashSet::new();
        let mut after = String::new();
        for c in head_children.iter().chain(tail_children.iter()) {
            let main_el = is_main(c.qname, &main);
            let ids = rel_ids_in(c.xml, &relp);
            referenced_all.extend(ids.iter().cloned());
            let handled = main_el && sheet_element_handled(c.local(), &rels);
            if !handled {
                referenced_kept.extend(ids);
                pt.elements.push(RawElement {
                    name: c.qname.to_string(),
                    after: after.clone(),
                    xml: c.xml.to_string(),
                });
            }
            if main_el && WORKSHEET_ORDER.contains(&c.local()) {
                after = c.local().to_string();
            }
        }
        pt.root_attrs = keep_root_attrs(root_attrs);
        pt.rels = rels
            .into_iter()
            .filter(|r| {
                referenced_kept.contains(&r.id)
                    || (!referenced_all.contains(&r.id)
                        && !HANDLED_SHEET_REL_TYPES.contains(&rel_kind(&r.rel_type)))
            })
            .collect();
        let parts = self.closure(zip, &pt.rels)?;
        self.sheet_parts.push(parts);
        Ok(pt)
    }

    /// A sheet that is not a worksheet (chart sheet…): the part itself, verbatim.
    pub fn opaque_sheet<R: Read + Seek>(
        &mut self,
        zip: &mut zip::ZipArchive<R>,
        path: &str,
        rel_type: &str,
    ) -> Result<Option<Passthrough>, IoError> {
        let Some(data) = read_bytes(zip, path, MAX_PART_BYTES)? else {
            return Ok(None);
        };
        let rels = read_raw_rels(zip, path)?;
        let content_type = self
            .ct
            .get(path)
            .unwrap_or("application/octet-stream")
            .to_string();
        let parts = self.closure(zip, &rels)?;
        self.sheet_parts.push(parts);
        Ok(Some(Passthrough {
            sheet_part: Some(OpaqueSheet {
                rel_type: rel_type.to_string(),
                part: RawPart {
                    path: path.to_string(),
                    content_type,
                    data: Arc::from(data),
                    rels,
                },
            }),
            ..Passthrough::default()
        }))
    }

    /// A sheet that captured nothing (keeps the per-sheet bookkeeping aligned).
    pub fn skip_sheet(&mut self) {
        self.sheet_parts.push(Vec::new());
    }

    /// Hand captured parts to their owners: parts reachable from one sheet only go to that
    /// sheet; parts reachable from the workbook or from several sheets go to the workbook.
    pub fn finish(mut self, wb: &mut sp_model::Workbook) {
        let mut count: HashMap<&str, usize> = HashMap::new();
        for list in &self.sheet_parts {
            for p in list {
                *count.entry(p.as_str()).or_default() += 1;
            }
        }
        let shared: Vec<String> = self
            .sheet_parts
            .iter()
            .flatten()
            .filter(|p| count.get(p.as_str()).copied().unwrap_or(0) > 1)
            .cloned()
            .collect();
        let mut wb_list = std::mem::take(&mut self.wb_parts);
        for p in shared {
            if !wb_list.contains(&p) {
                wb_list.push(p);
            }
        }
        let wb_set: HashSet<String> = wb_list.iter().cloned().collect();
        for p in &wb_list {
            if let Some(part) = self.cache.get(p) {
                wb.passthrough.parts.push(part.clone());
            }
        }
        for (sheet, list) in wb.sheets.iter_mut().zip(&self.sheet_parts) {
            for p in list {
                if !wb_set.contains(p)
                    && let Some(part) = self.cache.get(p)
                {
                    sheet.passthrough.parts.push(part.clone());
                }
            }
        }
    }
}

/// Worksheet elements the model covers. `legacyDrawing` is modelled only for sheets with
/// notes (it then carries them); otherwise (form controls…) it is passed through.
fn sheet_element_handled(local: &str, rels: &[RawRel]) -> bool {
    if local == "legacyDrawing" {
        return rels.iter().any(|r| rel_kind(&r.rel_type) == "comments");
    }
    HANDLED_SHEET_ELEMENTS.contains(&local)
}

/// Root attributes worth carrying: namespace declarations and `mc:Ignorable`.
fn keep_root_attrs(attrs: Vec<(String, String)>) -> Vec<(String, String)> {
    attrs
        .into_iter()
        .filter(|(k, _)| k.starts_with("xmlns:") || k.ends_with(":Ignorable"))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Writing

/// Save-time state: part-name allocation and the parts to emit.
#[derive(Default)]
pub struct Emit {
    /// Lower-cased part names already taken (generated parts and placed opaque parts).
    used: HashSet<String>,
    /// Old → new part name, workbook-owned parts.
    wb_map: HashMap<String, String>,
    /// Per sheet: old → new part name.
    sheet_maps: Vec<HashMap<String, String>>,
    /// Per sheet: the duplicated-table parts were dropped.
    pub tables_dropped: Vec<bool>,
    /// Parts to write: (new name, part, owner sheet index or None for the workbook).
    parts: Vec<(String, RawPart, Option<usize>)>,
    /// Opaque sheet parts by sheet index: new name.
    pub opaque_paths: HashMap<usize, String>,
    /// Paths the writer generates itself (valid rel targets).
    generated: HashSet<String>,
}

impl Emit {
    /// `reserved`: part names the writer may produce; `generated`: those it surely writes
    /// (valid targets for captured relationships).
    pub fn plan(wb: &sp_model::Workbook, reserved: &[String], generated: &[String]) -> Emit {
        let mut e = Emit::default();
        for g in reserved {
            e.used.insert(g.to_ascii_lowercase());
        }
        for g in generated {
            e.used.insert(g.to_ascii_lowercase());
            e.generated.insert(g.clone());
        }
        for p in &wb.passthrough.parts {
            let new = e.alloc(&p.path);
            e.wb_map.insert(p.path.clone(), new.clone());
            e.parts.push((new, p.clone(), None));
        }
        let mut owners: HashMap<String, usize> = HashMap::new();
        for (i, sh) in wb.sheets.iter().enumerate() {
            let mut map = HashMap::new();
            let pt = &sh.passthrough;
            // A duplicated sheet carries copies of the same table parts: table names must be
            // unique, so the copy loses its tables.
            let dup_tables = pt.parts.iter().any(|p| {
                p.content_type == TABLE_CONTENT_TYPE && owners.get(&p.path).is_some_and(|o| *o != i)
            });
            for p in &pt.parts {
                owners.entry(p.path.clone()).or_insert(i);
                if dup_tables && p.content_type == TABLE_CONTENT_TYPE {
                    continue;
                }
                let new = e.alloc(&p.path);
                map.insert(p.path.clone(), new.clone());
                e.parts.push((new, p.clone(), Some(i)));
            }
            if let Some(os) = &pt.sheet_part {
                let new = e.alloc(&os.part.path);
                map.insert(os.part.path.clone(), new.clone());
                e.opaque_paths.insert(i, new);
            }
            e.sheet_maps.push(map);
            e.tables_dropped.push(dup_tables);
        }
        e
    }

    fn alloc(&mut self, path: &str) -> String {
        if self.used.insert(path.to_ascii_lowercase()) {
            return path.to_string();
        }
        let (stem, ext) = match path.rsplit_once('.') {
            Some((s, x)) if !x.contains('/') => (s, format!(".{x}")),
            _ => (path, String::new()),
        };
        for k in 2..100_000u32 {
            let cand = format!("{stem}_{k}{ext}");
            if self.used.insert(cand.to_ascii_lowercase()) {
                return cand;
            }
        }
        // Practically unreachable (caps on part counts); fall back to a unique-ish name.
        format!("{stem}_x{ext}")
    }

    /// Where an internal target ends up for `owner` (None: not written).
    pub fn resolve(&self, owner: Option<usize>, target: &str) -> Option<String> {
        if let Some(i) = owner
            && let Some(m) = self.sheet_maps.get(i)
            && let Some(n) = m.get(target)
        {
            return Some(n.clone());
        }
        if let Some(n) = self.wb_map.get(target) {
            return Some(n.clone());
        }
        self.generated.contains(target).then(|| target.to_string())
    }

    /// Relationships for `owner`, filtered to resolvable ones, targets made absolute.
    pub fn rels_for(&self, owner: Option<usize>, rels: &[RawRel]) -> Vec<RawRel> {
        rels.iter()
            .filter_map(|r| {
                if r.external {
                    return Some(r.clone());
                }
                let t = self.resolve(owner, &r.target)?;
                Some(RawRel {
                    target: format!("/{t}"),
                    ..r.clone()
                })
            })
            .collect()
    }

    /// Opaque parts (and their `.rels`) to write, plus content-type overrides.
    pub fn files(&self) -> Vec<(String, Arc<[u8]>, String)> {
        let mut out = Vec::new();
        for (path, part, owner) in &self.parts {
            out.push((path.clone(), part.data.clone(), part.content_type.clone()));
            let rels = self.rels_for(*owner, &part.rels);
            if !part.rels.is_empty() {
                out.push((
                    rels_path(path),
                    Arc::from(rels_xml(&rels).into_bytes()),
                    String::new(),
                ));
            }
        }
        out
    }
}

/// A `.rels` part.
pub fn rels_xml(rels: &[RawRel]) -> String {
    let mut x = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PKG_REL_NS}">"#
    );
    for r in rels {
        let _ = write!(
            x,
            r#"<Relationship Id="{}" Type="{}" Target="{}"{}/>"#,
            esc(&r.id),
            esc(&r.rel_type),
            esc(&r.target),
            if r.external {
                r#" TargetMode="External""#
            } else {
                ""
            }
        );
    }
    x.push_str("</Relationships>");
    x
}

/// Extra attributes for our root element: the captured namespace declarations (minus the
/// default and `r` prefixes we declare ourselves) and `mc:Ignorable`.
pub fn root_attrs_xml(pt: &Passthrough) -> String {
    let mut x = String::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (k, v) in &pt.root_attrs {
        if k == "xmlns" || k == "xmlns:r" || !seen.insert(k.as_str()) {
            continue;
        }
        let _ = write!(x, r#" {k}="{}""#, esc(v));
    }
    x
}

/// Merge generated elements (`known`: local name + XML) with captured ones into schema
/// order. Captured elements whose name was generated, or that reference a relationship id
/// not in `valid_ids`, are skipped. Returns the ordered XML pieces and the relationship ids
/// the kept captured elements reference.
pub fn merge_elements<'a>(
    order: &[&str],
    known: &'a [(String, String)],
    pt: &'a Passthrough,
    skip: &dyn Fn(&RawElement) -> bool,
    valid_ids: &HashSet<String>,
) -> (Vec<(usize, &'a str)>, HashSet<String>) {
    let main = prefixes_for(&pt.root_attrs, MAIN_NS);
    let relp = {
        let mut v = prefixes_for(&pt.root_attrs, REL_NS);
        if v.is_empty() {
            v.push("r".to_string());
        }
        v
    };
    let rank = |name: &str| order.iter().position(|n| *n == name);
    let ext = rank("extLst").unwrap_or(order.len());
    let mut items: Vec<(usize, &str)> = known
        .iter()
        .map(|(n, x)| (rank(n).unwrap_or(ext).saturating_mul(2), x.as_str()))
        .collect();
    let mut ids = HashSet::new();
    for el in &pt.elements {
        let local = crate::xml::local(&el.name);
        let main_el = is_main(&el.name, &main);
        if (main_el && known.iter().any(|(n, _)| n == local)) || skip(el) {
            continue;
        }
        let el_ids = rel_ids_in(&el.xml, &relp);
        if el_ids.iter().any(|i| !valid_ids.contains(i)) {
            continue;
        }
        ids.extend(el_ids);
        let r = match (main_el, rank(local)) {
            (true, Some(r)) => r * 2,
            (true, None) => ext.saturating_mul(2).saturating_sub(1),
            (false, _) => match rank(&el.after) {
                Some(a) => a * 2 + 1,
                None => 0,
            },
        };
        items.push((r, el.xml.as_str()));
    }
    items.sort_by_key(|(r, _)| *r);
    (items, ids)
}

/// Every relationship id referenced by the captured elements.
pub fn referenced_ids(pt: &Passthrough) -> HashSet<String> {
    let mut relp = prefixes_for(&pt.root_attrs, REL_NS);
    if relp.is_empty() {
        relp.push("r".to_string());
    }
    pt.elements
        .iter()
        .flat_map(|e| rel_ids_in(&e.xml, &relp))
        .collect()
}

/// Split our own generated XML fragment into (local name, XML) elements.
pub fn split_known(fragment: &str) -> Vec<(String, String)> {
    match scan_children(fragment, false) {
        Ok((_, cs)) => cs
            .into_iter()
            .map(|c| (c.local().to_string(), c.xml.to_string()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Package validation

/// Structural problems in a saved package: parts without a content type, relationships
/// whose internal target is missing, `r:id` references with no relationship, malformed
/// XML parts, and worksheet children out of CT_Worksheet order. Empty = valid.
pub fn validate_package(bytes: &[u8]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut zip = match zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
        Ok(z) => z,
        Err(e) => return vec![format!("not a zip: {e}")],
    };
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let lower: HashSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let read = |zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, n: &str| {
        read_bytes(zip, n, MAX_PART_BYTES).ok().flatten()
    };
    let Some(ct_bytes) = read(&mut zip, "[Content_Types].xml") else {
        return vec!["missing [Content_Types].xml".into()];
    };
    let ct = match ContentTypes::parse(&String::from_utf8_lossy(&ct_bytes)) {
        Ok(c) => c,
        Err(e) => return vec![format!("[Content_Types].xml: {e}")],
    };
    for n in &names {
        if n == "[Content_Types].xml" || n.ends_with('/') {
            continue;
        }
        if ct.get(n).is_none() {
            problems.push(format!("{n}: no content type"));
        }
    }
    for o in ct.overrides.keys() {
        if !lower.contains(o) {
            problems.push(format!("content type override for missing part /{o}"));
        }
    }
    let mut rels_of: HashMap<String, Vec<RawRel>> = HashMap::new();
    for n in names.iter().filter(|n| n.ends_with(".rels")) {
        let Some(b) = read(&mut zip, n) else { continue };
        let source = match n.rsplit_once("_rels/") {
            Some((dir, file)) => format!("{dir}{}", file.trim_end_matches(".rels")),
            None => continue,
        };
        if !source.is_empty() && !lower.contains(&source.to_ascii_lowercase()) {
            problems.push(format!("{n}: source part {source} missing"));
        }
        match parse_raw_rels(&String::from_utf8_lossy(&b), dir_of(&source)) {
            Ok(rels) => {
                let mut ids = HashSet::new();
                for r in &rels {
                    if !ids.insert(r.id.clone()) {
                        problems.push(format!("{n}: duplicate id {}", r.id));
                    }
                    if !r.external && !lower.contains(&r.target.to_ascii_lowercase()) {
                        problems.push(format!("{n}: {} → missing {}", r.id, r.target));
                    }
                }
                rels_of.insert(source, rels);
            }
            Err(e) => problems.push(format!("{n}: {e}")),
        }
    }
    for n in &names {
        let t = ct.get(n).unwrap_or("");
        if !(t.ends_with("+xml") || t == "application/xml" || n.ends_with(".xml")) {
            continue;
        }
        let Some(b) = read(&mut zip, n) else { continue };
        let text = String::from_utf8_lossy(&b);
        let (root_attrs, children) = match scan_children(&text, true) {
            Ok(x) => x,
            Err(e) => {
                problems.push(format!("{n}: malformed XML ({e})"));
                continue;
            }
        };
        if let Err(e) = well_formed(&text) {
            problems.push(format!("{n}: malformed XML ({e})"));
            continue;
        }
        let is_ws =
            t == "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
        let is_wb = t.ends_with(".main+xml");
        if !(is_ws || is_wb) {
            continue;
        }
        let ids: HashSet<String> = rels_of
            .get(n)
            .map(|v| v.iter().map(|r| r.id.clone()).collect())
            .unwrap_or_default();
        let relp = prefixes_for(&root_attrs, REL_NS);
        let main = prefixes_for(&root_attrs, MAIN_NS);
        let order = if is_ws {
            WORKSHEET_ORDER
        } else {
            WORKBOOK_ORDER
        };
        let mut last: Option<usize> = None;
        let mut seen: HashSet<&str> = HashSet::new();
        for c in &children {
            for id in rel_ids_in(c.xml, &relp) {
                if !ids.contains(&id) {
                    problems.push(format!("{n}: <{}> references unknown {id}", c.qname));
                }
            }
            if !is_main(c.qname, &main) {
                continue;
            }
            let local = c.local();
            let Some(r) = order.iter().position(|x| *x == local) else {
                problems.push(format!("{n}: unexpected element <{local}>"));
                continue;
            };
            if last.is_some_and(|l| r < l) {
                problems.push(format!("{n}: <{local}> out of schema order"));
            }
            if local != "conditionalFormatting" && !seen.insert(local) {
                problems.push(format!("{n}: <{local}> repeated"));
            }
            last = Some(r);
        }
    }
    problems
}

fn well_formed(xml: &str) -> Result<(), IoError> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader
            .read_event()
            .map_err(|e| IoError::Xml(e.to_string()))?
        {
            Event::Eof => return Ok(()),
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_model::{Sheet, Workbook};

    fn part(path: &str, ct: &str, data: &str, rels: Vec<RawRel>) -> RawPart {
        RawPart {
            path: path.into(),
            content_type: ct.into(),
            data: Arc::from(data.as_bytes().to_vec()),
            rels,
        }
    }

    fn rel(id: &str, kind: &str, target: &str) -> RawRel {
        RawRel {
            id: id.into(),
            rel_type: format!("{REL_NS}/{kind}"),
            target: target.into(),
            external: false,
        }
    }

    /// A sheet with a drawing (→ chart → shared image) and a table, plus conditional
    /// formatting and an extension list, all opaque.
    fn sample() -> Workbook {
        let mut wb = Workbook::new();
        let s = &mut wb.sheets[0];
        s.hyperlinks.push(sp_model::Hyperlink {
            range: sp_core::CellRange::parse("A1").unwrap(),
            target: "https://example.com".into(),
            location: String::new(),
            tooltip: String::new(),
            display: String::new(),
        });
        let pt = &mut s.passthrough;
        pt.root_attrs = vec![("xmlns:x14".into(), "urn:x14".into())];
        for (name, after, xml) in [
            (
                "extLst",
                "tableParts",
                r#"<extLst><ext uri="u"><x14:a/></ext></extLst>"#,
            ),
            (
                "conditionalFormatting",
                "mergeCells",
                r#"<conditionalFormatting sqref="A1"><cfRule type="expression" priority="1"><formula>A1&gt;1</formula></cfRule></conditionalFormatting>"#,
            ),
            ("drawing", "pageMargins", r#"<drawing r:id="rId1"/>"#),
            (
                "tableParts",
                "drawing",
                r#"<tableParts count="1"><tablePart r:id="rId2"/></tableParts>"#,
            ),
        ] {
            pt.elements.push(RawElement {
                name: name.into(),
                after: after.into(),
                xml: xml.into(),
            });
        }
        pt.rels = vec![
            rel("rId1", "drawing", "xl/drawings/drawing1.xml"),
            rel("rId2", "table", "xl/tables/table1.xml"),
        ];
        pt.parts = vec![
            part(
                "xl/drawings/drawing1.xml",
                "application/vnd.openxmlformats-officedocument.drawing+xml",
                "<wsDr/>",
                vec![rel("rId1", "chart", "xl/charts/chart1.xml")],
            ),
            part(
                "xl/charts/chart1.xml",
                "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
                "<chartSpace/>",
                vec![rel("rId1", "image", "xl/media/image1.png")],
            ),
            part(
                "xl/tables/table1.xml",
                TABLE_CONTENT_TYPE,
                "<table/>",
                vec![],
            ),
        ];
        wb.passthrough.parts.push(part(
            "xl/media/image1.png",
            "image/png",
            "\u{89}PNG",
            vec![],
        ));
        wb.passthrough.parts.push(part(
            "xl/theme/theme1.xml",
            "application/vnd.openxmlformats-officedocument.theme+xml",
            "<theme/>",
            vec![],
        ));
        wb.passthrough
            .rels
            .push(rel("rId1", "theme", "xl/theme/theme1.xml"));
        wb
    }

    fn save(wb: &Workbook) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        crate::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
        buf.into_inner()
    }

    fn text(bytes: &[u8], name: &str) -> Option<String> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut s = String::new();
        z.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    }

    #[test]
    fn round_trip_keeps_order_ids_and_parts() {
        let wb = sample();
        let out = save(&wb);
        assert_eq!(validate_package(&out), Vec::<String>::new());
        let ws = text(&out, "xl/worksheets/sheet1.xml").unwrap();
        let pos = |s: &str| ws.find(s).unwrap();
        assert!(pos("<sheetData") < pos("<conditionalFormatting"));
        assert!(pos("<conditionalFormatting") < pos("<hyperlinks"));
        assert!(pos("<pageMargins") < pos("<drawing"));
        assert!(pos("<drawing") < pos("<tableParts"));
        assert!(pos("<tableParts") < pos("<extLst"));
        assert!(ws.contains(r#"xmlns:x14="urn:x14""#));
        // Our hyperlink relationship avoids the captured ids.
        let rels = text(&out, "xl/worksheets/_rels/sheet1.xml.rels").unwrap();
        assert!(rels.contains(r#"Id="rId3""#), "{rels}");
        let (back, _) = crate::xlsx_read::read_xlsx(std::io::Cursor::new(out.clone())).unwrap();
        let (a, b) = (&wb.sheets[0].passthrough, &back.sheets[0].passthrough);
        // Captured elements that are now modelled (conditional formats, table parts) are
        // read into the model instead of being captured again.
        let unhandled = a
            .elements
            .iter()
            .filter(|e| !HANDLED_SHEET_ELEMENTS.contains(&crate::xml::local(&e.name)))
            .count();
        assert_eq!(unhandled, b.elements.len());
        assert_eq!(back.sheets[0].cond_formats.len(), 1);
        // The image is now reachable from one sheet only, so that sheet owns it; the
        // (invalid) table part is dropped with its now-handled relationship.
        assert_eq!(b.parts.len(), 3);
        assert_eq!(back.passthrough.parts.len(), 1);
        assert_eq!(back.sheets[0].hyperlinks, wb.sheets[0].hyperlinks);
        // Byte-identical parts.
        assert_eq!(text(&out, "xl/charts/chart1.xml").unwrap(), "<chartSpace/>");
    }

    #[test]
    fn duplicate_rename_reorder_delete() {
        let mut wb = sample();
        let mut copy = wb.sheets[0].clone();
        copy.name = "Copy".into();
        wb.sheets.insert(0, copy);
        wb.sheets.push(Sheet::new("Plain"));
        wb.sheets[1].name = "Renamed".into();
        let out = save(&wb);
        assert_eq!(validate_package(&out), Vec::<String>::new());
        // The first (copy) keeps the original part names; the second gets renamed copies
        // and loses its duplicated table.
        assert!(text(&out, "xl/drawings/drawing1.xml").is_some());
        assert!(text(&out, "xl/drawings/drawing1_2.xml").is_some());
        assert!(text(&out, "xl/tables/table1_2.xml").is_none());
        let ws2 = text(&out, "xl/worksheets/sheet2.xml").unwrap();
        assert!(!ws2.contains("tableParts") && ws2.contains("<drawing"));
        // Deleting the sheet that owns the parts drops them; shared parts stay valid.
        wb.sheets.remove(0);
        wb.sheets.remove(0);
        let out = save(&wb);
        assert_eq!(validate_package(&out), Vec::<String>::new());
        assert!(text(&out, "xl/drawings/drawing1.xml").is_none());
        assert!(text(&out, "xl/theme/theme1.xml").is_some());
    }

    #[test]
    fn handled_names_win_over_captured_ones() {
        let mut wb = sample();
        // A captured legacyDrawing is superseded once the sheet has notes.
        let s = &mut wb.sheets[0];
        s.passthrough.elements.push(RawElement {
            name: "legacyDrawing".into(),
            after: "drawing".into(),
            xml: r#"<legacyDrawing r:id="rId9"/>"#.into(),
        });
        s.passthrough
            .rels
            .push(rel("rId9", "vmlDrawing", "xl/drawings/vmlDrawing9.vml"));
        s.passthrough.parts.push(part(
            "xl/drawings/vmlDrawing9.vml",
            "application/vnd.openxmlformats-officedocument.vmlDrawing",
            "<xml/>",
            vec![],
        ));
        let out = save(&wb);
        assert_eq!(validate_package(&out), Vec::<String>::new());
        assert!(
            text(&out, "xl/worksheets/sheet1.xml")
                .unwrap()
                .contains(r#"r:id="rId9""#)
        );
        wb.sheets[0].comments.push(sp_model::Comment {
            addr: sp_core::CellAddr::new(0, 0),
            kind: sp_model::CommentKind::Note,
            author: "a".into(),
            text: "t".into(),
            replies: vec![],
            visible: false,
            resolved: false,
        });
        let out = save(&wb);
        assert_eq!(validate_package(&out), Vec::<String>::new());
        let ws = text(&out, "xl/worksheets/sheet1.xml").unwrap();
        assert_eq!(ws.matches("<legacyDrawing").count(), 1);
        assert!(!ws.contains("rId9"));
    }

    #[test]
    fn vba_project_keeps_macro_enabled_workbook() {
        let mut wb = sample();
        wb.passthrough
            .rels
            .push(rel("rId2", "vbaProject", "xl/vbaProject.bin"));
        wb.passthrough.parts.push(part(
            "xl/vbaProject.bin",
            "application/vnd.ms-office.vbaProject",
            "VBA",
            vec![],
        ));
        let out = save(&wb);
        assert_eq!(validate_package(&out), Vec::<String>::new());
        let ct = text(&out, "[Content_Types].xml").unwrap();
        assert!(ct.contains("macroEnabled.main+xml"), "{ct}");
        assert_eq!(text(&out, "xl/vbaProject.bin").unwrap(), "VBA");
    }

    #[test]
    fn validator_catches_problems() {
        let wb = sample();
        let out = save(&wb);
        // Rewrite the package with a dangling relationship and misordered elements.
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(out)).unwrap();
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for i in 0..z.len() {
            let mut f = z.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut s = Vec::new();
            f.read_to_end(&mut s).unwrap();
            if name == "xl/charts/chart1.xml" {
                continue;
            }
            if name == "xl/worksheets/sheet1.xml" {
                let t = String::from_utf8(s).unwrap();
                s = t
                    .replace(
                        "<sheetData>",
                        "<sheetData/><cols><col min=\"1\" max=\"1\"/></cols><x>",
                    )
                    .replace("</sheetData>", "</x>")
                    .into_bytes();
            }
            w.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut w, &s).unwrap();
        }
        let bytes = w.finish().unwrap().into_inner();
        let p = validate_package(&bytes).join("\n");
        assert!(p.contains("missing xl/charts/chart1.xml"), "{p}");
        assert!(
            p.contains("no content type") || p.contains("override for missing"),
            "{p}"
        );
        assert!(p.contains("out of schema order"), "{p}");
    }
}
