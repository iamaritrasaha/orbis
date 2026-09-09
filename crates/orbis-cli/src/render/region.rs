use crossterm::{
    cursor::{MoveDown, MoveToColumn, MoveToNextLine, MoveUp},
    execute,
    style::Print,
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
    for index in 0..usize::from(count) {
        let _ = execute!(writer, Clear(ClearType::CurrentLine));
        if index + 1 < usize::from(count) {
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
        // Raw mode disables terminal output post-processing on Unix.  A bare
        // newline can therefore advance vertically without returning to
        // column zero, which corrupts multiline geometric frames.  Keep the
        // cursor contract explicit for every row instead.
        let _ = execute!(writer, MoveToColumn(0), Print(line), MoveToNextLine(1));
    }
    frame.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, process::Command};

    fn frame(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn frame_writer_explicitly_returns_to_column_zero_for_each_row() {
        let mut output = Cursor::new(Vec::new());
        write_owned_frame(&mut output, 0, &frame(&["one", "two", "three"]));
        let output = String::from_utf8(output.into_inner()).expect("UTF-8 ANSI stream");

        assert_eq!(output.matches("\x1b[1G").count(), 3);
        assert_eq!(output.matches("\x1b[1E").count(), 3);
        assert!(!output.contains("one\ntwo\nthree"));
    }

    #[test]
    fn ascii_wordmark_uses_the_same_cursor_safe_path() {
        let mut output = Cursor::new(Vec::new());
        let rows = [
            "+--+  +--+  +--+    |    +--+".to_owned(),
            "|  |  +--/  +--+    |    +--+".to_owned(),
            "+--+  |  \\  +--+    |    \\--+".to_owned(),
        ];
        write_owned_frame(&mut output, 0, &rows);
        let output = String::from_utf8(output.into_inner()).expect("UTF-8 ANSI stream");
        let expected = rows.iter().map(|row| format!("\x1b[1G{row}\x1b[1E")).collect::<String>();
        assert!(output.contains(&expected));
    }

    #[test]
    fn variable_height_frames_clear_only_the_previous_owned_region() {
        let mut output = Cursor::new(Vec::new());
        let mut owned = 0;
        for lines in [
            frame(&["one"]),
            frame(&["one", "two", "three", "four"]),
            frame(&["one"]),
            frame(&["one", "two", "three"]),
            frame(&["one", "two", "three", "four"]),
            frame(&["one", "two", "three"]),
        ] {
            owned = write_owned_frame(&mut output, owned, &lines);
            assert_eq!(owned, lines.len());
        }

        let output = output.into_inner();
        let output_text = String::from_utf8_lossy(&output);
        assert!(output_text.matches("\x1b[1G").count() >= 1 + 4 + 1 + 3 + 4 + 3);
        assert!(output_text.matches("\x1b[2K").count() >= 1 + 4 + 1 + 3 + 4);

        let screen = replay(&output);
        assert_eq!(screen.first().map(String::as_str), Some("one"));
        assert_eq!(screen.get(1).map(String::as_str), Some("two"));
        assert_eq!(screen.get(2).map(String::as_str), Some("three"));
        assert!(screen.iter().skip(3).all(|line| line.is_empty()), "screen: {screen:?}");
    }

    #[test]
    fn raw_mode_pty_keeps_each_wordmark_row_at_column_zero() {
        if Command::new("script").arg("--version").output().is_err() {
            eprintln!("skipping pseudo-TTY check: script is unavailable");
            return;
        }

        let executable = std::env::current_exe().expect("test executable");
        let command = format!(
            "{} --exact render::region::tests::raw_mode_pty_child --nocapture",
            shell_quote(&executable.to_string_lossy())
        );
        let output = Command::new("script")
            .args(["-qefc", &command, "/dev/null"])
            .env("ORBIS_RAW_MODE_PTY_CHILD", "1")
            .output()
            .expect("run pseudo-TTY harness");
        assert!(output.status.success(), "pseudo-TTY child failed: {output:?}");

        let transcript = String::from_utf8_lossy(&output.stdout);
        let rows = [
            "╭──╮  ╭──╮  ╭──╮    ╷    ╭──╮",
            "│  │  ├──╯  ├──┤    │    ╰──╮",
            "╰──╯  ╵  ╲  ╰──╯    ╵    ╰──╯",
        ];
        let expected = rows.iter().map(|row| format!("\x1b[1G{row}\x1b[1E")).collect::<String>();
        assert!(transcript.contains(&expected), "raw-mode transcript: {transcript:?}");
    }

    #[test]
    fn raw_mode_pty_child() {
        if std::env::var_os("ORBIS_RAW_MODE_PTY_CHILD").is_none() {
            return;
        }

        crossterm::terminal::enable_raw_mode().expect("enable raw mode on pseudo-TTY");
        let mut stdout = std::io::stdout();
        let rows = [
            "╭──╮  ╭──╮  ╭──╮    ╷    ╭──╮".to_owned(),
            "│  │  ├──╯  ├──┤    │    ╰──╮".to_owned(),
            "╰──╯  ╵  ╲  ╰──╯    ╵    ╰──╯".to_owned(),
        ];
        write_owned_frame(&mut stdout, 0, &rows);
        stdout.flush().expect("flush pseudo-TTY frame");
        crossterm::terminal::disable_raw_mode().expect("restore raw mode");
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    fn replay(bytes: &[u8]) -> Vec<String> {
        let mut rows: Vec<Vec<char>> = vec![Vec::new()];
        let (mut row, mut column) = (0usize, 0usize);
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
                let mut end = index + 2;
                while end < bytes.len() && !bytes[end].is_ascii_alphabetic() {
                    end += 1;
                }
                let command = bytes.get(end).copied().unwrap_or_default() as char;
                let parameter = std::str::from_utf8(&bytes[index + 2..end])
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1);
                match command {
                    'A' => row = row.saturating_sub(parameter),
                    'B' | 'E' => {
                        row += parameter;
                        if command == 'E' {
                            column = 0;
                        }
                    }
                    'G' => column = parameter.saturating_sub(1),
                    'K' => rows[row].clear(),
                    _ => {}
                }
                while rows.len() <= row {
                    rows.push(Vec::new());
                }
                index = end.saturating_add(1);
            } else if bytes[index] == b'\n' {
                row += 1;
                column = 0;
                while rows.len() <= row {
                    rows.push(Vec::new());
                }
                index += 1;
            } else if bytes[index] == b'\r' {
                column = 0;
                index += 1;
            } else {
                let value = std::str::from_utf8(&bytes[index..]).expect("UTF-8 frame stream");
                let character = value.chars().next().expect("frame character");
                while rows[row].len() <= column {
                    rows[row].push(' ');
                }
                rows[row][column] = character;
                column += 1;
                index += character.len_utf8();
            }
        }
        rows.into_iter().map(|line| line.into_iter().collect()).collect()
    }
}
