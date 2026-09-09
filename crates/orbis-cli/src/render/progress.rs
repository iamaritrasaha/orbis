use crate::render::theme::{StageState, Theme, Token};
use crossterm::{
    cursor::{MoveDown, MoveToColumn, MoveUp},
    execute,
    terminal::{Clear, ClearType},
};
use orbis_core::{
    models::PackageSource,
    progress::{ExecutionStage, OperationEvent, OperationHeader, OutputStream, ProgressObserver},
};
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

struct ActivityLine {
    key: String,
    text: String,
}

struct ProgressState {
    current_stage: ExecutionStage,
    output_lines: VecDeque<String>,
    output_streams: VecDeque<OutputStream>,
    activity_lines: VecDeque<ActivityLine>,
    spinner_index: usize,
    running: bool,
    rendered_line_count: usize,
}

pub(crate) struct PlainProgressRenderer {
    theme: Theme,
    header: OperationHeader,
    stages: &'static [ExecutionStage],
    state: Arc<Mutex<ProgressState>>,
    // Kept as a small inspection surface for tests and future plain renderers.
    output_lines: Mutex<VecDeque<String>>,
    spinner_started: AtomicBool,
    spinner_thread: Mutex<Option<JoinHandle<()>>>,
    is_tty: bool,
    animate: bool,
    max_output_lines: usize,
    refresh: bool,
    preserve_raw_output: bool,
}

impl PlainProgressRenderer {
    #[cfg(test)]
    pub(crate) fn new(
        theme: Theme,
        header: OperationHeader,
        stages: &'static [ExecutionStage],
        is_tty: bool,
        max_output_lines: usize,
    ) -> Self {
        Self::with_mode(theme, header, stages, is_tty, max_output_lines, false, true)
    }

    pub(crate) fn new_with_output_policy(
        theme: Theme,
        header: OperationHeader,
        stages: &'static [ExecutionStage],
        is_tty: bool,
        max_output_lines: usize,
        preserve_raw_output: bool,
    ) -> Self {
        Self::with_mode(theme, header, stages, is_tty, max_output_lines, false, preserve_raw_output)
    }

    pub(crate) fn new_refresh_with_output_policy(
        theme: Theme,
        header: OperationHeader,
        stages: &'static [ExecutionStage],
        is_tty: bool,
        max_output_lines: usize,
        preserve_raw_output: bool,
    ) -> Self {
        Self::with_mode(theme, header, stages, is_tty, max_output_lines, true, preserve_raw_output)
    }

    fn with_mode(
        theme: Theme,
        header: OperationHeader,
        stages: &'static [ExecutionStage],
        is_tty: bool,
        max_output_lines: usize,
        refresh: bool,
        preserve_raw_output: bool,
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
                activity_lines: VecDeque::new(),
                spinner_index: 0,
                running: true,
                rendered_line_count: 0,
            })),
            output_lines: Mutex::new(VecDeque::new()),
            spinner_started: AtomicBool::new(false),
            spinner_thread: Mutex::new(None),
            is_tty,
            animate,
            max_output_lines,
            refresh,
            preserve_raw_output,
        }
    }

    pub(crate) fn print_header(&self) {
        {
            let _guard = output_lock();
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
                draw_tty(
                    self.theme,
                    self.stages,
                    &self.state,
                    self.header.privileged,
                    self.refresh,
                );
            }
        }
        if self.is_tty {
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
        let privileged = self.header.privileged;
        let refresh = self.refresh;
        let handle = thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(100));
                let should_draw = if let Ok(mut state_guard) = state.lock() {
                    if !state_guard.running {
                        false
                    } else {
                        state_guard.spinner_index = state_guard.spinner_index.wrapping_add(1);
                        true
                    }
                } else {
                    false
                };
                if !should_draw {
                    break;
                }
                let _guard = output_lock();
                let still_running =
                    state.lock().map(|state_guard| state_guard.running).unwrap_or(false);
                if !still_running {
                    break;
                }
                draw_tty(theme, stages, &state, privileged, refresh);
            }
        });
        *self.spinner_thread.lock().expect("spinner thread lock poisoned") = Some(handle);
    }

    fn redraw(&self) {
        if !self.is_tty {
            return;
        }
        let _guard = output_lock();
        draw_tty(self.theme, self.stages, &self.state, self.header.privileged, self.refresh);
    }

    fn finish(&self, stage: ExecutionStage) {
        // Stop the ticker before taking ownership of the final frame. The ticker
        // checks this flag again after taking the output lock, so it cannot paint
        // an old frame after the final one.
        if let Ok(mut state) = self.state.lock() {
            state.current_stage = stage;
            state.output_lines.clear();
            state.output_streams.clear();
            state.activity_lines.clear();
            state.running = false;
        }
        self.output_lines.lock().expect("output lines lock poisoned").clear();

        if self.is_tty {
            let _guard = output_lock();
            draw_tty(self.theme, self.stages, &self.state, self.header.privileged, self.refresh);
            drop(_guard);
        }

        if let Some(handle) =
            self.spinner_thread.lock().expect("spinner thread lock poisoned").take()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for PlainProgressRenderer {
    fn drop(&mut self) {
        let was_running = if let Ok(mut state) = self.state.lock() {
            let was_running = state.running;
            state.running = false;
            was_running
        } else {
            false
        };

        if self.is_tty && was_running {
            let _guard = output_lock();
            let mut stderr = io::stderr();
            if let Ok(mut state) = self.state.lock() {
                clear_owned_frame(&mut stderr, state.rendered_line_count);
                state.rendered_line_count = 0;
            }
        }

        if let Some(handle) =
            self.spinner_thread.get_mut().expect("spinner thread lock poisoned").take()
        {
            let _ = handle.join();
        }
    }
}

impl ProgressObserver for PlainProgressRenderer {
    fn on_event(&self, event: &OperationEvent) {
        if !matches!(event, OperationEvent::Finished { .. })
            && self.state.lock().map(|state| !state.running).unwrap_or(true)
        {
            return;
        }
        match event {
            OperationEvent::StageChanged { stage } => {
                if let Ok(mut state) = self.state.lock() {
                    state.current_stage = *stage;
                    state.output_lines.clear();
                    state.output_streams.clear();
                    state.activity_lines.clear();
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
                    let _guard = output_lock();
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(self.theme.stage_mark(state), token),
                        self.theme
                            .paint(display_stage_label(*stage, self.refresh), Token::Foreground)
                    );
                }
            }
            OperationEvent::ProviderOutput(line) => {
                let content = sanitize_output(&line.content);
                if let Ok(mut state) = self.state.lock() {
                    if state.output_lines.len() >= self.max_output_lines {
                        state.output_lines.pop_front();
                        state.output_streams.pop_front();
                    }
                    state.output_lines.push_back(content.clone());
                    state.output_streams.push_back(line.stream);
                    add_activity_line(
                        &mut state.activity_lines,
                        self.header.source,
                        &content,
                        self.refresh,
                        self.max_output_lines,
                    );
                }
                let mut inspected = self.output_lines.lock().expect("output lines lock poisoned");
                if inspected.len() >= self.max_output_lines {
                    inspected.pop_front();
                }
                inspected.push_back(content.clone());
                drop(inspected);

                if self.is_tty {
                    self.redraw();
                } else {
                    let pipe = if self.theme.unicode { "┊" } else { "|" };
                    let display_content = if self.preserve_raw_output {
                        content.clone()
                    } else {
                        compact_activity(self.header.source, &content, self.refresh)
                    };
                    let _guard = output_lock();
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(pipe, Token::Muted),
                        self.theme.paint(&display_content, Token::Muted)
                    );
                }
            }
            OperationEvent::ProviderStarted { .. }
            | OperationEvent::ProviderFinished { .. }
            | OperationEvent::ProviderInventory { .. } => {}
            OperationEvent::Finished { stage, message, .. } => {
                self.finish(*stage);
                if let Some(msg) = message {
                    let _guard = output_lock();
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
                    add_activity_line(
                        &mut state.activity_lines,
                        self.header.source,
                        &format!("! {message}"),
                        self.refresh,
                        self.max_output_lines,
                    );
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
                    let _guard = output_lock();
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

fn draw_tty(
    theme: Theme,
    stages: &'static [ExecutionStage],
    state: &Arc<Mutex<ProgressState>>,
    privileged: bool,
    refresh: bool,
) {
    let mut stderr = io::stderr();
    let Ok(mut state) = state.lock() else { return };
    draw_tty_to(&mut stderr, theme, stages, &mut state, privileged, refresh);
}

fn draw_tty_to<W: Write>(
    writer: &mut W,
    theme: Theme,
    stages: &'static [ExecutionStage],
    state: &mut ProgressState,
    privileged: bool,
    refresh: bool,
) {
    let frame = frame_lines(theme, stages, state, privileged, refresh);
    clear_owned_frame(writer, state.rendered_line_count);
    for line in &frame {
        let _ = writeln!(writer, "{line}");
    }
    state.rendered_line_count = frame.len();
}

fn clear_owned_frame<W: Write>(writer: &mut W, line_count: usize) {
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

fn frame_lines(
    theme: Theme,
    stages: &'static [ExecutionStage],
    state: &ProgressState,
    privileged: bool,
    refresh: bool,
) -> Vec<String> {
    let effective_current = match state.current_stage {
        ExecutionStage::Authenticating => ExecutionStage::Executing,
        stage => stage,
    };
    let current_index = stages.iter().position(|stage| *stage == effective_current);
    let terminal = state.current_stage.is_terminal();
    let mut lines = Vec::with_capacity(stages.len() + state.activity_lines.len() + 1);

    for (index, &stage) in stages.iter().enumerate() {
        if stage == ExecutionStage::Authenticating && !privileged {
            continue;
        }
        let stage_state = if terminal || (stage == ExecutionStage::Authenticating && privileged) {
            StageState::Done
        } else if current_index == Some(index) {
            StageState::Active
        } else if current_index.is_some_and(|current| index < current) {
            StageState::Done
        } else {
            StageState::Pending
        };
        let token = match stage_state {
            StageState::Done => Token::Positive,
            StageState::Active => Token::Caution,
            StageState::Pending => Token::Muted,
        };
        let mark = if stage_state == StageState::Active {
            spinner(theme, state.spinner_index)
        } else {
            theme.stage_mark(stage_state)
        };
        let label = display_stage_label(stage, refresh);
        let label_token =
            if stage_state == StageState::Pending { Token::Muted } else { Token::Foreground };
        lines.push(format!(
            "  {} {}{}",
            theme.paint(mark, token),
            theme.paint(label, label_token),
            if stage_state == StageState::Active { " …" } else { "" }
        ));

        if stage_state == StageState::Active {
            for activity in &state.activity_lines {
                let pipe = if theme.unicode { "┊" } else { "|" };
                lines.push(format!(
                    "  {} {}",
                    theme.paint(pipe, Token::Muted),
                    theme.paint(&activity.text, Token::Muted)
                ));
            }
        }
    }

    if terminal {
        let label = if state.current_stage == ExecutionStage::Completed {
            "Done"
        } else {
            "Could not finish"
        };
        let token = if state.current_stage == ExecutionStage::Completed {
            Token::Positive
        } else {
            Token::Caution
        };
        lines.push(format!(
            "  {} {}",
            theme.paint(theme.stage_mark(StageState::Done), token),
            theme.paint(label, Token::Foreground)
        ));
    }

    lines
}

fn add_activity_line(
    lines: &mut VecDeque<ActivityLine>,
    source: PackageSource,
    content: &str,
    refresh: bool,
    max_lines: usize,
) {
    let (key, text) = if refresh && source == PackageSource::Apt {
        apt_activity(content).unwrap_or_else(|| ("latest".into(), content.into()))
    } else {
        ("latest".into(), content.into())
    };
    if let Some(index) = lines.iter().position(|line| line.key == key) {
        lines.remove(index);
    }
    if lines.len() >= max_lines {
        lines.pop_front();
    }
    lines.push_back(ActivityLine { key, text });
}

fn compact_activity(source: PackageSource, content: &str, refresh: bool) -> String {
    if refresh
        && source == PackageSource::Apt
        && let Some((_, text)) = apt_activity(content)
    {
        return text;
    }
    if content.starts_with('!') { content.into() } else { "Provider activity updated".into() }
}

fn apt_activity(value: &str) -> Option<(String, String)> {
    let action = value.split_whitespace().next()?.split(':').next()?;
    let state = match action {
        "Hit" | "Ign" => "up to date",
        "Get" => "checking",
        "Err" => "error",
        _ => return None,
    };
    let url = value
        .split_whitespace()
        .find(|word| word.starts_with("http://") || word.starts_with("https://"))?;
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split(['/', ':'])
        .next()
        .filter(|host| !host.is_empty())?;
    Some((host.into(), format!("{host:<30} {state}")))
}

fn spinner(theme: Theme, index: usize) -> &'static str {
    if theme.unicode {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[index % FRAMES.len()]
    } else {
        ["|", "/", "-", "\\"][index % 4]
    }
}

fn display_stage_label(stage: ExecutionStage, refresh: bool) -> &'static str {
    match stage {
        ExecutionStage::Preparing => "Preparing",
        ExecutionStage::AwaitingConfirmation => "Ready to continue",
        ExecutionStage::Authenticating => "Permission granted",
        ExecutionStage::Executing if refresh => "Refreshing information",
        ExecutionStage::Executing => "Working",
        ExecutionStage::Verifying if refresh => "Checking result",
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

    fn state() -> ProgressState {
        ProgressState {
            current_stage: ExecutionStage::Executing,
            output_lines: VecDeque::new(),
            output_streams: VecDeque::new(),
            activity_lines: VecDeque::new(),
            spinner_index: 0,
            running: true,
            rendered_line_count: 0,
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

        for content in ["Line 1", "Line 2", "Line 3"] {
            renderer.on_event(&OperationEvent::ProviderOutput(OutputLine {
                stream: OutputStream::Stdout,
                content: content.into(),
            }));
        }

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

    #[test]
    fn spinner_ticks_rewrite_one_owned_frame() {
        let theme = Theme::test(80);
        let stages = ExecutionStage::transaction_stages();
        let mut progress = state();
        let mut capture = Vec::new();
        let mut visible = Vec::new();
        let mut owned_lines = 0;

        for index in 0..20 {
            progress.spinner_index = index;
            let frame = frame_lines(theme, stages, &progress, true, false);
            visible.splice(0..owned_lines, frame.clone());
            owned_lines = frame.len();
            draw_tty_to(&mut capture, theme, stages, &mut progress, true, false);
        }

        assert_eq!(owned_lines, visible.len());
        assert!(String::from_utf8_lossy(&capture).matches("\x1b[").count() >= 19);
        assert_eq!(visible.iter().filter(|line| line.contains("Preparing")).count(), 1);
        assert_eq!(visible.iter().filter(|line| line.contains("Finishing up")).count(), 1);
    }

    #[test]
    fn varying_frame_heights_clear_stale_lines() {
        let theme = Theme::test(80);
        let stages = ExecutionStage::transaction_stages();
        let mut progress = state();
        let mut capture = Vec::new();

        draw_tty_to(&mut capture, theme, stages, &mut progress, true, false);
        add_activity_line(
            &mut progress.activity_lines,
            PackageSource::Apt,
            "Get:1 http://archive.ubuntu.com/ubuntu",
            true,
            8,
        );
        draw_tty_to(&mut capture, theme, stages, &mut progress, true, true);
        add_activity_line(
            &mut progress.activity_lines,
            PackageSource::Apt,
            "Get:2 http://security.ubuntu.com/ubuntu",
            true,
            8,
        );
        draw_tty_to(&mut capture, theme, stages, &mut progress, true, true);
        progress.activity_lines.clear();
        draw_tty_to(&mut capture, theme, stages, &mut progress, true, true);
        progress.current_stage = ExecutionStage::Completed;
        progress.running = false;
        draw_tty_to(&mut capture, theme, stages, &mut progress, true, true);

        assert_eq!(progress.rendered_line_count, stages.len() + 1);
        let ansi = String::from_utf8_lossy(&capture);
        assert!(ansi.contains("\x1b[5A") || ansi.contains("\x1b[6A"));
        assert!(ansi.contains("archive.ubuntu.com"));
        assert!(ansi.contains("security.ubuntu.com"));
    }

    #[test]
    fn unprivileged_and_refresh_labels_are_truthful() {
        let theme = Theme::test(80);
        let stages = ExecutionStage::maintenance_stages();
        let mut user = state();
        user.current_stage = ExecutionStage::Executing;
        let labels = frame_lines(theme, stages, &user, false, true).join("\n");
        assert!(!labels.contains("Permission granted"));
        assert!(labels.contains("Refreshing information"));
        assert!(labels.contains("Checking result"));

        let admin = state();
        let labels = frame_lines(theme, stages, &admin, true, false).join("\n");
        assert!(labels.contains("Permission granted"));
        assert!(!labels.contains("Getting permission"));
    }

    #[test]
    fn apt_activity_interprets_hosts_without_fabricating_names() {
        let (host, activity) =
            apt_activity("Hit:1 http://archive.ubuntu.com/ubuntu noble InRelease")
                .expect("APT activity");
        assert_eq!(host, "archive.ubuntu.com");
        assert!(activity.ends_with("up to date"));
        assert!(apt_activity("Reading package lists...").is_none());
    }
}
