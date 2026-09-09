use crossterm::{
    cursor::{MoveDown, MoveToColumn, MoveUp, RestorePosition, SavePosition},
    execute,
    style::Print,
    terminal::{Clear, ClearType},
};
use std::io::{self, Write};

/// A terminal region with a fixed anchor and height.
///
/// Once reserved, rendering never moves below the region. The terminal's
/// saved cursor position is the anchor: this keeps redraws correct even when
/// work performed between frames moves the cursor elsewhere.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransientRegion {
    height: usize,
}

impl TransientRegion {
    pub(crate) fn new(height: usize) -> Self {
        Self { height }
    }

    #[cfg(test)]
    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn reserve<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let count = self.cursor_count()?;
        if count == 0 {
            return Ok(());
        }

        // Reservation happens in cooked mode.  Explicit CRLF creates the
        // space once, including when the cursor is near the bottom edge;
        // subsequent redraws stay inside these rows.
        for _ in 0..self.height {
            writer.write_all(b"\r\n")?;
        }
        execute!(writer, MoveUp(count), MoveToColumn(0), SavePosition)
    }

    pub(crate) fn render<W: Write>(&self, writer: &mut W, frame: &[String]) -> io::Result<()> {
        if frame.len() > self.height {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "transient frame exceeds its reserved region",
            ));
        }
        let count = self.cursor_count()?;
        if count == 0 {
            return Ok(());
        }

        execute!(writer, RestorePosition)?;
        for index in 0..self.height {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            if let Some(line) = frame.get(index) {
                execute!(writer, Print(line))?;
            }
            if index + 1 < self.height {
                execute!(writer, MoveDown(1))?;
            }
        }
        execute!(writer, MoveUp(count - 1), MoveToColumn(0), RestorePosition)
    }

    /// Clears the owned canvas and returns to its anchor for the next view.
    ///
    /// This is the transition operation: it deliberately does not advance
    /// below the reserved canvas, so the next renderer can reuse the same
    /// terminal position without leaving a blank gap in scrollback.
    pub(crate) fn clear_and_release_at_anchor<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.clear_at_anchor(writer)
    }

    /// Preserves the meaningful part of the current frame and places the
    /// cursor immediately below it.
    ///
    /// The region may be larger than the final frame because transient
    /// activity needs bounded spare rows. Those spare rows are cleared before
    /// the cursor is positioned below the committed frame.
    pub(crate) fn commit<W: Write>(
        &self,
        writer: &mut W,
        meaningful_height: usize,
    ) -> io::Result<()> {
        if meaningful_height > self.height {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "committed frame exceeds its reserved region",
            ));
        }
        let count = self.cursor_count()?;
        if count == 0 {
            return Ok(());
        }

        execute!(writer, RestorePosition)?;
        let unused = self.height - meaningful_height;
        if unused == 0 {
            execute!(writer, MoveDown(count), MoveToColumn(0))
        } else {
            execute!(writer, MoveDown(meaningful_height as u16))?;
            for index in 0..unused {
                execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                if index + 1 < unused {
                    execute!(writer, MoveDown(1))?;
                }
            }
            execute!(writer, MoveUp((unused - 1) as u16), MoveToColumn(0))
        }
    }

    /// Clears the owned canvas and leaves the cursor below its full reserved
    /// height. This is used when a temporary UI is cancelled and no final
    /// frame is being committed (for example, the launcher).
    pub(crate) fn clear_and_finish<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let count = self.cursor_count()?;
        if count == 0 {
            return Ok(());
        }
        self.clear_at_anchor(writer)?;
        // Reservation guarantees that this is the line immediately below the
        // owned region, never a new scroll-producing line.
        execute!(writer, MoveDown(count), MoveToColumn(0))
    }

    fn clear_at_anchor<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let count = self.cursor_count()?;
        if count == 0 {
            return Ok(());
        }
        execute!(writer, RestorePosition)?;
        for index in 0..self.height {
            execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            if index + 1 < self.height {
                execute!(writer, MoveDown(1))?;
            }
        }
        execute!(writer, MoveUp(count - 1), MoveToColumn(0), RestorePosition)
    }

    fn cursor_count(&self) -> io::Result<u16> {
        self.height.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "transient region is too tall")
        })
    }
}

/// Replaces one owned transient frame without touching lines above it.
#[cfg(test)]
pub(crate) fn write_owned_frame<W: Write>(
    writer: &mut W,
    previous_line_count: usize,
    frame: &[String],
) -> usize {
    let region = TransientRegion::new(previous_line_count.max(frame.len()));
    let _ = region.render(writer, frame);
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

        assert!(output.matches("\x1b[1G").count() >= 3);
        assert!(output.matches("\x1b[1B").count() >= 2);
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
        let expected = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let move_down = if index + 1 < rows.len() { "\x1b[1B" } else { "" };
                format!("\x1b[1G\x1b[2K{row}{move_down}")
            })
            .collect::<String>();
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
    fn reserved_region_keeps_the_cursor_at_its_anchor() {
        let mut output = Cursor::new(Vec::new());
        let region = TransientRegion::new(4);
        region.reserve(&mut output).expect("reserve rows");
        region.render(&mut output, &frame(&["one"])).expect("render short frame");
        region
            .render(&mut output, &frame(&["one", "two", "three", "four"]))
            .expect("render full frame");
        region.render(&mut output, &frame(&["final"])).expect("render replacement");

        let output = output.into_inner();
        let screen = replay(&output);
        assert_eq!(screen.first().map(String::as_str), Some("final"));
        assert!(screen.iter().skip(1).all(|line| line.is_empty()), "screen: {screen:?}");
        assert!(output.ends_with(b"\x1b8"));
    }

    #[test]
    fn displaced_cursor_during_provider_work_does_not_corrupt_replacement() {
        let mut output = Cursor::new(Vec::new());
        let region = TransientRegion::new(1);
        region.reserve(&mut output).expect("reserve row");
        region
            .render(&mut output, &frame(&["⠹ Checking software sources…"]))
            .expect("render checking state");

        // Model a graphical terminal or provider-side output displacing the
        // cursor while the transient region is active.
        execute!(&mut output, MoveDown(4), MoveToColumn(0)).expect("displace cursor");
        region.clear_and_release_at_anchor(&mut output).expect("clear displaced transient state");
        write!(output, "FINAL_RESULT").expect("write final result");

        let screen = replay(&output.into_inner());
        assert_eq!(screen.first().map(String::as_str), Some("FINAL_RESULT"));
        assert!(screen.iter().skip(1).all(|line| line.is_empty()), "screen: {screen:?}");
    }

    #[test]
    fn release_returns_to_anchor_without_a_blank_gap() {
        let mut output = Cursor::new(Vec::new());
        let region = TransientRegion::new(4);
        region.reserve(&mut output).expect("reserve rows");
        region
            .render(&mut output, &frame(&["ORBIS", "identity", "signature", "tagline"]))
            .expect("render identity");
        region.clear_and_release_at_anchor(&mut output).expect("release identity region");
        write!(output, "◈ ORBIS // REFRESH").expect("write replacement heading");

        let screen = replay(&output.into_inner());
        assert_eq!(screen.first().map(String::as_str), Some("◈ ORBIS // REFRESH"));
        assert!(screen.iter().skip(1).all(|line| line.is_empty()), "screen: {screen:?}");
    }

    #[test]
    fn commit_places_shell_prompt_below_meaningful_final_frame() {
        let mut output = Cursor::new(Vec::new());
        let region = TransientRegion::new(15);
        region.reserve(&mut output).expect("reserve rows");
        let final_frame = frame(&["Ubuntu repositories", "Flatpak · system", "summary"]);
        region.render(&mut output, &final_frame).expect("render final frame");
        region.commit(&mut output, final_frame.len()).expect("commit final frame");
        write!(output, "SHELL_PROMPT").expect("write shell prompt");

        let screen = replay(&output.into_inner());
        assert_eq!(screen.first().map(String::as_str), Some("Ubuntu repositories"));
        assert_eq!(screen.get(1).map(String::as_str), Some("Flatpak · system"));
        assert_eq!(screen.get(2).map(String::as_str), Some("summary"));
        assert_eq!(screen.get(3).map(String::as_str), Some("SHELL_PROMPT"));
        assert!(screen.iter().skip(4).all(|line| line.is_empty()), "screen: {screen:?}");
    }

    #[test]
    fn commit_clears_unused_reserved_rows_and_repeated_commands_do_not_overlap() {
        let mut output = Cursor::new(Vec::new());
        let first = TransientRegion::new(15);
        first.reserve(&mut output).expect("reserve first command");
        first.render(&mut output, &frame(&["first", "summary"])).expect("render first");
        first.commit(&mut output, 2).expect("commit first");

        let second = TransientRegion::new(15);
        second.reserve(&mut output).expect("reserve second command");
        second.render(&mut output, &frame(&["second", "summary"])).expect("render second");
        second.commit(&mut output, 2).expect("commit second");
        write!(output, "SHELL_PROMPT").expect("write shell prompt");

        let screen = replay(&output.into_inner());
        assert_eq!(screen.first().map(String::as_str), Some("first"));
        assert_eq!(screen.get(1).map(String::as_str), Some("summary"));
        assert_eq!(screen.get(2).map(String::as_str), Some("second"));
        assert_eq!(screen.get(3).map(String::as_str), Some("summary"));
        assert_eq!(screen.get(4).map(String::as_str), Some("SHELL_PROMPT"));
        assert!(screen.iter().skip(5).all(|line| line.is_empty()), "screen: {screen:?}");
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
        let expected = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let move_down = if index + 1 < rows.len() { "\x1b[1B" } else { "" };
                format!("\x1b[1G\x1b[2K{row}{move_down}")
            })
            .collect::<String>();
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
        let mut saved_position = (0usize, 0usize);
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'7') {
                saved_position = (row, column);
                index += 2;
            } else if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'8') {
                (row, column) = saved_position;
                while rows.len() <= row {
                    rows.push(Vec::new());
                }
                index += 2;
            } else if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'[') {
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
