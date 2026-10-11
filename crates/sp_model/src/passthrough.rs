//! Opaque file content SheetPro does not model (drawings, charts, images, pivot tables,
//! table parts, conditional formatting, themes, VBA, extension lists…), kept verbatim on
//! load so a save does not drop it. Owned by `sp_io`: the rest of the app only moves it
//! around with its sheet (rename / reorder / delete / copy need no special handling here;
//! the writer resolves part-name collisions and drops dangling references).

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Unmodelled content attached to a workbook (workbook-level) or to one sheet.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Passthrough {
    /// Unmodelled child elements of the owning part's root element (`<worksheet>` or
    /// `<workbook>`), verbatim, in document order.
    pub elements: Vec<RawElement>,
    /// Namespace declarations (and `mc:Ignorable`) of the original root element, so the
    /// verbatim elements stay well-formed when re-emitted under our root.
    pub root_attrs: Vec<(String, String)>,
    /// Relationships from the owning part kept with their original ids (the ids are what
    /// `elements` reference through `r:id`).
    pub rels: Vec<RawRel>,
    /// Unmodelled parts owned here: everything reachable through `rels`, transitively.
    pub parts: Vec<RawPart>,
    /// Workbook only: package relationships (`_rels/.rels`) beyond the ones the writer emits.
    pub package_rels: Vec<RawRel>,
    /// Sheet only: the sheet is a part we do not model as a worksheet (a chart sheet,
    /// dialog sheet or macro sheet) and is written back verbatim.
    pub sheet_part: Option<OpaqueSheet>,
    /// Sheet only: the original `sheetId` (kept when still unique).
    pub sheet_id: Option<u32>,
}

impl Passthrough {
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
            && self.rels.is_empty()
            && self.parts.is_empty()
            && self.package_rels.is_empty()
            && self.sheet_part.is_none()
    }
}

/// A child element of a part's root, verbatim.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RawElement {
    /// Qualified name as written (`conditionalFormatting`, `mc:AlternateContent`).
    pub name: String,
    /// Local name of the nearest preceding schema element in the source document (used to
    /// place elements whose position the schema does not fix); empty at the start.
    pub after: String,
    /// The element's complete XML text.
    pub xml: String,
}

/// One package relationship.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RawRel {
    pub id: String,
    /// Full relationship type URI.
    pub rel_type: String,
    /// Internal: absolute part name without the leading `/`. External: the target as given.
    pub target: String,
    pub external: bool,
}

/// One package part, verbatim.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RawPart {
    /// Part name without the leading `/`, e.g. `xl/drawings/drawing1.xml`.
    pub path: String,
    pub content_type: String,
    pub data: Arc<[u8]>,
    /// The part's own relationships (its `_rels/*.rels`), targets resolved.
    pub rels: Vec<RawRel>,
}

/// A sheet stored as an opaque part.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OpaqueSheet {
    /// Relationship type from the workbook (chartsheet, dialogsheet, macrosheet).
    pub rel_type: String,
    pub part: RawPart,
}
