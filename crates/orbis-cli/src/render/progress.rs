use crate::render::theme::{StageState, Theme, Token};
use orbis_core::{
    models::PackageSource,
    progress::{ExecutionStage, OperationEvent, OperationHeader, OutputStream, ProgressObserver},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

struct ProgressState {
    current_stage: ExecutionStage,
    output_lines: VecDeque<String>,
    output_streams: VecDeque<OutputStream>,
    spinner_index: usize,
    running: bool,
}

pub(crate) struct PlainProgressRenderer {
    theme: Theme,
    header: OperationHeader,
    stages: &'static [ExecutionStage],
    state: Arc<Mutex<ProgressState>>,
    // Kept as a small inspection surface for tests and future plain renderers.
    // The authoritative live state remains in `state` so the ticker and provider
    // callbacks always render the same bounded buffer.
    output_lines: Mutex<VecDeque<String>>,
    spinner_started: AtomicBool,
    is_tty: bool,
    animate: bool,
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
        let animate = is_tty
            && std::env::var_os("REDUCE_MOTION").is_none()
            && std::env::var("TERM").map(|term| term != "dumb").unwrap_or(true);
        Self {
            theme,
            header,
            stages,
            state: Arc::new(Mutex::new(ProgressState {
                current_stage,
                output_lines: VecDeque::new(),
                output_streams: VecDeque::new(),
                spinner_index: 0,
                running: true,
            })),
            output_lines: Mutex::new(VecDeque::new()),
            spinner_started: AtomicBool::new(false),
            is_tty,
            animate,
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
            self.theme.paint(friendly_source(self.header.source), Token::Provider),
            self.header.scope
        );
        eprintln!("  {:<13} {}\n", "Privilege", privilege);

        if self.is_tty {
            self.redraw();
            self.start_spinner();
        }
    }

    fn start_spinner(&self) {
        if !self.animate || self.spinner_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = Arc::clone(&self.state);
        let theme = self.theme;
        let stages = self.stages;
        thread::spawn(move || {
            while state.lock().map(|state| state.running).unwrap_or(false) {
                thread::sleep(Duration::from_millis(100));
                let running = if let Ok(mut state_guard) = state.lock() {
                    if !state_guard.running {
                        false
                    } else {
                        state_guard.spinner_index = state_guard.spinner_index.wrapping_add(1);
                        true
                    }
                } else {
                    false
                };
                if !running {
                    break;
                }
                let _guard = output_lock();
                draw_tty(theme, stages, &state);
            }
        });
    }

    fn redraw(&self) {
        if !self.is_tty {
            return;
        }
        let _guard = output_lock();
        draw_tty(self.theme, self.stages, &self.state);
    }

    fn clear(&self) {
        let _guard = output_lock();
        let state = self.state.lock().expect("progress state lock poisoned");
        let mut total_lines = self.stages.len();
        if !state.current_stage.is_terminal() {
            total_lines += state.output_lines.len();
        }
        drop(state);
        for _ in 0..total_lines {
            eprint!("\x1b[1A\x1b[2K");
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
                if let Ok(mut state) = self.state.lock() {
                    state.current_stage = *stage;
                    state.output_lines.clear();
                    state.output_streams.clear();
                }
                self.output_lines.lock().expect("output lines lock poisoned").clear();

                if self.is_tty {
                    self.redraw();
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
                        stage_label(*stage)
                    );
                }
            }
            OperationEvent::ProviderOutput(line) => {
                let content = sanitize_output(&line.content);
                if self.is_tty {
                    self.clear();
                }
                let mut lines = self.state.lock().expect("progress state lock poisoned");

                if lines.output_lines.len() >= self.max_output_lines {
                    lines.output_lines.pop_front();
                    lines.output_streams.pop_front();
                }
                lines.output_lines.push_back(content.clone());
                lines.output_streams.push_back(line.stream);
                let mut inspected = self.output_lines.lock().expect("output lines lock poisoned");
                if inspected.len() >= self.max_output_lines {
                    inspected.pop_front();
                }
                inspected.push_back(content.clone());
                drop(inspected);

                if self.is_tty {
                    drop(lines);
                    self.redraw();
                } else {
                    let pipe = if self.theme.unicode { "┊" } else { "|" };
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(pipe, Token::Muted),
                        self.theme.paint(&content, Token::Muted)
                    );
                }
            }
            OperationEvent::ProviderStarted { .. }
            | OperationEvent::ProviderFinished { .. }
            | OperationEvent::ProviderInventory { .. } => {}
            OperationEvent::Finished { stage, message, .. } => {
                if self.is_tty {
                    self.clear();
                }
                if let Ok(mut state) = self.state.lock() {
                    state.current_stage = *stage;
                    state.output_lines.clear();
                    state.output_streams.clear();
                    state.running = false;
                }
                self.output_lines.lock().expect("output lines lock poisoned").clear();

                if self.is_tty {
                    self.redraw();
                }
                if let Some(msg) = message {
                    eprintln!(
                        "\n  {} {}",
                        self.theme.paint("Result", Token::Primary),
                        sanitize_output(msg)
                    );
                }
            }
            OperationEvent::Warning { message } => {
                let message = sanitize_output(message);
                if let Ok(mut state) = self.state.lock() {
                    if state.output_lines.len() >= self.max_output_lines {
                        state.output_lines.pop_front();
                        state.output_streams.pop_front();
                    }
                    state.output_lines.push_back(format!("! {message}"));
                    state.output_streams.push_back(OutputStream::Stderr);
                }
                let mut inspected = self.output_lines.lock().expect("output lines lock poisoned");
                if inspected.len() >= self.max_output_lines {
                    inspected.pop_front();
                }
                inspected.push_back(format!("! {message}"));
                drop(inspected);
                if self.is_tty {
                    self.redraw();
                } else {
                    eprintln!("  {} {}", self.theme.paint("Warning", Token::Caution), message);
                }
            }
        }
    }
}

fn output_lock() -> MutexGuard<'static, ()> {
    static OUTPUT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    OUTPUT_LOCK.get_or_init(|| Mutex::new(())).lock().expect("output lock poisoned")
}

fn draw_tty(theme: Theme, stages: &'static [ExecutionStage], state: &Arc<Mutex<ProgressState>>) {
    let Ok(state) = state.lock() else { return };
    let current = state.current_stage;
    let mut found_current = false;
    for &stage in stages {
        let is_current = stage == current;
        if is_current {
            found_current = true;
        }
        let is_past = !found_current;
        let stage_state = if is_current {
            if stage.is_terminal() { StageState::Done } else { StageState::Active }
        } else if is_past {
            StageState::Done
        } else {
            StageState::Pending
        };
        let token = match stage_state {
            StageState::Done => Token::Positive,
            StageState::Active => Token::Caution,
            StageState::Pending => Token::Muted,
        };
        let mark = if is_current && !stage.is_terminal() {
            spinner(theme, state.spinner_index)
        } else {
            theme.stage_mark(stage_state)
        };
        let label_token =
            if stage_state == StageState::Done { Token::Foreground } else { Token::Muted };
        if is_current && !stage.is_terminal() {
            eprintln!(
                "  {} {} …",
                theme.paint(mark, token),
                theme.paint(stage_label(stage), Token::Foreground)
            );
            for line in &state.output_lines {
                let pipe = if theme.unicode { "┊" } else { "|" };
                eprintln!(
                    "  {} {}",
                    theme.paint(pipe, Token::Muted),
                    theme.paint(line, Token::Muted)
                );
            }
        } else {
            eprintln!(
                "  {} {}",
                theme.paint(mark, token),
                theme.paint(stage_label(stage), label_token)
            );
        }
    }
}

fn spinner(theme: Theme, index: usize) -> &'static str {
    if theme.unicode {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[index % FRAMES.len()]
    } else {
        ["|", "/", "-", "\\"][index % 4]
    }
}

fn stage_label(stage: ExecutionStage) -> &'static str {
    match stage {
        ExecutionStage::Preparing => "Preparing",
        ExecutionStage::AwaitingConfirmation => "Ready to continue",
        ExecutionStage::Authenticating => "Getting permission",
        ExecutionStage::Executing => "Working",
        ExecutionStage::Verifying => "Checking changes",
        ExecutionStage::SavingResult => "Finishing up",
        ExecutionStage::Completed => "Done",
        ExecutionStage::Failed => "Could not finish",
    }
}

fn sanitize_output(value: &str) -> String {
    let latest = value.rsplit('\r').next().unwrap_or(value);
    latest
        .chars()
        .map(|character| if character == '\t' || !character.is_control() { character } else { '�' })
        .collect()
}

fn friendly_source(source: PackageSource) -> &'static str {
    match source {
        PackageSource::Apt => "Ubuntu/Debian repositories",
        PackageSource::Flatpak => "Flatpak apps",
        PackageSource::Snap => "Snap Store",
        PackageSource::Cargo => "Rust tools",
        PackageSource::Npm | PackageSource::Pnpm => "Node.js tools",
        PackageSource::Uv | PackageSource::Pipx => "Python tools",
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

    #[test]
    fn test_output_sanitizes_carriage_returns_and_retains_streams() {
        let renderer = PlainProgressRenderer::new(
            Theme::test(80),
            create_header(),
            ExecutionStage::transaction_stages(),
            false,
            4,
        );
        renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stdout,
            content: "old progress\rnew progress\x1b[2K".into(),
        }));
        renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stderr,
            content: "warning".into(),
        }));
        let lines = renderer.output_lines.lock().expect("output lines").clone();
        assert_eq!(lines[0], "new progress�[2K");
        assert_eq!(lines[1], "warning");
        let state = renderer.state.lock().expect("progress state");
        assert_eq!(
            state.output_streams.as_slices().0,
            &[OutputStream::Stdout, OutputStream::Stderr]
        );
    }

    #[test]
    fn test_spinner_has_unicode_and_ascii_frames() {
        let mut unicode = Theme::test(80);
        unicode.unicode = true;
        let ascii = Theme::test(80);
        assert_eq!(spinner(unicode, 0), "⠋");
        assert_eq!(spinner(unicode, 11), "⠙");
        assert_eq!(spinner(ascii, 0), "|");
        assert_eq!(spinner(ascii, 3), "\\");
    }
}
