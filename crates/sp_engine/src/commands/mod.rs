//! Command modules. Each `<area>_cmds` module exposes `specs()`; add new modules here with
//! a one-line edit (see AGENTS.md › Parallel agents).

pub mod app_cmds;
pub mod audit_cmds;
pub mod cell_cmds;
pub mod cf_cmds;
pub mod chart_cmds;
pub mod comment_cmds;
pub mod data_cmds;
pub mod dialog_cmds;
pub mod edit_cmds;
pub mod file_cmds;
pub mod filter_cmds;
pub mod flash_cmds;
pub mod format_cmds;
pub mod layout_cmds;
pub mod misc_cmds;
pub mod nav_cmds;
pub mod outline_cmds;
pub mod pivot_cmds;
pub mod print_cmds;
pub mod protect_cmds;
pub mod sheet_cmds;
pub mod table_cmds;
pub mod theme_cmds;
pub mod ui_cmds;
pub mod validation_cmds;
pub mod view_cmds;
pub mod whatif_cmds;

use crate::CommandSpec;

pub fn all_specs() -> Vec<CommandSpec> {
    let mut v = Vec::new();
    v.extend(cell_cmds::specs());
    v.extend(data_cmds::specs());
    v.extend(dialog_cmds::specs());
    v.extend(edit_cmds::specs());
    v.extend(file_cmds::specs());
    v.extend(format_cmds::specs());
    v.extend(nav_cmds::specs());
    v.extend(sheet_cmds::specs());
    v.extend(view_cmds::specs());
    v.extend(audit_cmds::specs());
    v.extend(comment_cmds::specs());
    v.extend(filter_cmds::specs());
    v.extend(layout_cmds::specs());
    v.extend(misc_cmds::specs());
    v.extend(outline_cmds::specs());
    v.extend(protect_cmds::specs());
    v.extend(validation_cmds::specs());
    v.extend(chart_cmds::specs());
    v.extend(cf_cmds::specs());
    v.extend(table_cmds::specs());
    v.extend(pivot_cmds::specs());
    v.extend(whatif_cmds::specs());
    v.extend(print_cmds::specs());
    v.extend(app_cmds::specs());
    v.extend(ui_cmds::specs());
    v.extend(flash_cmds::specs());
    v.extend(theme_cmds::specs());
    v
}
