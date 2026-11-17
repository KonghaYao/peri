use crate::analysis::PersonStats;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};

/// Render PersonStats vector as a colored terminal table.
pub fn render_table(stats: &[PersonStats]) {
    if stats.is_empty() {
        eprintln!("No commits found in the specified time window.");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            cell_bold("Name"),
            cell_bold("Commits"),
            cell_bold("+Lines"),
            cell_bold("-Lines"),
            cell_bold("Files"),
            cell_bold("Co-Authored"),
            cell_bold("Feat"),
            cell_bold("Fix"),
            cell_bold("Refactor"),
            cell_bold("Chore"),
            cell_bold("Docs"),
            cell_bold("Test"),
            cell_bold("Other"),
        ]);

    for s in stats {
        let co_authored_str = if s.co_authored_with.is_empty() {
            String::from("-")
        } else {
            s.co_authored_with.join(", ")
        };

        table.add_row(vec![
            Cell::new(&s.name).fg(comfy_table::Color::Cyan),
            Cell::new(s.commits),
            Cell::new(s.added_lines).fg(comfy_table::Color::Green),
            Cell::new(s.deleted_lines).fg(comfy_table::Color::Red),
            Cell::new(s.files_touched),
            Cell::new(co_authored_str),
            Cell::new(s.feat).fg(comfy_table::Color::Green),
            Cell::new(s.fix).fg(comfy_table::Color::Magenta),
            Cell::new(s.refactor_).fg(comfy_table::Color::Blue),
            Cell::new(s.chore),
            Cell::new(s.docs),
            Cell::new(s.test),
            Cell::new(s.other),
        ]);
    }

    println!("{table}");
}

fn cell_bold(text: &str) -> Cell {
    Cell::new(text).add_attribute(comfy_table::Attribute::Bold)
}
