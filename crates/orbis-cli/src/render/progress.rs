use crate::render::theme::{StageState, Theme, Token};
use orbis_core::progress::{ExecutionStage, OperationEvent, OperationHeader, ProgressObserver};
use std::collections::VecDeque;
use std::sync::Mutex;

pub(crate) struct PlainProgressRenderer {
    theme: Theme,
    header: OperationHeader,
    stages: &'static [ExecutionStage],
    current_stage: Mutex<ExecutionStage>,
    output_lines: Mutex<VecDeque<String>>,
    is_tty: bool,
    max_output_lines: usize,
}

impl PlainProgressRenderer {
    pub(crate) fn new(
        theme: Theme,
        header: OperationHeader,
        stages: &'static [ExecutionStage],
        is_tty: bool,
        max_output_lines: usize,
    ) -> Self {
        let current_stage = stages.first().copied().unwrap_or(ExecutionStage::Preparing);
        Self {
            theme,
            header,
            stages,
            current_stage: Mutex::new(current_stage),
            output_lines: Mutex::new(VecDeque::new()),
            is_tty,
            max_output_lines,
        }
    }

    pub(crate) fn print_header(&self) {
        let privilege = if self.header.privileged { "administrator" } else { "user" };
        eprintln!(
            "{} {}",
            self.theme.paint(self.theme.mark(Token::Primary), Token::Primary),
            self.theme.paint(&self.header.title, Token::Primary)
        );
        eprintln!(
            "  {:<13} {} · {}",
            "Source",
            self.theme.paint(self.header.source.label(), Token::Provider),
            self.header.scope
        );
        eprintln!("  {:<13} {}\n", "Privilege", privilege);

        if self.is_tty {
            self.draw();
        }
    }

    fn clear(&self) {
        let output_len = self.output_lines.lock().unwrap().len();
        let current = *self.current_stage.lock().unwrap();
        let mut total_lines = self.stages.len();
        if !current.is_terminal() {
            total_lines += output_len;
        }
        for _ in 0..total_lines {
            eprint!("\x1b[1A\x1b[2K");
        }
    }

    fn draw(&self) {
        let output_lines = self.output_lines.lock().unwrap();
        let current = *self.current_stage.lock().unwrap();
        let mut found_current = false;

        for &stage in self.stages {
            let is_current = stage == current;
            if is_current {
                found_current = true;
            }
            let is_past = !found_current;

            let state = if is_current {
                if stage.is_terminal() { StageState::Done } else { StageState::Active }
            } else if is_past {
                StageState::Done
            } else {
                StageState::Pending
            };

            let token = match state {
                StageState::Done => Token::Positive,
                StageState::Active => Token::Caution,
                StageState::Pending => Token::Muted,
            };

            let mark = self.theme.paint(self.theme.stage_mark(state), token);
            let label_token =
                if is_past || state == StageState::Done { Token::Foreground } else { Token::Muted };

            if is_current && !stage.is_terminal() {
                eprintln!("  {} {} …", mark, self.theme.paint(stage.label(), Token::Foreground));
                for line in output_lines.iter() {
                    let pipe = if self.theme.unicode { "┊" } else { "|" };
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(pipe, Token::Muted),
                        self.theme.paint(line, Token::Muted)
                    );
                }
            } else {
                eprintln!("  {} {}", mark, self.theme.paint(stage.label(), label_token));
            }
        }
    }
}

impl ProgressObserver for PlainProgressRenderer {
    fn on_event(&self, event: &OperationEvent) {
        match event {
            OperationEvent::StageChanged { stage } => {
                if self.is_tty {
                    self.clear();
                }
                *self.current_stage.lock().unwrap() = *stage;
                self.output_lines.lock().unwrap().clear();

                if self.is_tty {
                    self.draw();
                } else {
                    let state =
                        if stage.is_terminal() { StageState::Done } else { StageState::Active };
                    let token = match state {
                        StageState::Done => Token::Positive,
                        StageState::Active => Token::Caution,
                        StageState::Pending => Token::Muted,
                    };
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(self.theme.stage_mark(state), token),
                        stage.label()
                    );
                }
            }
            OperationEvent::ProviderOutput(line) => {
                let mut lines = self.output_lines.lock().unwrap();
                if self.is_tty {
                    let current = *self.current_stage.lock().unwrap();
                    let mut total_lines = self.stages.len();
                    if !current.is_terminal() {
                        total_lines += lines.len();
                    }
                    for _ in 0..total_lines {
                        eprint!("\x1b[1A\x1b[2K");
                    }
                }

                if lines.len() >= self.max_output_lines {
                    lines.pop_front();
                }
                lines.push_back(line.content.clone());

                if self.is_tty {
                    drop(lines);
                    self.draw();
                } else {
                    let pipe = if self.theme.unicode { "┊" } else { "|" };
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(pipe, Token::Muted),
                        self.theme.paint(&line.content, Token::Muted)
                    );
                }
            }
            OperationEvent::Finished { stage, message, .. } => {
                if self.is_tty {
                    self.clear();
                }
                *self.current_stage.lock().unwrap() = *stage;
                self.output_lines.lock().unwrap().clear();

                if self.is_tty {
                    self.draw();
                }
                if let Some(msg) = message {
                    eprintln!("\n  {} {}", self.theme.paint("Result", Token::Primary), msg);
                }
            }
            OperationEvent::Warning { message } => {
                eprintln!("  {} {}", self.theme.paint("Warning", Token::Caution), message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbis_core::models::PackageSource;
    use orbis_core::progress::{OutputLine, OutputStream};

    fn create_header() -> OperationHeader {
        OperationHeader {
            title: "Installing btop".into(),
            target: "btop".into(),
            source: PackageSource::Apt,
            scope: "system scope".into(),
            privileged: true,
        }
    }

    #[test]
    fn test_print_header_non_tty() {
        let theme = Theme::test(80);
        let header = create_header();
        let stages = ExecutionStage::transaction_stages();
        let renderer = PlainProgressRenderer::new(theme, header, stages, false, 8);
        renderer.print_header();
    }

    #[test]
    fn test_stage_progression() {
        let theme = Theme::test(80);
        let header = create_header();
        let stages = ExecutionStage::transaction_stages();
        let renderer = PlainProgressRenderer::new(theme, header, stages, true, 8);

        renderer.on_event(&OperationEvent::StageChanged { stage: ExecutionStage::Authenticating });
        renderer.on_event(&OperationEvent::StageChanged { stage: ExecutionStage::Executing });

        renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stdout,
            content: "Reading package lists...".into(),
        }));

        renderer.on_event(&OperationEvent::Finished {
            stage: ExecutionStage::Completed,
            message: Some("Successfully installed btop".into()),
            operation_id: "test-id".into(),
        });
    }

    #[test]
    fn test_output_bounding() {
        let theme = Theme::test(80);
        let header = create_header();
        let stages = ExecutionStage::transaction_stages();
        let renderer = PlainProgressRenderer::new(theme, header, stages, false, 2);

        renderer.on_event(&OperationEvent::StageChanged { stage: ExecutionStage::Executing });

        renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stdout,
            content: "Line 1".into(),
        }));
        renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stdout,
            content: "Line 2".into(),
        }));
        renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stdout,
            content: "Line 3".into(),
        }));

        let lines = renderer.output_lines.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Line 2");
        assert_eq!(lines[1], "Line 3");
    }
}
