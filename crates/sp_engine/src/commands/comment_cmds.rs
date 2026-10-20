//! Comments (threaded), notes (legacy) and hyperlinks (`insert.comment`, `insert.note`,
//! `review.comment.*`, `review.notes.show_all`, `home.editing.clear_comments`,
//! `insert.hyperlink`, `home.editing.clear_hyperlinks`).

use crate::params::{bool_opt, cell, range, str_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange, Value};
use sp_model::{Cell, Color, Comment, CommentKind, CommentReply, Hyperlink, Sheet};

/// The target app's default hyperlink colour.
pub const LINK_COLOR: Color = Color(0x05, 0x63, 0xC1);

fn author(s: &Session, p: &Json) -> String {
    str_opt(p, "author")
        .map(str::to_string)
        .filter(|a| !a.trim().is_empty())
        .or_else(|| Some(s.wb.props.author.clone()).filter(|a| !a.is_empty()))
        .unwrap_or_else(|| "Author".into())
}

fn text_param(p: &Json) -> Result<String, CmdError> {
    let t = str_opt(p, "text").ok_or_else(|| CmdError::Param("missing string 'text'".into()))?;
    if t.chars().count() > sp_core::MAX_TEXT_LEN {
        return Err(CmdError::Param(
            "text is longer than 32,767 characters".into(),
        ));
    }
    Ok(t.to_string())
}

fn comment_json(c: &Comment) -> Json {
    json!({
        "cell": c.addr.to_a1(),
        "kind": match c.kind { CommentKind::Note => "note", CommentKind::Thread => "comment" },
        "author": c.author,
        "text": c.text,
        "replies": c.replies.iter().map(|r| json!({"author": r.author, "text": r.text})).collect::<Vec<_>>(),
        "visible": c.visible,
    })
}

/// Insert or replace the comment at `c.addr`, keeping the list sorted by address.
fn put_comment(sh: &mut Sheet, c: Comment) {
    match sh.comments.binary_search_by_key(&c.addr, |x| x.addr) {
        Ok(i) => {
            if let Some(slot) = sh.comments.get_mut(i) {
                *slot = c;
            }
        }
        Err(i) => sh.comments.insert(i, c),
    }
}

fn new_comment(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    let text = text_param(p)?;
    let who = author(s, p);
    let existing = s.wb.sheet(sheet).and_then(|sh| sh.comment_at(at)).cloned();
    let c = match existing {
        Some(c) if c.kind == CommentKind::Note => {
            return Err(CmdError::Failed(
                "This cell already has a note. Edit or delete the note first.".into(),
            ));
        }
        Some(mut c) => {
            if c.replies.len() >= 10_000 {
                return Err(CmdError::Failed("too many replies".into()));
            }
            c.replies.push(CommentReply { author: who, text });
            c
        }
        None => Comment {
            addr: at,
            kind: CommentKind::Thread,
            author: who,
            text,
            replies: Vec::new(),
            visible: false,
            resolved: false,
        },
    };
    let out = comment_json(&c);
    s.transact("New Comment", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            put_comment(sh, c);
        }
        Ok(())
    })?;
    Ok(out)
}

fn new_note(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, at) = cell(s, p, "cell")?;
    let text = text_param(p)?;
    let who = author(s, p);
    let existing = s.wb.sheet(sheet).and_then(|sh| sh.comment_at(at)).cloned();
    if existing
        .as_ref()
        .is_some_and(|c| c.kind == CommentKind::Thread)
    {
        return Err(CmdError::Failed(
            "This cell already has a comment thread.".into(),
        ));
    }
    let c = Comment {
        addr: at,
        kind: CommentKind::Note,
        author: existing.as_ref().map_or(who, |c| c.author.clone()),
        text,
        replies: Vec::new(),
        visible: existing.as_ref().is_some_and(|c| c.visible),
        resolved: false,
    };
    let out = comment_json(&c);
    s.transact(
        if existing.is_some() {
            "Edit Note"
        } else {
            "New Note"
        },
        |tx, _| {
            tx.touch_sheet(sheet);
            if let Some(sh) = tx.wb.sheet_mut(sheet) {
                put_comment(sh, c);
            }
            Ok(())
        },
    )?;
    Ok(out)
}

fn delete_in(s: &mut Session, p: &Json, label: &str, must_exist: bool) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let n = s.wb.sheet(sheet).map_or(0, |sh| {
        sh.comments.iter().filter(|c| r.contains(c.addr)).count()
    });
    if n == 0 {
        return if must_exist {
            Err(CmdError::Failed(
                "There are no comments or notes here.".into(),
            ))
        } else {
            Ok(json!(0))
        };
    }
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.comments.retain(|c| !r.contains(c.addr));
        }
        Ok(())
    })?;
    Ok(json!(n))
}

fn step_comment(s: &mut Session, forward: bool) -> CmdResult {
    let sheet = s.sheet_index();
    let addrs: Vec<CellAddr> =
        s.wb.sheet(sheet)
            .map(|sh| sh.comments.iter().map(|c| c.addr).collect())
            .unwrap_or_default();
    let cur = s.sel.active;
    let next = if forward {
        addrs.iter().find(|a| **a > cur).or_else(|| addrs.first())
    } else {
        addrs
            .iter()
            .rev()
            .find(|a| **a < cur)
            .or_else(|| addrs.last())
    }
    .copied()
    .ok_or_else(|| CmdError::Failed("There are no comments on this sheet.".into()))?;
    s.select(next);
    let c = s.wb.sheet(sheet).and_then(|sh| sh.comment_at(next));
    Ok(c.map_or(Json::Null, comment_json))
}

fn show_comments(s: &mut Session, p: &Json) -> CmdResult {
    s.view.review.comments_pane = bool_opt(p, "show").unwrap_or(!s.view.review.comments_pane);
    let sheet = s.sheet_index();
    let list: Vec<Json> =
        s.wb.sheet(sheet)
            .map(|sh| {
                sh.comments
                    .iter()
                    .filter(|c| c.kind == CommentKind::Thread)
                    .map(comment_json)
                    .collect()
            })
            .unwrap_or_default();
    Ok(json!({ "pane": s.view.review.comments_pane, "comments": list }))
}

/// Toggle every note in the workbook visible / hidden.
fn show_all_notes(s: &mut Session, p: &Json) -> CmdResult {
    let notes = |sh: &Sheet| {
        sh.comments
            .iter()
            .filter(|c| c.kind == CommentKind::Note)
            .map(|c| c.visible)
            .collect::<Vec<_>>()
    };
    let all: Vec<bool> = s.wb.sheets.iter().flat_map(notes).collect();
    if all.is_empty() {
        return Err(CmdError::Failed(
            "There are no notes in this workbook.".into(),
        ));
    }
    let show = bool_opt(p, "show").unwrap_or(!all.iter().all(|v| *v));
    let sheets: Vec<usize> = (0..s.wb.sheets.len()).collect();
    s.transact("Show All Notes", |tx, _| {
        for i in sheets {
            if tx.wb.sheet(i).is_some_and(|sh| !notes(sh).is_empty()) {
                tx.touch_sheet(i);
                if let Some(sh) = tx.wb.sheet_mut(i) {
                    for c in sh
                        .comments
                        .iter_mut()
                        .filter(|c| c.kind == CommentKind::Note)
                    {
                        c.visible = show;
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(json!({ "visible": show, "notes": all.len() }))
}

fn insert_hyperlink(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = match str_opt(p, "cell") {
        Some(_) => {
            let (i, a) = cell(s, p, "cell")?;
            (i, CellRange::single(a))
        }
        None => range(s, p, "range")?,
    };
    if r.cell_count() > 100_000 {
        return Err(CmdError::Param("range too large".into()));
    }
    let target = str_opt(p, "address").unwrap_or("").trim().to_string();
    let location = str_opt(p, "location").unwrap_or("").trim().to_string();
    if target.is_empty() && location.is_empty() {
        return Err(CmdError::Param(
            "give 'address' (URL/file) or 'location' (Sheet2!A1)".into(),
        ));
    }
    if !location.is_empty() {
        crate::params::parse_ref(s, &location)?;
    }
    if target.len() > 2_079 {
        return Err(CmdError::Param(
            "addresses are limited to 2,079 characters".into(),
        ));
    }
    let text = str_opt(p, "text").map(str::to_string);
    let tooltip = str_opt(p, "tooltip")
        .unwrap_or("")
        .chars()
        .take(255)
        .collect::<String>();
    let display = text.clone().unwrap_or_else(|| {
        if target.is_empty() {
            location.clone()
        } else {
            target.clone()
        }
    });
    let link = Hyperlink {
        range: r,
        target,
        location,
        tooltip,
        display: display.clone(),
    };
    let anchor = r.start;
    let old = s.wb.sheet(sheet).and_then(|sh| sh.get(anchor)).cloned();
    let empty = old
        .as_ref()
        .is_none_or(|c| c.value.is_empty() && c.formula_src().is_none());
    let style = old.as_ref().map(|c| c.style).unwrap_or_default();
    let link_style = s.wb.styles.derive(style, |st| {
        if st.font_color.is_none() {
            st.font_color = Some(LINK_COLOR);
        }
        st.underline = 1;
    });
    s.transact("Hyperlink", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.hyperlinks.retain(|h| h.range.intersect(&r).is_none());
            sh.hyperlinks.push(link);
            if empty || text.is_some() {
                let mut c = old.clone().unwrap_or_default();
                c.content = sp_model::Content::Value(Value::text(display.as_str()));
                c.value = Value::text(display.as_str());
                sh.set(anchor, c);
            }
            if sh.get(anchor).is_some() {
                sh.entry(anchor).style = link_style;
            }
        }
        Ok(())
    })?;
    Ok(json!({ "range": r.to_a1(), "display": display }))
}

fn clear_hyperlinks(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let formats = bool_opt(p, "formats").unwrap_or(false);
    let hit: Vec<CellRange> =
        s.wb.sheet(sheet)
            .map(|sh| {
                sh.hyperlinks
                    .iter()
                    .filter(|h| h.range.intersect(&r).is_some())
                    .map(|h| h.range)
                    .collect()
            })
            .unwrap_or_default();
    if hit.is_empty() {
        return Ok(json!(0));
    }
    // Restyled cells (link colour and underline removed) when asked.
    let mut restyle: Vec<(CellAddr, Cell)> = Vec::new();
    let cells: Vec<(CellAddr, Cell)> = match s.wb.sheet(sheet) {
        Some(sh) if formats => hit
            .iter()
            .flat_map(|h| sh.cells_in(*h).map(|(a, c)| (a, c.clone())))
            .collect(),
        _ => Vec::new(),
    };
    {
        {
            for (a, c) in cells {
                let style = s.wb.styles.derive(c.style, |st| {
                    if st.font_color == Some(LINK_COLOR) {
                        st.font_color = None;
                    }
                    st.underline = 0;
                });
                restyle.push((a, Cell { style, ..c }));
            }
        }
    }
    let n = hit.len();
    s.transact("Clear Hyperlinks", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.hyperlinks.retain(|h| h.range.intersect(&r).is_none());
            for (a, c) in restyle {
                sh.set(a, c);
            }
        }
        Ok(())
    })?;
    Ok(json!(n))
}

pub fn specs() -> Vec<CommandSpec> {
    let c = r#"{"cell"?: "B2", "text": "…", "author"?: "Name"} (adds a reply when the cell has a thread)"#;
    let link = r#"{"cell"?: "A1" | "range"?: "A1:B2", "address"?: "https://…", "location"?: "Sheet2!A1", "text"?: "shown", "tooltip"?: "tip"}"#;
    vec![
        CommandSpec::new("insert.comment", c, new_comment),
        CommandSpec::new("review.comment.new", c, new_comment),
        CommandSpec::new(
            "insert.note",
            r#"{"cell"?: "B2", "text": "…", "author"?: "Name"} (edits an existing note)"#,
            new_note,
        ),
        CommandSpec::new("review.comment.delete", r#"{"range"?: "A1:C9"}"#, |s, p| {
            delete_in(s, p, "Delete Comment", true)
        })
        // Something to delete in the selection (as delete_in checks).
        .when(|s| {
            let r = s.sel.range();
            s.wb.sheet(s.sheet_index())
                .is_some_and(|sh| sh.comments.iter().any(|c| r.contains(c.addr)))
        }),
        CommandSpec::new("review.comment.previous", "{}", |s, _| {
            step_comment(s, false)
        }),
        CommandSpec::new("review.comment.next", "{}", |s, _| step_comment(s, true)),
        CommandSpec::new(
            "review.comment.show",
            r#"{"show"?: bool (default toggles)}"#,
            show_comments,
        ),
        CommandSpec::new(
            "review.notes.show_all",
            r#"{"show"?: bool (default toggles)}"#,
            show_all_notes,
        ),
        CommandSpec::new(
            "home.editing.clear_comments",
            r#"{"range"?: "A1:C9"}"#,
            |s, p| delete_in(s, p, "Clear Comments", false),
        ),
        CommandSpec::new("insert.hyperlink", link, insert_hyperlink),
        CommandSpec::new(
            "home.editing.clear_hyperlinks",
            r#"{"range"?: "A1:C9", "formats"?: bool}"#,
            clear_hyperlinks,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_notes_navigation() {
        let mut s = Session::new();
        s.execute(
            "insert.comment",
            &json!({"cell": "B2", "text": "Check this", "author": "Ann"}),
        )
        .unwrap();
        s.execute(
            "review.comment.new",
            &json!({"cell": "B2", "text": "Done", "author": "Bob"}),
        )
        .unwrap();
        s.execute("insert.note", &json!({"cell": "D5", "text": "a note"}))
            .unwrap();
        assert!(
            s.execute("insert.note", &json!({"cell": "B2", "text": "x"}))
                .is_err()
        );
        assert!(
            s.execute("insert.comment", &json!({"cell": "D5", "text": "x"}))
                .is_err()
        );
        let sh = &s.wb.sheets[0];
        assert_eq!(sh.comments.len(), 2);
        assert_eq!(
            sh.comment_at(CellAddr::parse("B2").unwrap())
                .unwrap()
                .replies
                .len(),
            1
        );
        let n = s.execute("review.comment.next", &json!({})).unwrap();
        assert_eq!(n["cell"], "B2");
        let n = s.execute("review.comment.next", &json!({})).unwrap();
        assert_eq!(n["cell"], "D5");
        let n = s.execute("review.comment.next", &json!({})).unwrap();
        assert_eq!(n["cell"], "B2");
        let n = s.execute("review.comment.previous", &json!({})).unwrap();
        assert_eq!(n["cell"], "D5");
        let r = s.execute("review.notes.show_all", &json!({})).unwrap();
        assert_eq!(r["visible"], true);
        assert!(s.wb.sheets[0].comments[1].visible);
        let r = s.execute("review.comment.show", &json!({})).unwrap();
        assert_eq!(r["comments"].as_array().unwrap().len(), 1);
        s.execute("review.comment.delete", &json!({"range": "B2"}))
            .unwrap();
        assert_eq!(s.wb.sheets[0].comments.len(), 1);
        assert!(
            s.execute("review.comment.delete", &json!({"range": "B2"}))
                .is_err()
        );
        s.execute("home.editing.clear_comments", &json!({"range": "A1:Z99"}))
            .unwrap();
        assert!(s.wb.sheets[0].comments.is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets[0].comments.len(), 1);
    }

    #[test]
    fn hyperlinks() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A2", "input": "Docs"}))
            .unwrap();
        s.execute(
            "insert.hyperlink",
            &json!({"cell": "A1", "address": "https://example.com"}),
        )
        .unwrap();
        s.execute(
            "insert.hyperlink",
            &json!({"cell": "A2", "location": "Sheet1!C3", "tooltip": "go"}),
        )
        .unwrap();
        let sh = &s.wb.sheets[0];
        assert_eq!(
            sh.value(CellAddr::new(0, 0)).to_display(),
            "https://example.com"
        );
        assert_eq!(sh.value(CellAddr::new(1, 0)).to_display(), "Docs");
        assert_eq!(sh.hyperlinks.len(), 2);
        let st = s.wb.styles.get(sh.get(CellAddr::new(1, 0)).unwrap().style);
        assert_eq!(st.font_color, Some(LINK_COLOR));
        assert!(
            s.execute("insert.hyperlink", &json!({"cell": "A3"}))
                .is_err()
        );
        assert!(
            s.execute(
                "insert.hyperlink",
                &json!({"cell": "A3", "location": "Nope!A1"})
            )
            .is_err()
        );
        let n = s
            .execute(
                "home.editing.clear_hyperlinks",
                &json!({"range": "A1:A1", "formats": true}),
            )
            .unwrap();
        assert_eq!(n, 1);
        let sh = &s.wb.sheets[0];
        assert_eq!(sh.hyperlinks.len(), 1);
        assert_eq!(
            s.wb.styles
                .get(sh.get(CellAddr::new(0, 0)).unwrap().style)
                .underline,
            0
        );
    }
}
