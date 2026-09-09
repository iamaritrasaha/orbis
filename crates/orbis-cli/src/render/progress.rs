use crate::render::theme::{StageState, Theme, Token};
use orbis_core::{
    maintenance::{MaintenancePlan, MaintenanceProviderStatus, MaintenanceResult},
    models::PackageSource,
    progress::{ExecutionStage, OperationEvent, OperationHeader, OutputStream, ProgressObserver},
    transaction::InstallScope,
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

use super::region::TransientRegion;

#[derive(Clone)]
struct MaintenanceRowMeta {
    sources: Vec<PackageSource>,
    label: String,
    scope: Option<InstallScope>,
    mutates: bool,
    executable: bool,
    provider_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderViewState {
    Waiting,
    Active,
    Refreshed,
    PartiallyRefreshed,
    ManagedAutomatically,
    OnDemand,
    Skipped,
    Failed,
}

#[derive(Clone)]
enum RenderMode {
    Standard { header: OperationHeader, stages: &'static [ExecutionStage], refresh: bool },
    Maintenance { rows: Vec<MaintenanceRowMeta> },
}

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
    provider_states: Vec<ProviderViewState>,
    active_provider: Option<usize>,
    final_summary: Option<String>,
}

pub(crate) struct PlainProgressRenderer {
    theme: Theme,
    mode: RenderMode,
    state: Arc<Mutex<ProgressState>>,
    // Kept as a small inspection surface for tests and future plain renderers.
    output_lines: Mutex<VecDeque<String>>,
    spinner_started: AtomicBool,
    spinner_thread: Mutex<Option<JoinHandle<()>>>,
    region: Option<TransientRegion>,
    is_tty: bool,
    animate: bool,
    max_output_lines: usize,
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
        Self::new_with_output_policy(theme, header, stages, is_tty, max_output_lines, true)
    }

    pub(crate) fn new_with_output_policy(
        theme: Theme,
        header: OperationHeader,
        stages: &'static [ExecutionStage],
        is_tty: bool,
        max_output_lines: usize,
        preserve_raw_output: bool,
    ) -> Self {
        let mode = RenderMode::Standard { header, stages, refresh: false };
        Self::with_mode(theme, mode, is_tty, max_output_lines, preserve_raw_output)
    }

    pub(crate) fn new_maintenance_with_output_policy(
        theme: Theme,
        plan: &MaintenancePlan,
        is_tty: bool,
        preserve_raw_output: bool,
    ) -> Self {
        let rows = maintenance_rows(plan);
        Self::with_mode(theme, RenderMode::Maintenance { rows }, is_tty, 5, preserve_raw_output)
    }

    fn with_mode(
        theme: Theme,
        mode: RenderMode,
        is_tty: bool,
        max_output_lines: usize,
        preserve_raw_output: bool,
    ) -> Self {
        let (current_stage, provider_states) = match &mode {
            RenderMode::Standard { stages, .. } => {
                (stages.first().copied().unwrap_or(ExecutionStage::Preparing), Vec::new())
            }
            RenderMode::Maintenance { rows } => (
                ExecutionStage::Preparing,
                rows.iter()
                    .map(|row| {
                        if !row.executable {
                            ProviderViewState::Skipped
                        } else if !row.mutates {
                            if row.sources.contains(&PackageSource::Snap) {
                                ProviderViewState::ManagedAutomatically
                            } else {
                                ProviderViewState::OnDemand
                            }
                        } else {
                            ProviderViewState::Waiting
                        }
                    })
                    .collect(),
            ),
        };
        let animate = is_tty
            && std::env::var_os("REDUCE_MOTION").is_none()
            && std::env::var("TERM").map(|term| term != "dumb").unwrap_or(true);
        let region =
            is_tty.then(|| TransientRegion::new(progress_region_height(&mode, max_output_lines)));
        Self {
            theme,
            mode,
            state: Arc::new(Mutex::new(ProgressState {
                current_stage,
                output_lines: VecDeque::new(),
                output_streams: VecDeque::new(),
                activity_lines: VecDeque::new(),
                spinner_index: 0,
                running: true,
                rendered_line_count: 0,
                provider_states,
                active_provider: None,
                final_summary: None,
            })),
            output_lines: Mutex::new(VecDeque::new()),
            spinner_started: AtomicBool::new(false),
            spinner_thread: Mutex::new(None),
            region,
            is_tty,
            animate,
            max_output_lines,
            preserve_raw_output,
        }
    }

    pub(crate) fn print_header(&self) {
        if self.is_tty {
            let _guard = output_lock();
            let mut stderr = io::stderr();
            if let Some(region) = self.region {
                let _ = region.reserve(&mut stderr);
                if let Ok(mut state) = self.state.lock() {
                    draw_tty_to_region(&mut stderr, self.theme, &self.mode, region, &mut state);
                }
                let _ = stderr.flush();
            }
            let has_active_animation = match &self.mode {
                RenderMode::Standard { .. } => true,
                RenderMode::Maintenance { rows } => {
                    rows.iter().any(|row| row.executable && row.mutates)
                }
            };
            if has_active_animation {
                self.start_spinner();
            }
        }
    }

    fn start_spinner(&self) {
        if !self.animate || self.spinner_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = Arc::clone(&self.state);
        let theme = self.theme;
        let mode = self.mode.clone();
        let Some(region) = self.region else { return };
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
                draw_tty(theme, &mode, region, &state);
            }
        });
        *self.spinner_thread.lock().expect("spinner thread lock poisoned") = Some(handle);
    }

    fn redraw(&self) {
        if !self.is_tty {
            return;
        }
        let _guard = output_lock();
        if let Some(region) = self.region {
            draw_tty(self.theme, &self.mode, region, &self.state);
        }
    }

    fn finish_standard(&self, stage: ExecutionStage) {
        if let Ok(mut state) = self.state.lock() {
            state.current_stage = stage;
            state.output_lines.clear();
            state.output_streams.clear();
            state.activity_lines.clear();
            state.running = false;
        }
        self.output_lines.lock().expect("output lines lock poisoned").clear();
        self.join_spinner();
        self.commit_tty_frame();
    }

    pub(crate) fn finish_maintenance(&self, result: &MaintenanceResult) {
        let RenderMode::Maintenance { rows } = &self.mode else { return };
        if let Ok(mut state) = self.state.lock() {
            for (row_index, row) in rows.iter().enumerate() {
                state.provider_states[row_index] = maintenance_row_state(row, &result.providers);
            }
            state.active_provider = None;
            state.activity_lines.clear();
            state.final_summary = Some(maintenance_summary(&state.provider_states));
            state.running = false;
        }
        self.join_spinner();
        self.commit_tty_frame();
    }

    pub(crate) fn mark_provider_failure(&self, source: PackageSource) {
        let RenderMode::Maintenance { rows } = &self.mode else { return };
        if let Ok(mut state) = self.state.lock()
            && let Some((row_index, _)) = rows.iter().enumerate().find(|(row_index, row)| {
                row.sources.contains(&source)
                    && !matches!(
                        state.provider_states[*row_index],
                        ProviderViewState::Failed
                            | ProviderViewState::Refreshed
                            | ProviderViewState::PartiallyRefreshed
                            | ProviderViewState::ManagedAutomatically
                            | ProviderViewState::OnDemand
                    )
            })
        {
            state.provider_states[row_index] = ProviderViewState::Failed;
            state.active_provider = None;
            state.activity_lines.clear();
        }
        self.redraw();
    }

    pub(crate) fn is_tty(&self) -> bool {
        self.is_tty
    }

    fn join_spinner(&self) {
        if let Some(handle) =
            self.spinner_thread.lock().expect("spinner thread lock poisoned").take()
        {
            let _ = handle.join();
        }
    }

    fn commit_tty_frame(&self) {
        if !self.is_tty {
            return;
        }
        let Some(region) = self.region else { return };
        let _guard = output_lock();
        let mut stderr = io::stderr();
        if let Ok(mut state) = self.state.lock() {
            let meaningful_height =
                draw_tty_to_region(&mut stderr, self.theme, &self.mode, region, &mut state);
            let _ = region.commit(&mut stderr, meaningful_height);
            let _ = stderr.flush();
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
        self.join_spinner();
        if self.is_tty && was_running {
            let _guard = output_lock();
            let mut stderr = io::stderr();
            if let Some(region) = self.region {
                let _ = region.clear_and_finish(&mut stderr);
                let _ = stderr.flush();
            }
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
        if matches!(&self.mode, RenderMode::Maintenance { .. }) {
            self.on_maintenance_event(event);
            return;
        }
        self.on_standard_event(event);
    }
}

impl PlainProgressRenderer {
    fn on_standard_event(&self, event: &OperationEvent) {
        let (refresh, header_source) = match &self.mode {
            RenderMode::Standard { header, refresh, .. } => (*refresh, header.source),
            RenderMode::Maintenance { .. } => return,
        };
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
                    let token =
                        if state == StageState::Done { Token::Positive } else { Token::Caution };
                    let _guard = output_lock();
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(self.theme.stage_mark(state), token),
                        self.theme.paint(display_stage_label(*stage, refresh), Token::Foreground)
                    );
                }
            }
            OperationEvent::ProviderOutput(line) => {
                let content = sanitize_output(&line.content);
                if let Ok(mut state) = self.state.lock() {
                    push_raw_line(&mut state, content.clone(), line.stream, self.max_output_lines);
                    add_activity_line(
                        &mut state.activity_lines,
                        header_source,
                        &content,
                        refresh,
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
                    let display_content = if self.preserve_raw_output {
                        content
                    } else {
                        compact_activity(header_source, &content, refresh)
                    };
                    let pipe = if self.theme.unicode { "┊" } else { "|" };
                    let _guard = output_lock();
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(pipe, Token::Muted),
                        self.theme.paint(&display_content, Token::Muted)
                    );
                }
            }
            OperationEvent::Finished { stage, message, .. } => {
                self.finish_standard(*stage);
                if let Some(message) = message {
                    let _guard = output_lock();
                    eprintln!(
                        "\n  {} {}",
                        self.theme.paint("Result", Token::Primary),
                        sanitize_output(message)
                    );
                }
            }
            OperationEvent::Warning { message } => {
                let message = sanitize_output(message);
                if let Ok(mut state) = self.state.lock() {
                    push_raw_line(
                        &mut state,
                        format!("! {message}"),
                        OutputStream::Stderr,
                        self.max_output_lines,
                    );
                    add_activity_line(
                        &mut state.activity_lines,
                        header_source,
                        &format!("! {message}"),
                        refresh,
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
            OperationEvent::ProviderStarted { .. }
            | OperationEvent::ProviderFinished { .. }
            | OperationEvent::ProviderInventory { .. } => {}
        }
    }

    fn on_maintenance_event(&self, event: &OperationEvent) {
        let RenderMode::Maintenance { rows } = &self.mode else { return };
        match event {
            OperationEvent::ProviderStarted { source } => {
                if let Ok(mut state) = self.state.lock()
                    && let Some((row_index, _)) =
                        rows.iter().enumerate().find(|(row_index, row)| {
                            row.sources.contains(source)
                                && row.mutates
                                && state.provider_states[*row_index] == ProviderViewState::Waiting
                        })
                {
                    state.provider_states[row_index] = ProviderViewState::Active;
                    state.active_provider = Some(row_index);
                    state.activity_lines.clear();
                }
                self.redraw();
            }
            OperationEvent::ProviderOutput(line) => {
                let content = sanitize_output(&line.content);
                if let Ok(mut state) = self.state.lock()
                    && let Some(row_index) = state.active_provider
                {
                    let source = rows[row_index].sources[0];
                    add_activity_line(
                        &mut state.activity_lines,
                        source,
                        &content,
                        true,
                        self.max_output_lines,
                    );
                }
                if !self.is_tty {
                    let display_content = if self.preserve_raw_output {
                        content
                    } else {
                        compact_activity(PackageSource::Apt, &content, true)
                    };
                    let pipe = if self.theme.unicode { "┊" } else { "|" };
                    let _guard = output_lock();
                    eprintln!(
                        "  {} {}",
                        self.theme.paint(pipe, Token::Muted),
                        self.theme.paint(&display_content, Token::Muted)
                    );
                }
                self.redraw();
            }
            OperationEvent::ProviderFinished { source, success } => {
                if let Ok(mut state) = self.state.lock()
                    && let Some((row_index, _)) =
                        rows.iter().enumerate().find(|(row_index, row)| {
                            row.sources.contains(source)
                                && state.provider_states[*row_index] == ProviderViewState::Active
                        })
                {
                    state.provider_states[row_index] = if *success {
                        ProviderViewState::Refreshed
                    } else {
                        ProviderViewState::Failed
                    };
                    state.active_provider = None;
                    state.activity_lines.clear();
                }
                self.redraw();
            }
            OperationEvent::Finished { stage, message, .. } => {
                if *stage == ExecutionStage::Failed
                    && let Ok(mut state) = self.state.lock()
                {
                    if let Some(row_index) = state.active_provider {
                        state.provider_states[row_index] = ProviderViewState::Failed;
                    }
                    state.final_summary =
                        message.as_deref().map(|_| "1 source needs attention".into());
                }
                self.redraw();
            }
            OperationEvent::StageChanged { .. } => self.redraw(),
            OperationEvent::Warning { message } => {
                if let Ok(mut state) = self.state.lock() {
                    add_activity_line(
                        &mut state.activity_lines,
                        PackageSource::Apt,
                        &format!("! {}", sanitize_output(message)),
                        true,
                        self.max_output_lines,
                    );
                }
                self.redraw();
            }
            OperationEvent::ProviderInventory { .. } => {}
        }
    }
}

fn output_lock() -> MutexGuard<'static, ()> {
    static OUTPUT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    OUTPUT_LOCK.get_or_init(|| Mutex::new(())).lock().expect("output lock poisoned")
}

fn draw_tty(
    theme: Theme,
    mode: &RenderMode,
    region: TransientRegion,
    state: &Arc<Mutex<ProgressState>>,
) {
    let mut stderr = io::stderr();
    let Ok(mut state) = state.lock() else { return };
    draw_tty_to_region(&mut stderr, theme, mode, region, &mut state);
}

fn draw_tty_to_region<W: Write>(
    writer: &mut W,
    theme: Theme,
    mode: &RenderMode,
    region: TransientRegion,
    state: &mut ProgressState,
) -> usize {
    let frame = match mode {
        RenderMode::Standard { header, stages, refresh } => {
            standard_frame_lines(theme, header.privileged, stages, *refresh, state)
        }
        RenderMode::Maintenance { rows } => maintenance_frame_lines(theme, rows, state),
    };
    let meaningful_height = frame.len();
    let _ = region.render(writer, &frame);
    state.rendered_line_count = meaningful_height;
    meaningful_height
}

#[cfg(test)]
fn draw_tty_to<W: Write>(
    writer: &mut W,
    theme: Theme,
    mode: &RenderMode,
    state: &mut ProgressState,
) {
    let frame = match mode {
        RenderMode::Standard { header, stages, refresh } => {
            standard_frame_lines(theme, header.privileged, stages, *refresh, state)
        }
        RenderMode::Maintenance { rows } => maintenance_frame_lines(theme, rows, state),
    };
    let region = TransientRegion::new(state.rendered_line_count.max(frame.len()));
    let _ = region.render(writer, &frame);
    state.rendered_line_count = frame.len();
}

fn progress_region_height(mode: &RenderMode, max_output_lines: usize) -> usize {
    match mode {
        RenderMode::Standard { stages, .. } => stages.len() + max_output_lines + 1,
        RenderMode::Maintenance { rows } => rows.len() + max_output_lines + 3,
    }
}

fn standard_frame_lines(
    theme: Theme,
    privileged: bool,
    stages: &'static [ExecutionStage],
    refresh: bool,
    state: &ProgressState,
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
        let label_token =
            if stage_state == StageState::Pending { Token::Muted } else { Token::Foreground };
        lines.push(format!(
            "  {} {}{}",
            theme.paint(mark, token),
            theme.paint(display_stage_label(stage, refresh), label_token),
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
        lines.push(format!(
            "  {} {}",
            theme.paint(theme.stage_mark(StageState::Done), Token::Positive),
            theme.paint(label, Token::Foreground)
        ));
    }
    lines
}

fn maintenance_frame_lines(
    theme: Theme,
    rows: &[MaintenanceRowMeta],
    state: &ProgressState,
) -> Vec<String> {
    let label_width = theme.width.saturating_sub(25).clamp(24, 34);
    let mut lines = Vec::with_capacity(rows.len() + state.activity_lines.len() + 3);
    for (index, row) in rows.iter().enumerate() {
        let row_state = state.provider_states[index];
        let (mark, token, status) = provider_view(row_state, theme, state.spinner_index);
        lines.push(format!(
            "  {} {:<label_width$} {}",
            theme.paint(mark, token),
            theme.paint(&row.label, token),
            theme.paint(status, token),
            label_width = label_width
        ));
        if state.active_provider == Some(index) {
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
    lines.push(String::new());
    let rule = if theme.unicode { "─" } else { "-" };
    lines.push(theme.paint(&rule.repeat(theme.width.clamp(40, 72)), Token::Divider));
    let summary = state
        .final_summary
        .clone()
        .unwrap_or_else(|| maintenance_live_summary(&state.provider_states));
    lines.push(format!("  {}", theme.paint(&summary, Token::Muted)));
    lines
}

fn provider_view(
    state: ProviderViewState,
    theme: Theme,
    spinner_index: usize,
) -> (&'static str, Token, &'static str) {
    match state {
        ProviderViewState::Waiting => {
            (if theme.unicode { "○" } else { "o" }, Token::Muted, "waiting")
        }
        ProviderViewState::Active => (spinner(theme, spinner_index), Token::Primary, "refreshing"),
        ProviderViewState::Refreshed => {
            (if theme.unicode { "●" } else { "*" }, Token::Positive, "refreshed")
        }
        ProviderViewState::PartiallyRefreshed => {
            (if theme.unicode { "●" } else { "*" }, Token::Caution, "partially refreshed")
        }
        ProviderViewState::ManagedAutomatically => {
            (if theme.unicode { "◇" } else { "-" }, Token::Muted, "managed by snapd")
        }
        ProviderViewState::OnDemand => {
            (if theme.unicode { "◇" } else { "-" }, Token::Muted, "metadata on demand")
        }
        ProviderViewState::Skipped => {
            (if theme.unicode { "○" } else { "o" }, Token::Muted, "unavailable")
        }
        ProviderViewState::Failed => {
            (if theme.unicode { "×" } else { "x" }, Token::Destructive, "failed")
        }
    }
}

fn maintenance_rows(plan: &MaintenancePlan) -> Vec<MaintenanceRowMeta> {
    let mut rows = Vec::new();
    for (index, provider) in plan.providers.iter().enumerate() {
        let family = provider_family(provider.source);
        let row_index = rows.iter().position(|row: &MaintenanceRowMeta| {
            provider_family(row.sources[0]) == family
                && (provider.source != PackageSource::Flatpak || row.scope == provider.scope)
        });
        if let Some(row_index) = row_index {
            let row = &mut rows[row_index];
            row.sources.push(provider.source);
            row.mutates |= provider.mutates;
            row.executable &= provider.executable();
            row.provider_indices.push(index);
        } else {
            rows.push(MaintenanceRowMeta {
                sources: vec![provider.source],
                label: maintenance_source_label(provider.source, provider.scope),
                scope: provider.scope,
                mutates: provider.mutates,
                executable: provider.executable(),
                provider_indices: vec![index],
            });
        }
    }
    rows
}

fn maintenance_row_state(
    row: &MaintenanceRowMeta,
    providers: &[orbis_core::maintenance::MaintenanceProviderResult],
) -> ProviderViewState {
    let mut partial = false;
    let mut failed = false;
    let mut skipped = false;
    for index in &row.provider_indices {
        match providers.get(*index).map(|provider| &provider.status) {
            Some(MaintenanceProviderStatus::Failed) => failed = true,
            Some(MaintenanceProviderStatus::PartiallySucceeded) => partial = true,
            Some(MaintenanceProviderStatus::Skipped | MaintenanceProviderStatus::Blocked)
            | None => skipped = true,
            _ => {}
        }
    }
    if failed {
        ProviderViewState::Failed
    } else if skipped {
        ProviderViewState::Skipped
    } else if !row.mutates {
        if row.sources.contains(&PackageSource::Snap) {
            ProviderViewState::ManagedAutomatically
        } else {
            ProviderViewState::OnDemand
        }
    } else if partial {
        ProviderViewState::PartiallyRefreshed
    } else {
        ProviderViewState::Refreshed
    }
}

fn maintenance_live_summary(states: &[ProviderViewState]) -> String {
    let refreshed = states
        .iter()
        .filter(|state| {
            matches!(state, ProviderViewState::Refreshed | ProviderViewState::PartiallyRefreshed)
        })
        .count();
    let active = states.iter().filter(|state| **state == ProviderViewState::Active).count();
    let waiting = states.iter().filter(|state| **state == ProviderViewState::Waiting).count();
    format!("{refreshed} refreshed   {active} active   {waiting} waiting")
}

fn maintenance_summary(states: &[ProviderViewState]) -> String {
    let checked = states.len();
    let refreshed = states
        .iter()
        .filter(|state| {
            matches!(state, ProviderViewState::Refreshed | ProviderViewState::PartiallyRefreshed)
        })
        .count();
    let no_refresh = states
        .iter()
        .filter(|state| {
            matches!(state, ProviderViewState::ManagedAutomatically | ProviderViewState::OnDemand)
        })
        .count();
    let failed = states.iter().filter(|state| **state == ProviderViewState::Failed).count();
    if failed > 0 {
        format!("{failed} source{} need attention", if failed == 1 { "" } else { "s" })
    } else {
        format!(
            "{checked} source{} checked · {refreshed} refreshed · {no_refresh} require no refresh",
            if checked == 1 { "" } else { "s" }
        )
    }
}

fn provider_family(source: PackageSource) -> PackageSource {
    match source {
        PackageSource::Pnpm => PackageSource::Npm,
        PackageSource::Pipx => PackageSource::Uv,
        source => source,
    }
}

fn maintenance_source_label(source: PackageSource, scope: Option<InstallScope>) -> String {
    match source {
        PackageSource::Apt => "Ubuntu repositories".into(),
        PackageSource::Flatpak => scope
            .map(|scope| format!("Flatpak · {}", scope.label()))
            .unwrap_or_else(|| "Flatpak".into()),
        PackageSource::Snap => "Snap Store".into(),
        PackageSource::Cargo => "Rust tools".into(),
        PackageSource::Npm | PackageSource::Pnpm => "Node.js tools".into(),
        PackageSource::Uv | PackageSource::Pipx => "Python tools".into(),
    }
}

fn push_raw_line(
    state: &mut ProgressState,
    content: String,
    stream: OutputStream,
    max_lines: usize,
) {
    if state.output_lines.len() >= max_lines {
        state.output_lines.pop_front();
        state.output_streams.pop_front();
    }
    state.output_lines.push_back(content);
    state.output_streams.push_back(stream);
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

#[cfg(test)]
mod tests {
    use super::*;
    use orbis_core::progress::{OutputLine, OutputStream};
    use orbis_core::{
        maintenance::{MaintenanceAction, ProviderMaintenancePlan},
        transaction::PrivilegeRequirement,
    };

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
            provider_states: Vec::new(),
            active_provider: None,
            final_summary: None,
        }
    }

    #[test]
    fn test_print_header_non_tty() {
        let renderer = PlainProgressRenderer::new(
            Theme::test(80),
            create_header(),
            ExecutionStage::transaction_stages(),
            false,
            8,
        );
        renderer.print_header();
    }

    #[test]
    fn test_stage_progression() {
        let renderer = PlainProgressRenderer::new(
            Theme::test(80),
            create_header(),
            ExecutionStage::transaction_stages(),
            true,
            8,
        );
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
        let renderer = PlainProgressRenderer::new(
            Theme::test(80),
            create_header(),
            ExecutionStage::transaction_stages(),
            false,
            2,
        );
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
            let frame = standard_frame_lines(theme, true, stages, false, &progress);
            visible.splice(0..owned_lines, frame.clone());
            owned_lines = frame.len();
            draw_tty_to(
                &mut capture,
                theme,
                &RenderMode::Standard { header: create_header(), stages, refresh: false },
                &mut progress,
            );
        }
        assert_eq!(owned_lines, visible.len());
        assert!(String::from_utf8_lossy(&capture).matches("\x1b[").count() >= 19);
        assert_eq!(visible.iter().filter(|line| line.contains("Preparing")).count(), 1);
        assert_eq!(visible.iter().filter(|line| line.contains("Finishing up")).count(), 1);
    }

    #[test]
    fn tty_progress_reserves_one_fixed_region_for_redraws() {
        let stages = ExecutionStage::transaction_stages();
        let renderer =
            PlainProgressRenderer::new(Theme::test(80), create_header(), stages, true, 8);
        assert_eq!(renderer.region.expect("TTY region").height(), stages.len() + 8 + 1);
    }

    #[test]
    fn varying_frame_heights_clear_stale_lines() {
        let theme = Theme::test(80);
        let stages = ExecutionStage::transaction_stages();
        let mut progress = state();
        let mut capture = Vec::new();
        draw_tty_to(
            &mut capture,
            theme,
            &RenderMode::Standard { header: create_header(), stages, refresh: false },
            &mut progress,
        );
        add_activity_line(
            &mut progress.activity_lines,
            PackageSource::Apt,
            "Get:1 http://archive.ubuntu.com/ubuntu",
            true,
            8,
        );
        draw_tty_to(
            &mut capture,
            theme,
            &RenderMode::Standard { header: create_header(), stages, refresh: true },
            &mut progress,
        );
        add_activity_line(
            &mut progress.activity_lines,
            PackageSource::Apt,
            "Get:2 http://security.ubuntu.com/ubuntu",
            true,
            8,
        );
        draw_tty_to(
            &mut capture,
            theme,
            &RenderMode::Standard { header: create_header(), stages, refresh: true },
            &mut progress,
        );
        progress.activity_lines.clear();
        draw_tty_to(
            &mut capture,
            theme,
            &RenderMode::Standard { header: create_header(), stages, refresh: true },
            &mut progress,
        );
        progress.current_stage = ExecutionStage::Completed;
        progress.running = false;
        draw_tty_to(
            &mut capture,
            theme,
            &RenderMode::Standard { header: create_header(), stages, refresh: true },
            &mut progress,
        );
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
        let labels = standard_frame_lines(theme, false, stages, true, &user).join("\n");
        assert!(!labels.contains("Permission granted"));
        assert!(labels.contains("Refreshing information"));
        assert!(labels.contains("Checking result"));
        let admin = state();
        let labels = standard_frame_lines(theme, true, stages, false, &admin).join("\n");
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

    #[test]
    fn maintenance_rows_compose_families_and_scopes() {
        let plan = MaintenancePlan {
            operation_id: "maintenance".into(),
            action: MaintenanceAction::Refresh,
            source: None,
            providers: vec![
                ProviderMaintenancePlan::blocked(
                    PackageSource::Flatpak,
                    MaintenanceAction::Refresh,
                    "blocked",
                ),
                ProviderMaintenancePlan::blocked(
                    PackageSource::Cargo,
                    MaintenanceAction::Refresh,
                    "blocked",
                ),
            ],
            risk: orbis_core::transaction::RiskLevel::Blocked,
            completeness: orbis_core::transaction::PlanCompleteness::Unknown,
            privilege: PrivilegeRequirement::None,
            warnings: Vec::new(),
            mutates: false,
        };
        let rows = maintenance_rows(&plan);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "Flatpak");
        assert_eq!(rows[1].label, "Rust tools");
    }

    #[test]
    fn aggregate_frame_contains_one_row_per_composed_source() {
        let provider = |source, scope, mutates, privilege| ProviderMaintenancePlan {
            operation_id: "maintenance-plan".into(),
            source,
            action: MaintenanceAction::Refresh,
            scope,
            candidates: Vec::new(),
            cleanup_candidates: Vec::new(),
            privilege,
            completeness: orbis_core::transaction::PlanCompleteness::Complete,
            confidence: orbis_core::transaction::PlanConfidence::High,
            authoritative_simulation: false,
            risk: orbis_core::transaction::RiskLevel::Normal,
            supported: true,
            mutates,
            warnings: Vec::new(),
            notes: Vec::new(),
            download_size_bytes: None,
            disk_delta_bytes: None,
        };
        let plan = MaintenancePlan {
            operation_id: "maintenance".into(),
            action: MaintenanceAction::Refresh,
            source: None,
            providers: vec![
                provider(PackageSource::Apt, None, true, PrivilegeRequirement::Administrator),
                provider(
                    PackageSource::Flatpak,
                    Some(InstallScope::System),
                    true,
                    PrivilegeRequirement::Administrator,
                ),
                provider(
                    PackageSource::Flatpak,
                    Some(InstallScope::User),
                    true,
                    PrivilegeRequirement::None,
                ),
                provider(PackageSource::Snap, None, false, PrivilegeRequirement::None),
                provider(PackageSource::Cargo, None, false, PrivilegeRequirement::None),
                provider(PackageSource::Npm, None, false, PrivilegeRequirement::None),
                provider(PackageSource::Pnpm, None, false, PrivilegeRequirement::None),
                provider(PackageSource::Uv, None, false, PrivilegeRequirement::None),
                provider(PackageSource::Pipx, None, false, PrivilegeRequirement::None),
            ],
            risk: orbis_core::transaction::RiskLevel::Normal,
            completeness: orbis_core::transaction::PlanCompleteness::Complete,
            privilege: PrivilegeRequirement::Administrator,
            warnings: Vec::new(),
            mutates: true,
        };
        let rows = maintenance_rows(&plan);
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0].label, "Ubuntu repositories");
        assert_eq!(rows[1].label, "Flatpak · system");
        assert_eq!(rows[2].label, "Flatpak · user");
        assert_eq!(rows[5].label, "Node.js tools");
        assert_eq!(rows[6].label, "Python tools");

        let mut state = state();
        state.provider_states =
            rows.iter()
                .map(|row| {
                    if row.mutates {
                        ProviderViewState::Active
                    } else {
                        ProviderViewState::OnDemand
                    }
                })
                .collect();
        state.active_provider = Some(0);
        let frame = maintenance_frame_lines(Theme::test(80), &rows, &state).join("\n");
        for label in [
            "Ubuntu repositories",
            "Flatpak · system",
            "Flatpak · user",
            "Snap Store",
            "Rust tools",
            "Node.js tools",
            "Python tools",
        ] {
            assert_eq!(frame.matches(label).count(), 1, "{label} should have one row");
        }
        assert!(!frame.contains("Preparing"));
        assert!(!frame.contains("Permission granted"));
    }
}
