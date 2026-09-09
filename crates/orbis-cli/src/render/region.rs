use crossterm::{
    cursor::{MoveDown, MoveToColumn, MoveUp},
    execute,
    terminal::{Clear, ClearType},
};
use std::io::Write;

/// Clears exactly the terminal lines most recently owned by a transient view.
/// The cursor is returned to the beginning of that region for the next frame.
pub(crate) fn clear_owned_frame<W: Write>(writer: &mut W, line_count: usize) {
    if line_count == 0 {
        return;
    }
    let count = line_count.min(u16::MAX as usize) as u16;
    let _ = execute!(writer, MoveUp(count), MoveToColumn(0));
    for index in 0..line_count {
        let _ = execute!(writer, Clear(ClearType::CurrentLine));
        if index + 1 < line_count {
            let _ = execute!(writer, MoveDown(1), MoveToColumn(0));
        }
    }
    let _ = execute!(writer, MoveUp(count.saturating_sub(1)), MoveToColumn(0));
}

/// Replaces one owned transient frame without touching lines above it.
pub(crate) fn write_owned_frame<W: Write>(
    writer: &mut W,
    previous_line_count: usize,
    frame: &[String],
) -> usize {
    clear_owned_frame(writer, previous_line_count);
    for line in frame {
        let _ = writeln!(writer, "{line}");
    }
    frame.len()
}
