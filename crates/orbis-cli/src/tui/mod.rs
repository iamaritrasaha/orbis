//! Interactive terminal presentation for Orbis.
//!
//! The application owns only presentation state. Provider reads are performed by
//! short-lived read-only workers and mutations cross the same plan/executor
//! boundary as the ordinary CLI.

use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, IsTerminal},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use orbis_core::{
    ProviderRegistry,
    explain::build_brief,
    maintenance::{
        MaintenanceAction, MaintenancePlan, UpdateCandidate, UpdateInventoryReport, WhyReport,
    },
    models::{Package, PackageSource, SourceInfo},
    transaction::{OperationAction, OperationPlan, OperationRequest, PackageRefJson},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Modifier,
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    cli::Command,
    commands,
    render::theme::{StageState, Theme, Token},
};

mod components;
use components as ui;

const MIN_WIDTH: u16 = 70;
const MIN_HEIGHT: u16 = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Dashboard,
    Search,
    Package,
    Updates,
    Sources,
    Health,
    Advanced,
    History,
    HistoryDetail,
    Why,
    Help,
    Confirm,
    MaintenanceReview,
    Progress,
    Result,
}

#[derive(Clone, Copy)]
enum LayoutClass {
    Compact,
    Normal,
    Wide,
}

enum WorkerMessage {
    Snapshot(Vec<SourceInfo>, UpdateInventoryReport, orbis_core::diagnostics::DoctorReport),
    UpdateInventory(UpdateInventoryReport),
    Search(String, orbis_core::SearchReport),
    Plan(Box<Result<(OperationRequest, OperationPlan), String>>),
    Maintenance(Result<MaintenancePlan, String>),
    Why(Box<Result<WhyReport, String>>),
    Progress(orbis_core::progress::OperationEvent),
    TransactionComplete(Box<Result<orbis_core::transaction::TransactionResult, String>>),
    #[allow(dead_code)]
    MaintenanceComplete(Box<Result<orbis_core::maintenance::MaintenanceResult, String>>),
}

pub(crate) fn should_launch(command: Option<&Command>, json: bool, plain: bool) -> bool {
    !json && !plain && command.is_none() && terminal_capable()
}

pub(crate) fn run(registry: &ProviderRegistry, theme: Theme, json: bool) -> Result<(), String> {
    if json {
        return Err("dashboard output is not JSON; use a normal read-only command".into());
    }
    if !terminal_capable() {
        let renderer = crate::render::Renderer::new(theme.color);
        print!("{}", renderer.home(&registry.sources()));
        return Ok(());
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(registry, theme, tx, rx);
    app.refresh();
    match ratatui::run(|terminal| app.event_loop(terminal)) {
        Ok(()) => Ok(()),
        Err(error) => {
            // Ratatui restores the terminal, including its panic hook, before
            // returning. Keep initialization and draw failures safe and plain.
            let renderer = crate::render::Renderer::new(theme.color);
            print!("{}", renderer.home(&registry.sources()));
            eprintln!("orbis: dashboard unavailable ({error}); showing plain status");
            Ok(())
        }
    }
}

/// Whether an operation should use the full-screen live presentation.
pub(crate) fn should_launch_operation() -> bool {
    terminal_capable()
}

/// Runs a reviewed maintenance operation in the live terminal presentation.
/// The plan is still built by the core registry and execution still crosses the
/// same typed executor/history boundary as the ordinary CLI.
pub(crate) fn run_maintenance(
    registry: &ProviderRegistry,
    theme: Theme,
    action: MaintenanceAction,
    source: Option<PackageSource>,
    yes: bool,
) -> Result<(), String> {
    if !terminal_capable() {
        return Err("live operations require an interactive terminal".into());
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(registry, theme, tx, rx);
    app.maintenance_source = source;
    app.maintenance_auto_apply = yes;
    app.request_maintenance(action);
    match ratatui::run(|terminal| app.event_loop(terminal)) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("live operation unavailable: {error}")),
    }
}

/// Runs an exact, already-resolved package plan in the live operation view.
/// Resolution and planning remain outside this presentation entry point so an
/// explicit CLI command and a dashboard selection share the same safety checks.
pub(crate) fn run_transaction(
    registry: &ProviderRegistry,
    theme: Theme,
    request: OperationRequest,
    plan: OperationPlan,
    yes: bool,
) -> Result<(), String> {
    if !terminal_capable() {
        return Err("live operations require an interactive terminal".into());
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(registry, theme, tx, rx);
    app.selected_package = Some(plan.target.clone());
    app.plan = Some((request, plan));
    app.screen = Screen::Confirm;
    if yes {
        app.start_transaction();
    }
    match ratatui::run(|terminal| app.event_loop(terminal)) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("live operation unavailable: {error}")),
    }
}

/// Checks for updates in a focused read-only TUI view. The check uses the core
/// registry directly and never crosses the mutation boundary.
pub(crate) fn run_update(
    registry: &ProviderRegistry,
    theme: Theme,
    source: Option<PackageSource>,
) -> Result<(), String> {
    if !terminal_capable() {
        return Err("live update checks require an interactive terminal".into());
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(registry, theme, tx, rx);
    app.screen = Screen::Updates;
    app.update_check_running = true;
    app.maintenance_provider_status = registry
        .sources()
        .into_iter()
        .filter(|info| source.is_none_or(|wanted| wanted == info.source))
        .map(|info| (info.source, "Waiting".to_owned()))
        .collect();
    let tx = app.tx.clone();
    std::thread::spawn(move || {
        struct TuiObserver(Sender<WorkerMessage>);
        impl orbis_core::progress::ProgressObserver for TuiObserver {
            fn on_event(&self, event: &orbis_core::progress::OperationEvent) {
                let _ = self.0.send(WorkerMessage::Progress(event.clone()));
            }
        }
        let observer = TuiObserver(tx.clone());
        let report = ProviderRegistry::system().updates_with_observer(source, &observer);
        let _ = tx.send(WorkerMessage::UpdateInventory(report));
    });
    match ratatui::run(|terminal| app.event_loop(terminal)) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("live update check unavailable: {error}")),
    }
}

fn terminal_capable() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

/// Startup reveal animation for the Orbis terminal identity.
#[derive(Debug)]
pub(crate) struct StartupAnimation {
    enabled: bool,
    start: Option<std::time::Instant>,
    completed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnimationStep {
    Diamond,
    Reveal,
    Settled,
    Finished,
}

impl StartupAnimation {
    pub(crate) fn new(theme: &Theme) -> Self {
        let enabled = Self::should_enable(theme);
        Self {
            enabled,
            start: if enabled { Some(std::time::Instant::now()) } else { None },
            completed: !enabled,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_testing(enabled: bool) -> Self {
        Self {
            enabled,
            start: if enabled { Some(std::time::Instant::now()) } else { None },
            completed: !enabled,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_elapsed_ms(&mut self, ms: u64) {
        self.start = Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(ms))
                .unwrap_or_else(std::time::Instant::now),
        );
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self { enabled: false, start: None, completed: true }
    }

    pub(crate) fn should_enable(theme: &Theme) -> bool {
        if !theme.unicode || !theme.color {
            return false;
        }
        if std::env::var("NO_COLOR").is_ok() {
            return false;
        }
        if std::env::var("REDUCE_MOTION").is_ok() {
            return false;
        }
        if std::env::var("TERM").map(|term| term == "dumb").unwrap_or(false) {
            return false;
        }
        true
    }

    pub(crate) fn is_active(&self) -> bool {
        self.enabled && !self.completed
    }

    pub(crate) fn finish(&mut self) {
        self.completed = true;
    }

    pub(crate) fn step_at(&mut self, elapsed_ms: u128) -> AnimationStep {
        if !self.enabled || self.completed {
            return AnimationStep::Finished;
        }
        match elapsed_ms {
            0..=140 => AnimationStep::Diamond,
            141..=440 => AnimationStep::Reveal,
            441..=600 => AnimationStep::Settled,
            _ => {
                self.completed = true;
                AnimationStep::Finished
            }
        }
    }

    pub(crate) fn step(&mut self) -> AnimationStep {
        if !self.is_active() {
            return AnimationStep::Finished;
        }
        let Some(start) = self.start else {
            self.completed = true;
            return AnimationStep::Finished;
        };
        self.step_at(start.elapsed().as_millis())
    }

    fn elapsed_ms(&self) -> u128 {
        self.start.map_or(0, |start| start.elapsed().as_millis())
    }
}

struct App<'a> {
    _registry: &'a ProviderRegistry,
    theme: Theme,
    tx: Sender<WorkerMessage>,
    rx: Receiver<WorkerMessage>,
    screen: Screen,
    previous: Screen,
    sources: Option<Vec<SourceInfo>>,
    updates: Option<UpdateInventoryReport>,
    update_check_running: bool,
    spinner_index: usize,
    search_query: String,
    search_results: Vec<Package>,
    selected: usize,
    search_loading: bool,
    snapshot_loading: bool,
    selected_package: Option<Package>,
    why: Option<WhyReport>,
    history: Vec<orbis_core::transaction::history::HistoryEntry>,
    history_selected: usize,
    navigation_scroll: usize,
    history_detail: Option<serde_json::Value>,
    plan: Option<(OperationRequest, OperationPlan)>,
    maintenance: Option<MaintenancePlan>,
    maintenance_action: MaintenanceAction,
    maintenance_source: Option<PackageSource>,
    maintenance_auto_apply: bool,
    maintenance_executing: bool,
    maintenance_provider_status: BTreeMap<PackageSource, String>,
    doctor: Option<orbis_core::diagnostics::DoctorReport>,
    why_loading: bool,
    doctor_loading: bool,
    loading_plan: bool,
    error: Option<String>,
    quit: bool,
    animation: StartupAnimation,
    // Progress screen state
    progress_stage: orbis_core::progress::ExecutionStage,
    progress_title: String,
    progress_context: String,
    progress_output: VecDeque<String>,
    progress_scroll: usize,
    progress_show_details: bool,
    progress_maintenance: bool,
    transaction_result: Option<orbis_core::transaction::TransactionResult>,
    maintenance_result: Option<orbis_core::maintenance::MaintenanceResult>,
}

impl<'a> App<'a> {
    fn new(
        registry: &'a ProviderRegistry,
        theme: Theme,
        tx: Sender<WorkerMessage>,
        rx: Receiver<WorkerMessage>,
    ) -> Self {
        Self {
            _registry: registry,
            theme,
            tx,
            rx,
            screen: Screen::Dashboard,
            previous: Screen::Dashboard,
            sources: None,
            updates: None,
            update_check_running: false,
            spinner_index: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            selected: 0,
            search_loading: false,
            snapshot_loading: false,
            selected_package: None,
            why: None,
            history: Vec::new(),
            history_selected: 0,
            navigation_scroll: 0,
            history_detail: None,
            plan: None,
            maintenance: None,
            maintenance_action: MaintenanceAction::Upgrade,
            maintenance_source: None,
            maintenance_auto_apply: false,
            maintenance_executing: false,
            maintenance_provider_status: BTreeMap::new(),
            doctor: None,
            why_loading: false,
            doctor_loading: false,
            loading_plan: false,
            error: None,
            quit: false,
            animation: StartupAnimation::new(&theme),
            progress_stage: orbis_core::progress::ExecutionStage::Preparing,
            progress_title: String::new(),
            progress_context: String::new(),
            progress_output: VecDeque::new(),
            progress_scroll: 0,
            // Static test/fixture progress views show their details panel;
            // active operations explicitly start with details collapsed.
            progress_show_details: true,
            progress_maintenance: false,
            transaction_result: None,
            maintenance_result: None,
        }
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        let mut dirty = true;
        while !self.quit {
            let mut received = false;
            while let Ok(message) = self.rx.try_recv() {
                self.accept(message);
                received = true;
            }
            if self.animation.is_active() {
                dirty = true;
            }
            if self.update_check_running || self.maintenance_executing {
                self.spinner_index = self.spinner_index.wrapping_add(1);
                dirty = true;
            }
            if dirty || received {
                terminal.draw(|frame| self.draw(frame))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(100))?
                && let event = event::read()?
            {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.animation.finish();
                        self.handle_key(key);
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn accept(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Snapshot(sources, updates, doctor) => {
                self.sources = Some(sources);
                self.updates = Some(updates);
                self.doctor = Some(doctor);
                self.snapshot_loading = false;
                self.doctor_loading = false;
            }
            WorkerMessage::UpdateInventory(report) => {
                self.updates = Some(report);
                self.update_check_running = false;
            }
            WorkerMessage::Search(query, report) if query == self.search_query => {
                self.search_results = report.results;
                self.search_loading = false;
                self.error = report.issues.first().map(|issue| issue.message.clone());
                self.selected = self.selected.min(self.search_results.len().saturating_sub(1));
            }
            WorkerMessage::Search(_, _) => {}
            WorkerMessage::Plan(result) => {
                self.loading_plan = false;
                match *result {
                    Ok(plan) => self.plan = Some(plan),
                    Err(error) => self.error = Some(error),
                }
            }
            WorkerMessage::Maintenance(result) => match result {
                Ok(plan) => {
                    self.maintenance = Some(plan);
                    if self.maintenance_auto_apply {
                        self.start_maintenance();
                    }
                }
                Err(error) => self.error = Some(error),
            },
            WorkerMessage::Why(result) => {
                self.why_loading = false;
                match *result {
                    Ok(report) => self.why = Some(report),
                    Err(error) => self.error = Some(error),
                }
            }
            WorkerMessage::Progress(event) => {
                use orbis_core::progress::OperationEvent;
                match &event {
                    OperationEvent::StageChanged { stage } => {
                        self.progress_stage = *stage;
                    }
                    OperationEvent::ProviderStarted { source } => {
                        self.maintenance_provider_status.insert(*source, "Working…".to_owned());
                    }
                    OperationEvent::ProviderInventory { inventory } => {
                        let mut inventories = self
                            .updates
                            .take()
                            .map(|report| report.inventories)
                            .unwrap_or_default();
                        inventories.retain(|item| item.source != inventory.source);
                        inventories.push(inventory.clone());
                        self.updates = Some(orbis_core::maintenance::aggregate_inventory(
                            inventories,
                            Vec::new(),
                        ));
                    }
                    OperationEvent::ProviderFinished { source, success } => {
                        self.maintenance_provider_status.insert(
                            *source,
                            if *success { "Done" } else { "Needs attention" }.to_owned(),
                        );
                    }
                    OperationEvent::ProviderOutput(line) => {
                        self.progress_output.push_back(line.content.clone());
                        if self.progress_output.len() > 200 {
                            self.progress_output.pop_front();
                        }
                    }
                    OperationEvent::Warning { message } => {
                        self.progress_output.push_back(format!("! {message}"));
                    }
                    OperationEvent::Finished { stage, .. } => {
                        self.progress_stage = *stage;
                    }
                }
            }
            WorkerMessage::TransactionComplete(result) => match *result {
                Ok(result) => {
                    self.progress_stage = orbis_core::progress::ExecutionStage::Completed;
                    self.transaction_result = Some(result);
                    self.screen = Screen::Result;
                    self.refresh();
                }
                Err(error) => {
                    self.progress_stage = orbis_core::progress::ExecutionStage::Failed;
                    self.error = Some(error);
                    self.screen = Screen::Result;
                }
            },
            WorkerMessage::MaintenanceComplete(result) => match *result {
                Ok(result) => {
                    self.maintenance_executing = false;
                    self.progress_stage = orbis_core::progress::ExecutionStage::Completed;
                    self.maintenance_result = Some(result);
                    self.screen = Screen::Result;
                    self.refresh();
                }
                Err(error) => {
                    self.maintenance_executing = false;
                    self.progress_stage = orbis_core::progress::ExecutionStage::Failed;
                    self.error = Some(error);
                    self.screen = Screen::Result;
                }
            },
        }
    }

    fn refresh(&mut self) {
        if self.snapshot_loading {
            return;
        }
        self.snapshot_loading = true;
        self.doctor_loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let registry = ProviderRegistry::system();
            let sources = registry.sources();
            let updates = registry.updates(None);
            let doctor = registry.diagnostics(None).with_environment();
            let _ = tx.send(WorkerMessage::Snapshot(sources, updates, doctor));
        });
    }

    fn search(&mut self) {
        if self.search_query.trim().is_empty() {
            self.search_results.clear();
            self.search_loading = false;
            return;
        }
        self.search_loading = true;
        let query = self.search_query.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let report = ProviderRegistry::system().search(&query, None);
            let _ = tx.send(WorkerMessage::Search(query, report));
        });
    }

    fn open(&mut self, screen: Screen) {
        self.animation.finish();
        self.previous = self.screen;
        self.screen = screen;
        self.error = None;
        if matches!(screen, Screen::Sources | Screen::Health | Screen::Advanced) {
            self.navigation_scroll = 0;
        }
        if screen == Screen::History
            && let Ok(history) = orbis_core::transaction::history::HistoryStore::default_location()
        {
            self.history = history.entries().unwrap_or_default();
            self.history_selected = 0;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.screen {
            Screen::Dashboard => self.handle_dashboard(key),
            Screen::Search => self.handle_search(key),
            Screen::Package => self.handle_package(key),
            Screen::Updates => self.handle_updates(key),
            Screen::Sources => self.handle_navigation(key),
            Screen::Health => self.handle_navigation(key),
            Screen::Advanced => self.handle_advanced(key),
            Screen::History => self.handle_history(key),
            Screen::HistoryDetail => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    self.screen = Screen::History
                }
            }
            Screen::Why => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('w')) {
                    self.screen = Screen::Package
                }
            }
            Screen::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.screen = self.previous
                } else if key.code == KeyCode::Char('q') {
                    self.quit = true;
                }
            }
            Screen::Confirm => self.handle_confirm(key),
            Screen::MaintenanceReview => {
                if key.code == KeyCode::Enter {
                    self.start_maintenance();
                } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('u')) {
                    self.screen = Screen::Updates;
                    self.maintenance = None;
                }
            }
            Screen::Progress => self.handle_progress(key),
            Screen::Result => self.handle_result(key),
        }
    }

    fn handle_progress(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.progress_scroll = self.progress_scroll.saturating_add(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.progress_scroll = self.progress_scroll.saturating_sub(1);
            }
            KeyCode::Char('l' | 'L') => {
                self.progress_show_details = !self.progress_show_details;
            }
            _ => {}
        }
    }

    fn handle_result(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                self.screen = Screen::Package;
                self.transaction_result = None;
                self.maintenance_result = None;
            }
            KeyCode::Char('h' | 'H') => self.open(Screen::History),
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn handle_dashboard(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('/' | 'f' | 'F') => {
                self.search_query.clear();
                self.open(Screen::Search);
            }
            KeyCode::Char('r' | 'R') => self.refresh(),
            KeyCode::Char('u' | 'U') => self.open(Screen::Updates),
            KeyCode::Char('s' | 'S') => self.open(Screen::Sources),
            KeyCode::Char('h' | 'H') => self.open(Screen::Health),
            KeyCode::Char('d' | 'D') => self.open(Screen::Health),
            KeyCode::Char('a' | 'A') => self.open(Screen::Advanced),
            KeyCode::Char('t' | 'T') => self.open(Screen::History),
            KeyCode::Char('c' | 'C') => self.request_maintenance(MaintenanceAction::Cleanup),
            _ => {}
        }
    }

    fn handle_navigation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Dashboard,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('r' | 'R') => self.refresh(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.navigation_scroll = self.navigation_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.navigation_scroll = self.navigation_scroll.saturating_add(1)
            }
            _ => {}
        }
    }

    fn handle_advanced(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Dashboard,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('r' | 'R') => self.refresh(),
            KeyCode::Char('s' | 'S') => self.open(Screen::Sources),
            KeyCode::Char('d' | 'D') => self.open(Screen::Health),
            _ => {}
        }
    }

    fn handle_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Dashboard,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.search_results.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(package) = self.search_results.get(self.selected).cloned() {
                    self.selected_package = Some(package);
                    self.open(Screen::Package);
                }
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.search();
            }
            KeyCode::Char(character) => {
                self.search_query.push(character);
                self.search();
            }
            _ => {}
        }
    }

    fn handle_package(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Search,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('/') => {
                self.search_query.clear();
                self.open(Screen::Search);
            }
            KeyCode::Char('w' | 'W') => self.load_why(),
            KeyCode::Char('i' | 'I') => self.request_transaction(OperationAction::Install),
            KeyCode::Char('r' | 'R') => self.request_transaction(OperationAction::Remove),
            _ => {}
        }
    }

    fn handle_updates(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Dashboard,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('r' | 'R') => self.refresh(),
            KeyCode::Char('u' | 'U') => self.request_maintenance(MaintenanceAction::Upgrade),
            _ => {}
        }
    }

    fn handle_history(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Dashboard,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Up | KeyCode::Char('k') => {
                self.history_selected = self.history_selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.history_selected =
                    (self.history_selected + 1).min(self.history.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(entry) = self.history.get(self.history_selected) {
                    self.history_detail = serde_json::to_value(entry).ok();
                    self.open(Screen::HistoryDetail);
                }
            }
            _ => {}
        }
    }

    fn request_transaction(&mut self, action: OperationAction) {
        let Some(package) = self.selected_package.clone() else { return };
        let request = OperationRequest {
            action,
            package: PackageRefJson::from(&orbis_core::models::PackageRef {
                source: Some(package.source),
                query: package.provider_id.clone(),
            }),
            scope: match package.source {
                PackageSource::Cargo
                | PackageSource::Npm
                | PackageSource::Pnpm
                | PackageSource::Uv
                | PackageSource::Pipx => Some(orbis_core::transaction::InstallScope::User),
                PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap => None,
            },
            channel: None,
        };
        self.loading_plan = true;
        self.plan = None;
        self.open(Screen::Confirm);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = ProviderRegistry::system()
                .plan_transaction(&request)
                .map(|plan| (request, plan))
                .map_err(|error| error.to_string());
            let _ = tx.send(WorkerMessage::Plan(Box::new(result)));
        });
    }

    fn request_maintenance(&mut self, action: MaintenanceAction) {
        self.maintenance_action = action;
        self.maintenance = None;
        self.maintenance_executing = false;
        self.maintenance_provider_status.clear();
        self.progress_maintenance = false;
        let source = self.maintenance_source;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = ProviderRegistry::system().maintenance_plan(action, source);
            let _ = tx.send(WorkerMessage::Maintenance(result));
        });
        self.open(Screen::MaintenanceReview);
    }

    fn start_maintenance(&mut self) {
        let Some(plan) = self.maintenance.clone() else { return };
        if !plan.executable() {
            self.error = Some(
                "Orbis cannot safely confirm everything this operation may change. Nothing was changed."
                    .into(),
            );
            return;
        }
        self.maintenance_executing = true;
        self.progress_maintenance = true;
        self.progress_stage = orbis_core::progress::ExecutionStage::Preparing;
        self.progress_title = match plan.action {
            MaintenanceAction::Refresh => "Refreshing software information".to_owned(),
            MaintenanceAction::Upgrade => "Updating your software".to_owned(),
            MaintenanceAction::Cleanup => "Cleaning up".to_owned(),
        };
        let sources = plan.providers.iter().filter(|provider| provider.executable()).count();
        self.progress_context =
            format!("{} source{} · staged safely", sources, if sources == 1 { "" } else { "s" });
        self.progress_output.clear();
        self.progress_scroll = 0;
        self.progress_show_details = false;
        self.maintenance_provider_status = plan
            .providers
            .iter()
            .filter(|provider| provider.executable())
            .map(|provider| (provider.source, "Waiting".to_owned()))
            .collect();
        self.maintenance_result = None;
        self.open(Screen::Progress);

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            struct TuiObserver(Sender<WorkerMessage>);
            impl orbis_core::progress::ProgressObserver for TuiObserver {
                fn on_event(&self, event: &orbis_core::progress::OperationEvent) {
                    let _ = self.0.send(WorkerMessage::Progress(event.clone()));
                }
            }
            let observer = TuiObserver(tx.clone());
            let registry = ProviderRegistry::system();
            let result =
                commands::execute_confirmed_maintenance_with_observer(&registry, plan, &observer);
            let _ = tx.send(WorkerMessage::MaintenanceComplete(Box::new(result)));
        });
    }

    fn load_why(&mut self) {
        let Some(package) = self.selected_package.clone() else { return };
        self.why = None;
        self.why_loading = true;
        self.open(Screen::Why);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result =
                ProviderRegistry::system().why(&package).map_err(|error| error.to_string());
            let _ = tx.send(WorkerMessage::Why(Box::new(result)));
        });
    }

    fn handle_confirm(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) {
            self.screen = Screen::Package;
            self.plan = None;
            return;
        }
        if matches!(key.code, KeyCode::Enter | KeyCode::Char('y')) {
            self.start_transaction();
        }
    }

    fn start_transaction(&mut self) {
        let Some((request, plan)) = self.plan.clone() else { return };
        if !plan.executable() {
            self.error = Some("This plan is blocked or incomplete and cannot be confirmed.".into());
            return;
        }
        self.progress_maintenance = false;
        self.progress_stage = orbis_core::progress::ExecutionStage::Preparing;
        self.progress_title = format!("{} {}", plan.action.label(), plan.target.name);
        self.progress_context = format!(
            "{} · {} · {} access",
            friendly_source(plan.target.source),
            plan.scope.label(),
            if plan.privilege == orbis_core::transaction::PrivilegeRequirement::None {
                "User"
            } else {
                "Administrator"
            }
        );
        self.progress_output.clear();
        self.progress_scroll = 0;
        self.progress_show_details = false;
        self.transaction_result = None;
        self.open(Screen::Progress);

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            struct TuiObserver(Sender<WorkerMessage>);
            impl orbis_core::progress::ProgressObserver for TuiObserver {
                fn on_event(&self, event: &orbis_core::progress::OperationEvent) {
                    let _ = self.0.send(WorkerMessage::Progress(event.clone()));
                }
            }
            let observer = TuiObserver(tx.clone());
            let registry = ProviderRegistry::system();
            let result = commands::execute_confirmed_transaction_with_observer(
                &registry, &request, plan, &observer,
            );
            let _ = tx.send(WorkerMessage::TransactionComplete(Box::new(result)));
        });
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        // Each frame owns the complete terminal surface. Clearing first prevents
        // shorter secondary pages from leaving dashboard content behind.
        frame.render_widget(Clear, area);
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            let lines = vec![
                Line::from(Span::styled(
                    "Orbis needs a little more room.",
                    self.theme.style(Token::Primary),
                )),
                Line::from(format!("Current: {} × {}", area.width, area.height)),
                Line::from(format!("Minimum recommended: {} × {}", MIN_WIDTH, MIN_HEIGHT)),
                Line::from("Press q to quit."),
            ];
            frame.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), area);
            return;
        }
        if self.screen != Screen::Dashboard {
            self.animation.finish();
        }
        match self.screen {
            Screen::Dashboard => self.draw_dashboard(frame, area),
            Screen::Search => self.draw_search(frame, area),
            Screen::Package => self.draw_package(frame, area),
            Screen::Updates => self.draw_updates(frame, area),
            Screen::Sources => self.draw_sources(frame, area),
            Screen::Health => self.draw_health(frame, area),
            Screen::Advanced => self.draw_advanced(frame, area),
            Screen::History => self.draw_history(frame, area),
            Screen::HistoryDetail => self.draw_history_detail(frame, area),
            Screen::Why => self.draw_why(frame, area),
            Screen::Help => {
                self.animation.finish();
                self.draw_dashboard(frame, area);
                self.draw_help(frame, area);
            }
            Screen::Confirm => {
                self.draw_package(frame, area);
                self.draw_confirm(frame, area);
            }
            Screen::MaintenanceReview => {
                self.draw_maintenance(frame, area);
            }
            Screen::Progress => self.draw_progress(frame, area),
            Screen::Result => self.draw_result(frame, area),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shell_scrolled_actions(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        subtitle: &str,
        body: Vec<Line<'static>>,
        scroll: u16,
        actions: &str,
    ) {
        self.shell_actions(frame, area, title, subtitle, body, scroll, actions);
    }

    #[allow(clippy::too_many_arguments)]
    fn shell_actions(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        subtitle: &str,
        body: Vec<Line<'static>>,
        scroll: u16,
        actions: &str,
    ) {
        ui::page(frame, area, self.theme, title, subtitle, body, scroll, actions);
    }

    fn draw_dashboard(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if area.height < 20 || area.width < 70 {
            self.animation.finish();
        }
        let compact = matches!(layout_class(area.width), LayoutClass::Compact);
        let margin = if compact { 2 } else { 3 };
        let content = Rect {
            x: area.x + margin,
            y: area.y,
            width: area.width.saturating_sub(margin * 2),
            height: area.height,
        };

        if compact && area.height < 24 {
            self.draw_tight_dashboard(frame, content);
            return;
        }

        let brand_height = if self.animation.is_active() && !compact { 7 } else { 3 };
        let chunks = Layout::vertical([
            Constraint::Length(brand_height),
            Constraint::Length(1),
            Constraint::Length(9),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(content);

        self.draw_brand(frame, chunks[0], compact);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "What would you like to do?",
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            ))),
            chunks[1],
        );
        self.draw_task_cards(frame, chunks[2], compact);
        let recent = if self.history.is_empty() {
            "No recent Orbis activity".to_owned()
        } else {
            format!(
                "{} recent action{}",
                self.history.len(),
                if self.history.len() == 1 { "" } else { "s" }
            )
        };
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                ui::section_title(self.theme, "RECENT ACTIVITY"),
                Line::from(Span::styled(recent, self.theme.style(Token::Muted))),
                Line::from(Span::styled(
                    "Open History to see completed installs and removals",
                    self.theme.style(Token::Muted),
                )),
                Line::from(Span::styled(
                    "Updates are checked without changing software",
                    self.theme.style(Token::Muted),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "A Advanced  ·  T History",
                    self.theme.style(Token::Muted),
                )),
            ])),
            chunks[4],
        );
        self.draw_footer(frame, chunks[6]);
    }

    fn draw_brand(&mut self, frame: &mut Frame<'_>, area: Rect, compact: bool) {
        let step = self.animation.step();
        let lines = if compact || matches!(step, AnimationStep::Finished | AnimationStep::Settled) {
            let mark = match step {
                AnimationStep::Diamond => "◈",
                AnimationStep::Reveal => "◈ ORBIS",
                AnimationStep::Settled | AnimationStep::Finished => self.theme.brand_compact(),
            };
            vec![
                Line::from(Span::styled(
                    mark,
                    self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(Theme::brand_tagline(), self.theme.style(Token::Muted))),
            ]
        } else {
            let brand = self.theme.brand_full();
            match step {
                AnimationStep::Diamond => vec![Line::from(Span::styled(
                    "◈",
                    self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
                ))],
                AnimationStep::Reveal => {
                    let rows = ((self.animation.elapsed_ms().saturating_sub(140) / 60) as usize)
                        .clamp(1, brand.len());
                    brand
                        .iter()
                        .enumerate()
                        .map(|(index, line)| {
                            if index < rows {
                                Line::from(Span::styled(*line, self.theme.style(Token::Primary)))
                            } else {
                                Line::from("")
                            }
                        })
                        .collect()
                }
                AnimationStep::Settled | AnimationStep::Finished => brand
                    .iter()
                    .map(|line| Line::from(Span::styled(*line, self.theme.style(Token::Primary))))
                    .chain(std::iter::once(Line::from(Span::styled(
                        Theme::brand_tagline(),
                        self.theme.style(Token::Muted),
                    ))))
                    .collect(),
            }
        };
        frame.render_widget(Paragraph::new(Text::from(lines)).alignment(Alignment::Center), area);
    }

    fn draw_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let footer =
            Block::default().borders(Borders::TOP).border_style(self.theme.style(Token::Divider));
        let inner = footer.inner(area);
        frame.render_widget(footer, area);
        let actions = "/ Find   U Updates   C Clean   H Health   A Advanced   ? Help   Q Quit";
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(actions, self.theme.style(Token::Muted)))),
            inner,
        );
    }

    fn draw_task_cards(&self, frame: &mut Frame<'_>, area: Rect, compact: bool) {
        let rows = Layout::vertical([Constraint::Length(4), Constraint::Length(4)]).split(area);
        let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]);
        let updates = update_summary(self.updates.as_ref(), self.snapshot_loading);
        let health = self.health_summary();
        let cards = [
            ("F", "Find software", "Search apps and tools"),
            ("U", "Updates", updates.as_str()),
            ("C", "Clean up", "Review unused software safely"),
            ("H", "Health", health.as_str()),
        ];
        for (row, pair) in rows.iter().zip(cards.chunks(2)) {
            let cells = columns.split(*row);
            for (cell, (key, title, detail)) in cells.iter().zip(pair) {
                let inner = Rect {
                    x: cell.x.saturating_add(1),
                    y: cell.y.saturating_add(1),
                    width: cell.width.saturating_sub(2),
                    height: cell.height.saturating_sub(2),
                };
                let border = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(self.theme.style(Token::Divider));
                frame.render_widget(border, *cell);
                let text = if compact {
                    vec![
                        Line::from(Span::styled(
                            format!("[{key}] {title}"),
                            self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
                        )),
                        Line::from(Span::styled(*detail, self.theme.style(Token::Muted))),
                    ]
                } else {
                    vec![
                        Line::from(vec![
                            Span::styled(format!("[{key}] "), self.theme.style(Token::Primary)),
                            Span::styled(
                                *title,
                                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::from(Span::styled(*detail, self.theme.style(Token::Muted))),
                    ]
                };
                frame.render_widget(
                    Paragraph::new(Text::from(text)).wrap(Wrap { trim: false }),
                    inner,
                );
            }
        }
    }

    fn health_summary(&self) -> String {
        let Some(report) = &self.doctor else { return "Checking health…".into() };
        let issues = report
            .checks
            .iter()
            .filter(|check| !check.passed && check.area != "Environment")
            .count();
        if issues == 0 {
            "Everything looks good".into()
        } else {
            format!("{issues} thing{} need attention", if issues == 1 { "" } else { "s" })
        }
    }

    fn draw_tight_dashboard(&self, frame: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(Span::styled(
                    self.theme.brand_compact(),
                    self.theme.style(Token::Primary),
                )),
                Line::from(Span::styled(Theme::brand_tagline(), self.theme.style(Token::Muted))),
            ])),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "What would you like to do?",
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            ))),
            chunks[1],
        );
        let lines = vec![
            Line::from("[F] Find software"),
            Line::from("[U] Updates"),
            Line::from("[C] Clean up"),
            Line::from(format!("[H] Health  · {}", self.health_summary())),
            Line::from(""),
            ui::section_title(self.theme, "RECENT ACTIVITY"),
            Line::from(Span::styled("No recent Orbis activity", self.theme.style(Token::Muted))),
        ];
        frame.render_widget(Paragraph::new(Text::from(lines)), chunks[2]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "/ Find  U Updates  C Clean  H Health  A Advanced  ? Help",
                self.theme.style(Token::Muted),
            ))),
            chunks[3],
        );
    }

    fn draw_search(&self, frame: &mut Frame<'_>, area: Rect) {
        let [header, body_area, footer] = ui::page_chunks(area);
        ui::page_header(frame, header, self.theme, "Find", "Search apps and tools across Linux");
        let body_chunks =
            Layout::vertical([Constraint::Length(3), Constraint::Length(1), Constraint::Min(1)])
                .split(body_area);
        let input = ui::panel(self.theme, " SEARCH PACKAGES ");
        let input_inner = input.inner(body_chunks[0]);
        frame.render_widget(input, body_chunks[0]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{}▌", self.search_query),
                self.theme.style(Token::Foreground),
            ))),
            input_inner,
        );

        let count = if self.search_loading {
            "Searching…".to_owned()
        } else {
            let count = self.search_results.len();
            format!("{count} result{}", if count == 1 { "" } else { "s" })
        };
        frame.render_widget(
            Paragraph::new(Span::styled(count, self.theme.style(Token::Muted))),
            body_chunks[1],
        );

        let mut results = Vec::new();
        if self.search_loading {
            results.push(ui::loading(self.theme, "Checking providers…"));
        } else if self.search_results.is_empty() && !self.search_query.is_empty() {
            results.push(ui::empty(self.theme, "No software matched. Try a different name."));
        }
        for (index, package) in self.search_results.iter().enumerate() {
            let selected = index == self.selected;
            let status = match package.installed {
                Some(true) => "Installed",
                Some(false) => "Available",
                None => "Availability unknown",
            };
            let name = truncate(&package.name, 27, self.theme.unicode);
            results.extend(ui::selectable_row(
                self.theme,
                selected,
                &name,
                &format!("{} · {status}", friendly_source(package.source)),
            ));
            if let Some(summary) = &package.summary {
                results.push(Line::from(Span::styled(
                    format!("      {summary}"),
                    self.theme.style(Token::Muted),
                )));
            }
            results.push(Line::from(""));
        }
        frame.render_widget(
            Paragraph::new(Text::from(results))
                .scroll((self.selected.saturating_sub(2) as u16 * 3, 0))
                .wrap(Wrap { trim: false }),
            body_chunks[2],
        );
        ui::page_footer(frame, footer, self.theme, "↑↓ Navigate   Enter Open   Esc Back   ? Help");
    }

    fn draw_package(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(package) = &self.selected_package else {
            self.shell_actions(
                frame,
                area,
                "Package",
                "No package selected",
                vec![ui::empty(self.theme, "Choose software from Find to see its Orbis Brief.")],
                0,
                "Esc Back   / Find   ? Help",
            );
            return;
        };
        let brief = build_brief(package.clone());
        let mut body = vec![
            Line::from(Span::styled(package.name.clone(), self.theme.style(Token::Primary))),
            Line::from(format!(
                "{} · {} · {}",
                friendly_source(package.source),
                installed_label(package.installed),
                package.version.as_deref().unwrap_or("version unknown"),
            )),
            Line::from(Span::styled(
                brief.headline.clone(),
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            ui::section_title(self.theme, "WHAT IT DOES"),
        ];
        body.extend(brief.paragraphs.into_iter().map(Line::from));
        if !brief.examples.is_empty() {
            body.extend([Line::from(""), ui::section_title(self.theme, "WHY YOU MIGHT WANT IT")]);
            body.extend(brief.examples.into_iter().map(|example| {
                Line::from(format!("  {} {example}", self.theme.mark(Token::Primary)))
            }));
        }
        body.extend([
            Line::from(""),
            ui::section_title(self.theme, "DETAILS"),
            ui::info_row(self.theme, "From", friendly_source(package.source)),
            ui::info_row(
                self.theme,
                "Type",
                package.kind.map_or("Not classified", |kind| kind.label()),
            ),
            ui::info_row(self.theme, "Scope", package_scope_label(package.source)),
            ui::info_row(self.theme, "Installed", installed_label(package.installed)),
            Line::from(""),
            ui::section_title(self.theme, "EVIDENCE"),
            ui::info_row(self.theme, "Confidence", brief.confidence.clone()),
        ]);
        for evidence in brief.evidence {
            body.push(Line::from(Span::styled(
                format!("  · {}", evidence.detail),
                self.theme.style(Token::Muted),
            )));
        }
        if let Some(caution) = brief.caution {
            body.extend([Line::from(""), ui::warning(self.theme, &caution)]);
        }
        self.shell_actions(
            frame,
            area,
            &package.name,
            &brief.headline,
            body,
            0,
            "I Install/Update   R Remove   W Why   Esc Back   ? Help",
        );
    }

    fn draw_updates(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = Vec::new();
        match &self.updates {
            None => body.push(ui::loading(self.theme, "Checking software sources…")),
            Some(report) if self.update_check_running => {
                if report.candidates.is_empty() {
                    body.push(ui::loading(self.theme, "Waiting for update results…"));
                } else {
                    body.push(Line::from(Span::styled(
                        format!(
                            "{} update{} found so far",
                            report.total(),
                            if report.total() == 1 { "" } else { "s" }
                        ),
                        self.theme.style(Token::Foreground),
                    )));
                    body.extend(update_group_lines(self.theme, report, true));
                    body.extend([Line::from(""), ui::section_title(self.theme, "DEVELOPER TOOLS")]);
                    body.extend(update_group_lines(self.theme, report, false));
                }
            }
            Some(report) if report.candidates.is_empty() => {
                body.push(ui::empty(self.theme, "No updates available."));
                for inventory in &report.inventories {
                    if !inventory.available
                        || inventory.metadata_state.as_deref().is_some_and(|state| {
                            state.contains("incomplete") || state.contains("unknown")
                        })
                    {
                        body.push(Line::from(Span::styled(
                            format!(
                                "  {:<24} Update status unavailable",
                                friendly_source(inventory.source)
                            ),
                            self.theme.style(Token::Caution),
                        )));
                    }
                }
            }
            Some(report) => {
                body.extend(update_group_lines(self.theme, report, true));
                body.extend([Line::from(""), ui::section_title(self.theme, "DEVELOPER TOOLS")]);
                body.extend(update_group_lines(self.theme, report, false));
            }
        }
        if self.update_check_running {
            body.extend([Line::from(""), ui::section_title(self.theme, "SOURCES")]);
            for (source, status) in &self.maintenance_provider_status {
                let token = if status == "Done" {
                    Token::Positive
                } else if status == "Needs attention" {
                    Token::Caution
                } else {
                    Token::Muted
                };
                body.push(ui::provider_row(
                    self.theme,
                    self.theme.mark(token),
                    friendly_source(*source),
                    status,
                    token,
                ));
            }
        }
        let subtitle = if self.update_check_running {
            "Checking software sources…".to_owned()
        } else {
            update_summary(self.updates.as_ref(), self.snapshot_loading)
        };
        self.shell_actions(
            frame,
            area,
            "Updates",
            &subtitle,
            body,
            0,
            if self.update_check_running {
                "Checking…   Esc Back   ? Help"
            } else {
                "U Review plan   R Refresh   Esc Back   ? Help"
            },
        );
    }

    fn draw_sources(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = Vec::new();
        if let Some(sources) = &self.sources {
            body.push(ui::section_title(self.theme, "SYSTEM & DESKTOP"));
            let mut developer_heading = false;
            // `info_row` reserves fifteen cells for its label, so bound the
            // detail itself to the remaining page width and keep every source
            // on exactly two compact rows.
            let detail_width = ui::inset(area).width.saturating_sub(16) as usize;
            for source in sources {
                let system = matches!(
                    source.source,
                    PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap
                );
                if !system && !developer_heading {
                    body.push(ui::section_title(self.theme, "DEVELOPER TOOLS"));
                    developer_heading = true;
                }
                let token = state_token(&source.state);
                body.push(ui::provider_row(
                    self.theme,
                    self.theme.mark(token),
                    source.source.label(),
                    &state_label(&source.state),
                    token,
                ));
                body.push(ui::info_row(
                    self.theme,
                    "Backend",
                    truncate(
                        &format!(
                            "{} · {}{}",
                            source.backend.as_deref().unwrap_or("Not detected"),
                            capability_summary(source),
                            source
                                .notes
                                .first()
                                .map(|note| format!(" · {note}"))
                                .unwrap_or_default()
                        ),
                        detail_width,
                        self.theme.unicode,
                    ),
                ));
            }
        } else {
            body.push(ui::loading(self.theme, "Checking providers…"));
        }
        self.shell_actions(
            frame,
            area,
            "Sources",
            "Software source details for advanced users",
            body,
            self.navigation_scroll as u16,
            "↑↓ Navigate   R Refresh   Esc Back   ? Help",
        );
    }

    fn draw_health(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = Vec::new();
        if self.doctor_loading && self.doctor.is_none() {
            body.push(ui::loading(self.theme, "Checking that everything is working…"));
        } else if let Some(report) = &self.doctor {
            let issue_count = report
                .checks
                .iter()
                .filter(|check| !check.passed && check.area != "Environment")
                .count();
            body.push(if issue_count == 0 {
                ui::status_chip(
                    self.theme,
                    self.theme.mark(Token::Positive),
                    "Everything looks good.",
                    Token::Positive,
                )
            } else {
                ui::warning(
                    self.theme,
                    &format!(
                        "{issue_count} thing{} need attention.",
                        if issue_count == 1 { "" } else { "s" }
                    ),
                )
            });
            body.push(Line::from(""));
            body.push(ui::section_title(self.theme, "SYSTEM & DESKTOP"));
            for check in report
                .checks
                .iter()
                .filter(|check| check.area != "Environment" && is_system_area(&check.area))
            {
                let token = if check.passed { Token::Positive } else { Token::Caution };
                body.push(ui::provider_row(
                    self.theme,
                    self.theme.mark(token),
                    friendly_short_area(&check.area),
                    &friendly_state(&check.title),
                    token,
                ));
            }
            body.extend([Line::from(""), ui::section_title(self.theme, "DEVELOPER TOOLS")]);
            for check in report
                .checks
                .iter()
                .filter(|check| check.area != "Environment" && !is_system_area(&check.area))
            {
                let token = if check.passed { Token::Positive } else { Token::Caution };
                body.push(ui::provider_row(
                    self.theme,
                    self.theme.mark(token),
                    friendly_short_area(&check.area),
                    &friendly_state(&check.title),
                    token,
                ));
            }
            let issues = report.checks.iter().filter(|check| !check.passed);
            if issues.clone().next().is_some() {
                body.extend([Line::from(""), ui::section_title(self.theme, "ISSUES")]);
                for issue in issues {
                    body.push(Line::from(Span::styled(
                        format!("{}  {}", friendly_short_area(&issue.area), issue.title),
                        self.theme.style(Token::Caution),
                    )));
                    body.push(Line::from(Span::styled(
                        format!("  {}", issue.message),
                        self.theme.style(Token::Muted),
                    )));
                }
            }
            body.extend([Line::from(""), ui::section_title(self.theme, "ENVIRONMENT")]);
            for check in report.checks.iter().filter(|check| check.area == "Environment") {
                let token = if check.passed { Token::Positive } else { Token::Caution };
                body.push(ui::status_chip(self.theme, self.theme.mark(token), &check.title, token));
            }
        } else {
            body.push(ui::empty(self.theme, "No diagnostic report is available."));
        }
        self.shell_actions(
            frame,
            area,
            "Health",
            "A clear view of what needs attention",
            body,
            self.navigation_scroll as u16,
            "↑↓ Navigate   R Recheck   Esc Back   ? Help",
        );
    }

    fn draw_advanced(&self, frame: &mut Frame<'_>, area: Rect) {
        let body = vec![
            ui::section_title(self.theme, "TOOLS FOR ADVANCED USERS"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "S  Software sources",
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            )]),
            Line::from(Span::styled(
                "   Inspect package managers, backends and capabilities",
                self.theme.style(Token::Muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "D  Diagnostics",
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "   Detailed health checks and remediation information",
                self.theme.style(Token::Muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "R  Raw details",
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "   Exact source names and technical package metadata",
                self.theme.style(Token::Muted),
            )),
        ];
        self.shell_actions(
            frame,
            area,
            "Advanced",
            "Technical views for when you need more detail",
            body,
            0,
            "S Sources   D Diagnostics   Esc Back   ? Help",
        );
    }

    fn draw_history(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = Vec::new();
        if self.history.is_empty() {
            body.push(ui::empty(self.theme, "No Orbis operations recorded yet."));
        }
        let mut previous_day = None;
        for (index, entry) in self.history.iter().enumerate() {
            let day = history_day_label(entry.recorded_at_unix_ms);
            if previous_day != Some(day) {
                if previous_day.is_some() {
                    body.push(Line::from(""));
                }
                body.push(ui::section_title(self.theme, day));
                previous_day = Some(day);
            }
            let selected = index == self.history_selected;
            let token = history_token(&entry.status);
            let source = entry.source.map_or("Orbis", friendly_source);
            let target = entry.package.as_deref().unwrap_or("Coordinated provider update");
            let title = format!(
                "{} {} {}",
                history_time(entry.recorded_at_unix_ms),
                if selected { ">" } else { "·" },
                entry.action
            );
            body.push(Line::from(vec![
                Span::styled(
                    title,
                    if selected {
                        self.theme.style(Token::Selected).add_modifier(Modifier::BOLD)
                    } else {
                        self.theme.style(token)
                    },
                ),
                Span::styled(format!("  {}", status_label(&entry.status)), self.theme.style(token)),
            ]));
            body.push(Line::from(Span::styled(
                format!("    {source}:{target}"),
                if selected {
                    self.theme.style(Token::Selected)
                } else {
                    self.theme.style(Token::Foreground)
                },
            )));
            body.push(Line::from(Span::styled(
                format!("    {}", entry.operation_id),
                self.theme.style(Token::Muted),
            )));
        }
        body.push(ui::action_hint(self.theme, "Enter opens details"));
        self.shell_scrolled_actions(
            frame,
            area,
            "History",
            "A timeline of confirmed Orbis operations",
            body,
            self.history_selected.saturating_sub(8) as u16,
            "↑↓ Navigate   Enter Details   Esc Back   ? Help",
        );
    }

    fn draw_history_detail(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = vec![ui::section_title(self.theme, "OPERATION DETAILS")];
        if let Some(serde_json::Value::Object(fields)) = &self.history_detail {
            for (key, value) in fields {
                let label = key.replace('_', " ");
                let label = title_case(&label);
                let value = match value {
                    serde_json::Value::String(value) => value.clone(),
                    _ => value.to_string(),
                };
                body.push(ui::info_row(self.theme, &label, value));
            }
        } else {
            body.push(ui::empty(self.theme, "No history record selected."));
        }
        self.shell_actions(
            frame,
            area,
            "History detail",
            "A sanitized record with execution context",
            body,
            0,
            "Esc Back   ? Help",
        );
    }

    fn draw_why(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = Vec::new();
        if self.why_loading {
            body.push(ui::loading(self.theme, "Checking why this software is installed…"));
        } else if let Some(report) = &self.why {
            body.extend([
                ui::section_title(self.theme, "REQUIRED"),
                Line::from(report.installed_as.clone()),
            ]);
            if !report.used_by.is_empty() {
                body.extend([Line::from(""), ui::section_title(self.theme, "USED BY")]);
                body.extend(report.used_by.iter().map(|consumer| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {}", consumer.name),
                            self.theme.style(Token::Foreground),
                        ),
                        Span::styled(
                            format!(" · {}", consumer.relationship),
                            self.theme.style(Token::Muted),
                        ),
                    ])
                }));
            }
            body.extend([
                Line::from(""),
                ui::section_title(self.theme, "REMOVAL CONTEXT"),
                Line::from(report.removal_advice.clone()),
            ]);
            if !report.evidence.is_empty() {
                body.extend([Line::from(""), ui::section_title(self.theme, "EVIDENCE")]);
                body.extend(report.evidence.iter().map(|evidence| {
                    Line::from(Span::styled(
                        format!("  · {evidence}"),
                        self.theme.style(Token::Muted),
                    ))
                }));
            }
            for note in &report.notes {
                body.push(ui::warning(self.theme, note));
            }
        } else {
            body.push(ui::empty(self.theme, "No explanation is available."));
        }
        let title = self
            .why
            .as_ref()
            .map(|report| format!("Why is {} installed?", report.package.name))
            .unwrap_or_else(|| "Why is this installed?".into());
        self.shell_actions(frame, area, "Why", &title, body, 0, "Esc Back   ? Help");
    }

    fn draw_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered(area, 58, 15);
        frame.render_widget(Clear, popup);
        let text = vec![
            Line::from(Span::styled("Keyboard", self.theme.style(Token::Primary))),
            Line::from(""),
            Line::from("↑ ↓ / j k   move through results"),
            Line::from("Enter       open or select"),
            Line::from("Esc         back / close"),
            Line::from("/           search"),
            Line::from("u           check and review updates"),
            Line::from("c           review safe cleanup"),
            Line::from("h           health · a advanced views · t history"),
            Line::from("?           this help"),
            Line::from("q           quit · Ctrl-C quit"),
        ];
        frame.render_widget(
            Paragraph::new(Text::from(text))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(self.theme.style(Token::Divider))
                        .style(self.theme.style(Token::Surface)),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_confirm(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered(area, 68, 16);
        frame.render_widget(Clear, popup);
        let title = self.plan.as_ref().map_or_else(
            || " REVIEW PACKAGE CHANGE ".to_owned(),
            |(_, plan)| format!(" {} {}? ", plan.action.label(), plan.target.name),
        );
        let mut lines = Vec::new();
        if self.loading_plan {
            lines.push(ui::loading(self.theme, "Preparing a safe review…"));
        } else if let Some((_, plan)) = &self.plan {
            if !plan.executable() {
                lines.extend([
                    ui::error(
                        self.theme,
                        "Orbis cannot safely confirm everything this may change.",
                    ),
                    Line::from("Nothing has been installed."),
                    Line::from(""),
                    ui::section_title(self.theme, "TRY THIS"),
                    Line::from("Choose a different source or open technical details."),
                ]);
            }
            lines.extend([ui::section_title(self.theme, "THIS WILL")]);
            if plan.changes.is_empty() {
                lines.push(Line::from("  • Change the selected software"));
            } else {
                lines.extend(plan.changes.iter().map(|change| {
                    Line::from(Span::styled(
                        format!("  • {}", change.name.as_deref().unwrap_or(&change.package_id)),
                        self.theme.style(Token::Foreground),
                    ))
                }));
            }
            lines.extend([
                Line::from(""),
                ui::section_title(self.theme, "FROM"),
                ui::info_row(self.theme, "Source", friendly_source(plan.target.source)),
                ui::info_row(
                    self.theme,
                    "Version",
                    plan.target.version.as_deref().unwrap_or("Not confirmed"),
                ),
                ui::info_row(self.theme, "Disk change", format_size_delta(plan)),
                ui::info_row(self.theme, "Permission", privilege_label(plan.privilege)),
            ]);
            if !plan.warnings.is_empty() || !plan.executable() {
                lines.extend([Line::from(""), ui::section_title(self.theme, "DETAILS")]);
                lines.push(ui::info_row(self.theme, "Risk", plan.risk.label()));
                lines.push(ui::info_row(
                    self.theme,
                    "Confidence",
                    confidence_label(plan.confidence),
                ));
            }
            lines.extend(
                plan.warnings.iter().map(|warning| ui::warning(self.theme, &warning.message)),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(if plan.executable() {
                "Enter Install     Esc Cancel"
            } else {
                "Blocked review     Esc Back"
            }));
        } else {
            lines.push(ui::empty(self.theme, "No safe review is available."));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    ui::detail_panel(self.theme, &title)
                        .border_style(self.theme.style(Token::Primary))
                        .style(self.theme.style(Token::Surface)),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_maintenance(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = Vec::new();
        let item_label = match self.maintenance_action {
            MaintenanceAction::Upgrade => "update",
            MaintenanceAction::Cleanup => "cleanup item",
            MaintenanceAction::Refresh => "refresh item",
        };
        if let Some(plan) = &self.maintenance {
            let mut grouped = BTreeMap::<PackageSource, (usize, bool, Option<String>)>::new();
            for provider in &plan.providers {
                let count = provider.candidates.len().max(provider.cleanup_candidates.len());
                if count == 0 && provider.executable() {
                    continue;
                }
                let entry = grouped.entry(provider.source).or_insert((0, false, None));
                entry.0 += count;
                entry.1 |= provider.executable();
                if entry.2.is_none() {
                    entry.2 = provider.warnings.first().map(|warning| warning.message.clone());
                }
            }
            let total: usize = grouped.values().map(|(count, _, _)| *count).sum();
            let provider_count = grouped.values().filter(|(count, _, _)| *count > 0).count();
            let requires_admin = plan.providers.iter().any(|provider| {
                let count = provider.candidates.len().max(provider.cleanup_candidates.len());
                (count > 0 || !provider.executable())
                    && provider.privilege != orbis_core::transaction::PrivilegeRequirement::None
            });
            let summary = match self.maintenance_action {
                MaintenanceAction::Refresh => format!(
                    "Refresh software information from {} source{}",
                    plan.providers.len(),
                    if plan.providers.len() == 1 { "" } else { "s" }
                ),
                MaintenanceAction::Upgrade if total == 0 => "No updates available.".to_owned(),
                MaintenanceAction::Cleanup if total == 0 => {
                    "No safely removable items were found.".to_owned()
                }
                _ => format!(
                    "{} {}{} across {} source{}",
                    total,
                    item_label,
                    if total == 1 { "" } else { "s" },
                    provider_count,
                    if provider_count == 1 { "" } else { "s" }
                ),
            };
            lines.push(Line::from(summary));
            for (source, (count, executable, warning)) in
                grouped.iter().filter(|(_, (count, _, _))| *count > 0)
            {
                lines.extend([
                    Line::from(""),
                    ui::section_title(self.theme, friendly_source(*source)),
                    ui::info_row(
                        self.theme,
                        "Items",
                        format!("{count} item{}", if *count == 1 { "" } else { "s" }),
                    ),
                    ui::info_row(
                        self.theme,
                        "Plan status",
                        if *executable { "Ready to review" } else { "Blocked" },
                    ),
                ]);
                if let Some(warning) = warning {
                    lines.push(ui::warning(self.theme, warning));
                }
            }
            let blocked = grouped.values().filter(|(count, _, _)| *count == 0).count();
            if blocked > 0 {
                lines.push(Line::from(""));
                lines.push(ui::section_title(self.theme, "NOT AVAILABLE"));
                let detail_width = ui::inset(area).width.saturating_sub(2) as usize;
                for (source, (_, _, warning)) in
                    grouped.iter().filter(|(_, (count, _, _))| *count == 0)
                {
                    let detail = warning.as_deref().unwrap_or("No executable plan is available.");
                    lines.push(ui::warning(
                        self.theme,
                        &truncate(
                            &format!("{} · {detail}", friendly_source(*source)),
                            detail_width,
                            self.theme.unicode,
                        ),
                    ));
                }
            }
            lines.extend([
                Line::from(""),
                ui::section_title(self.theme, "RISK"),
                ui::info_row(self.theme, "Level", plan.risk.label()),
                ui::section_title(self.theme, "ACCESS"),
                ui::info_row(
                    self.theme,
                    "Administrator",
                    if requires_admin { "Required for some providers" } else { "Not required" },
                ),
                Line::from(""),
                ui::empty(self.theme, "This review is read-only. No package state has changed."),
            ]);
        } else {
            lines.push(ui::loading(
                self.theme,
                &format!(
                    "Building coordinated {} plan…",
                    self.maintenance_action.label().to_ascii_lowercase()
                ),
            ));
        }
        let title = match self.maintenance_action {
            MaintenanceAction::Refresh => "Refresh software information".to_owned(),
            MaintenanceAction::Upgrade => "Update plan".to_owned(),
            MaintenanceAction::Cleanup => "Cleanup plan".to_owned(),
        };
        let actions = self.maintenance.as_ref().map_or("Esc Back   ? Help", |plan| {
            if plan.executable() && plan.mutates {
                "Enter Apply   Esc Back   ? Help"
            } else if plan.executable() {
                "Enter Continue   Esc Back   ? Help"
            } else {
                "Esc Back   ? Help"
            }
        });
        self.shell_actions(
            frame,
            area,
            &title,
            "Review the read-only plan before applying changes",
            lines,
            0,
            actions,
        );
    }

    fn draw_progress(&self, frame: &mut Frame<'_>, area: Rect) {
        let [header, body_area, footer] = ui::page_chunks(area);
        ui::page_header(
            frame,
            header,
            self.theme,
            &self.progress_title,
            if self.progress_context.is_empty() {
                "Operation in progress"
            } else {
                &self.progress_context
            },
        );
        let top_height = if self.progress_maintenance { 8 } else { 7 };
        let chunks =
            Layout::vertical([Constraint::Length(top_height), Constraint::Min(5)]).split(body_area);

        if self.progress_maintenance && body_area.width >= 90 {
            let columns =
                Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(chunks[0]);
            self.draw_progress_stages(frame, columns[0]);
            self.draw_provider_activity(frame, columns[1]);
        } else {
            self.draw_progress_stages(frame, chunks[0]);
        }

        let output_lines: Vec<Line<'static>> = if self.progress_show_details {
            self.progress_output
                .iter()
                .map(|line| Line::from(Span::styled(line.clone(), self.theme.style(Token::Muted))))
                .collect()
        } else {
            vec![ui::empty(
                self.theme,
                if self.progress_output.is_empty() {
                    "Provider details will appear here when available."
                } else {
                    "Provider details are available. Press L to view the bounded output."
                },
            )]
        };

        let visible_height = chunks[1].height.saturating_sub(2) as usize;
        let total_lines = output_lines.len();
        let max_scroll = total_lines.saturating_sub(visible_height);
        let scroll_offset = if self.progress_scroll == 0 {
            max_scroll as u16
        } else {
            max_scroll.saturating_sub(self.progress_scroll) as u16
        };

        let output_block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.style(Token::Divider))
            .title(Span::styled(
                if self.progress_show_details {
                    " PROVIDER OUTPUT · DETAILS "
                } else {
                    " DETAILS "
                },
                self.theme.style(Token::Section),
            ));

        let output_paragraph =
            Paragraph::new(Text::from(output_lines)).block(output_block).scroll((scroll_offset, 0));
        frame.render_widget(output_paragraph, chunks[1]);

        ui::page_footer(
            frame,
            footer,
            self.theme,
            if self.progress_maintenance {
                if self.progress_show_details {
                    "↑↓ Scroll details   L Hide details   Esc unavailable   ? Help"
                } else {
                    "L Show details   Esc unavailable while active   ? Help"
                }
            } else {
                if self.progress_show_details {
                    "↑↓ Scroll output   L Hide details   Esc unavailable   ? Help"
                } else {
                    "L Show details   Esc unavailable while active   ? Help"
                }
            },
        );
    }

    fn draw_progress_stages(&self, frame: &mut Frame<'_>, area: Rect) {
        let stages = orbis_core::progress::ExecutionStage::transaction_stages();
        let mut stage_lines = vec![ui::section_title(self.theme, "PROGRESS")];
        let current = self.progress_stage;
        let mut found_current = false;
        for &stage in stages {
            let is_current = stage == current;
            if is_current {
                found_current = true;
            }
            let state = if is_current {
                if stage.is_terminal() { StageState::Done } else { StageState::Active }
            } else if !found_current {
                StageState::Done
            } else {
                StageState::Pending
            };
            let token = match state {
                StageState::Done => Token::Positive,
                StageState::Active => Token::Caution,
                StageState::Pending => Token::Muted,
            };
            let label = if is_current && !stage.is_terminal() {
                format!("{} …", beginner_stage_label_for(stage, &self.progress_title))
            } else {
                beginner_stage_label_for(stage, &self.progress_title).to_string()
            };
            stage_lines.push(ui::progress_stage(
                self.theme,
                if is_current && !stage.is_terminal() {
                    self.spinner_mark()
                } else {
                    self.theme.stage_mark(state)
                },
                &label,
                if is_current { Token::Foreground } else { token },
            ));
        }
        frame.render_widget(Paragraph::new(Text::from(stage_lines)), area);
    }

    fn spinner_mark(&self) -> &'static str {
        if self.theme.unicode {
            const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            FRAMES[self.spinner_index % FRAMES.len()]
        } else {
            ["|", "/", "-", "\\"][self.spinner_index % 4]
        }
    }

    fn draw_provider_activity(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = vec![ui::section_title(self.theme, "SOURCES")];
        for (source, status) in &self.maintenance_provider_status {
            let token = if status == "Done" {
                Token::Positive
            } else if status == "Needs attention" {
                Token::Caution
            } else {
                Token::Muted
            };
            lines.push(ui::provider_row(
                self.theme,
                self.theme.mark(token),
                friendly_source(*source),
                status,
                token,
            ));
        }
        if self.maintenance_provider_status.is_empty() {
            lines.push(ui::loading(self.theme, "Waiting for sources…"));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    fn draw_result(&self, frame: &mut Frame<'_>, area: Rect) {
        let (title, subtitle, lines) = if let Some(result) = &self.transaction_result {
            let (title, token, subtitle) = match result.status {
                orbis_core::transaction::TransactionStatus::Succeeded => (
                    "Completed",
                    Token::Positive,
                    format!(
                        "{} {} successfully",
                        result.plan.target.name,
                        match result.plan.action {
                            OperationAction::Install => "installed",
                            OperationAction::Remove => "removed",
                        }
                    ),
                ),
                orbis_core::transaction::TransactionStatus::PartiallyVerified => (
                    "Completed with limited verification",
                    Token::Caution,
                    format!(
                        "{} changed, but some verification remains limited",
                        result.plan.target.name
                    ),
                ),
                orbis_core::transaction::TransactionStatus::Failed => (
                    "Installation failed",
                    Token::Destructive,
                    format!(
                        "{} could not complete the operation",
                        friendly_source(result.plan.target.source)
                    ),
                ),
            };
            let mut body =
                vec![ui::result_summary(self.theme, self.theme.mark(token), &subtitle, token)];
            if matches!(result.status, orbis_core::transaction::TransactionStatus::Failed) {
                body.extend([
                    Line::from(""),
                    ui::section_title(self.theme, "WHAT HAPPENED"),
                    ui::error(self.theme, result.execution.message.as_deref().unwrap_or("The provider did not complete the requested operation.")),
                    Line::from(""),
                    ui::section_title(self.theme, "NEXT STEP"),
                    ui::empty(self.theme, "Review the provider output or try again after resolving the reported issue."),
                ]);
            }
            body.extend([
                Line::from(""),
                ui::section_title(self.theme, "DETAILS"),
                ui::info_row(self.theme, "Source", result.plan.target.source.to_string()),
                ui::info_row(self.theme, "Verification", verification_label(result.verification)),
                ui::info_row(self.theme, "Operation ID", result.plan.operation_id.clone()),
            ]);
            (title.to_owned(), subtitle, body)
        } else if let Some(result) = &self.maintenance_result {
            let (title, token) = match result.status {
                orbis_core::maintenance::MaintenanceStatus::Succeeded => {
                    ("Completed", Token::Positive)
                }
                orbis_core::maintenance::MaintenanceStatus::PartiallySucceeded => {
                    ("Partially completed", Token::Caution)
                }
                orbis_core::maintenance::MaintenanceStatus::Failed => {
                    ("Maintenance failed", Token::Destructive)
                }
                orbis_core::maintenance::MaintenanceStatus::Cancelled => {
                    ("Maintenance cancelled", Token::Caution)
                }
                orbis_core::maintenance::MaintenanceStatus::Blocked => {
                    ("Maintenance blocked", Token::Destructive)
                }
            };
            let subtitle = format!("{} provider maintenance", result.action.label());
            let body = vec![
                ui::status_chip(
                    self.theme,
                    self.theme.mark(token),
                    maintenance_status_label(&result.status),
                    token,
                ),
                Line::from(""),
                ui::section_title(self.theme, "DETAILS"),
                ui::info_row(self.theme, "Operation ID", result.operation_id.clone()),
            ];
            (title.to_owned(), subtitle, body)
        } else {
            let error = self.error.as_deref().unwrap_or("No result is available.");
            (
                "Operation failed".to_owned(),
                "Orbis could not complete the operation".to_owned(),
                vec![
                    ui::error(self.theme, error),
                    Line::from(""),
                    ui::section_title(self.theme, "NEXT STEP"),
                    ui::empty(
                        self.theme,
                        "Review the details and try again when the provider is ready.",
                    ),
                ],
            )
        };

        self.shell_actions(
            frame,
            area,
            &title,
            &subtitle,
            lines,
            0,
            "Enter / Esc Return   H History   ? Help",
        );
    }
}

fn state_label(state: &str) -> String {
    match state {
        "ready" => "Ready".into(),
        "unavailable" => "Unavailable".into(),
        "restricted" => "Restricted".into(),
        "checking" => "Checking".into(),
        other => other.to_owned(),
    }
}

fn friendly_source(source: PackageSource) -> &'static str {
    match source {
        PackageSource::Apt => "Ubuntu repositories",
        PackageSource::Flatpak => "Flatpak apps",
        PackageSource::Snap => "Snap Store",
        PackageSource::Cargo => "Rust tools",
        PackageSource::Npm | PackageSource::Pnpm => "Node.js tools",
        PackageSource::Uv | PackageSource::Pipx => "Python tools",
    }
}

fn friendly_area(area: &str) -> &'static str {
    match area {
        "APT" => "Ubuntu repositories",
        "Flatpak" => "Flatpak apps",
        "Snap" => "Snap Store",
        "Cargo" => "Rust tools",
        "npm" | "pnpm" => "Node.js tools",
        "uv" | "pipx" => "Python tools",
        "Environment" => "Environment",
        _ => "Software tools",
    }
}

fn friendly_short_area(area: &str) -> &'static str {
    match area {
        "APT" => "Ubuntu repos",
        "Flatpak" => "Flatpak apps",
        "Snap" => "Snap Store",
        "Cargo" => "Rust tools",
        "npm" | "pnpm" => "Node.js tools",
        "uv" | "pipx" => "Python tools",
        _ => friendly_area(area),
    }
}

fn friendly_state(title: &str) -> String {
    let lower = title.to_ascii_lowercase();
    if lower.contains("not installed") || lower.contains("unavailable") {
        "Optional or not installed".into()
    } else if lower.contains("restricted") || lower.contains("permission") {
        "Needs attention".into()
    } else if lower.contains("ready") || lower.contains("available") || lower.contains("pass") {
        "Ready".into()
    } else {
        title.to_owned()
    }
}

fn state_token(state: &str) -> Token {
    match state {
        "ready" => Token::Positive,
        "unavailable" => Token::Unavailable,
        _ => Token::Caution,
    }
}

fn is_system_area(area: &str) -> bool {
    matches!(area, "APT" | "Flatpak" | "Snap")
}

fn installed_label(installed: Option<bool>) -> &'static str {
    match installed {
        Some(true) => "Installed",
        Some(false) => "Available",
        None => "State unknown",
    }
}

fn package_scope_label(source: PackageSource) -> &'static str {
    match source {
        PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap => "System",
        PackageSource::Cargo
        | PackageSource::Npm
        | PackageSource::Pnpm
        | PackageSource::Uv
        | PackageSource::Pipx => "User",
    }
}

fn update_summary(report: Option<&UpdateInventoryReport>, loading: bool) -> String {
    match report {
        None if loading => "Checking for updates…".into(),
        None => "Waiting for update information".into(),
        Some(report) if report.candidates.is_empty() => "You're up to date".into(),
        Some(report) => format!(
            "{} update{} available",
            report.total(),
            if report.total() == 1 { "" } else { "s" }
        ),
    }
}

fn update_group_lines(
    theme: Theme,
    report: &UpdateInventoryReport,
    system: bool,
) -> Vec<Line<'static>> {
    let mut groups = BTreeMap::<PackageSource, Vec<&UpdateCandidate>>::new();
    for candidate in &report.candidates {
        let candidate_is_system = matches!(
            candidate.source,
            PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap
        );
        if candidate_is_system == system {
            groups.entry(candidate.source).or_default().push(candidate);
        }
    }

    let mut lines = Vec::new();
    for (source, candidates) in groups {
        lines.push(Line::from(vec![
            Span::styled(friendly_source(source).to_owned(), theme.style(Token::Foreground)),
            Span::styled(
                format!(
                    "{:>width$}",
                    candidates.len(),
                    width = 42usize.saturating_sub(friendly_source(source).len())
                ),
                theme.style(Token::Muted),
            ),
        ]));
        for candidate in candidates {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<24}", truncate(&candidate.name, 24, theme.unicode)),
                    theme.style(Token::Foreground),
                ),
                Span::styled(
                    format!(
                        "{} → {}",
                        candidate.current_version.as_deref().unwrap_or("current"),
                        candidate.available_version.as_deref().unwrap_or("latest")
                    ),
                    theme.style(Token::Muted),
                ),
            ]));
        }
    }

    for inventory in &report.inventories {
        let inventory_is_system = matches!(
            inventory.source,
            PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap
        );
        let incomplete = inventory
            .metadata_state
            .as_deref()
            .is_some_and(|state| state.contains("incomplete") || state.contains("unknown"));
        if inventory_is_system == system && (!inventory.available || incomplete) {
            lines.push(ui::warning(
                theme,
                &format!("{}  Update status unavailable", friendly_source(inventory.source)),
            ));
        }
    }
    if lines.is_empty() {
        lines.push(ui::empty(theme, "No updates in this group."));
    }
    lines
}

fn capability_summary(source: &SourceInfo) -> String {
    let mut capabilities = Vec::new();
    if source.capabilities.search || source.capabilities.info {
        capabilities.push("discover");
    }
    if source.capabilities.updates
        || source.capabilities.upgrade
        || source.capabilities.refresh
        || source.capabilities.cleanup
    {
        capabilities.push("maintain");
    }
    if source.capabilities.install || source.capabilities.remove {
        capabilities.push("change");
    }
    if source.capabilities.why {
        capabilities.push("explain");
    }
    capabilities.join(" · ")
}

fn truncate(value: &str, width: usize, unicode: bool) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    let suffix = if unicode { "…" } else { "..." };
    let keep = width.saturating_sub(suffix.chars().count());
    format!("{}{}", value.chars().take(keep).collect::<String>(), suffix)
}

fn layout_class(width: u16) -> LayoutClass {
    match width {
        0..=89 => LayoutClass::Compact,
        90..=119 => LayoutClass::Normal,
        _ => LayoutClass::Wide,
    }
}

fn confidence_label(value: orbis_core::transaction::PlanConfidence) -> &'static str {
    match value {
        orbis_core::transaction::PlanConfidence::High => "high",
        orbis_core::transaction::PlanConfidence::Medium => "medium",
        orbis_core::transaction::PlanConfidence::Low => "low",
    }
}

fn privilege_label(value: orbis_core::transaction::PrivilegeRequirement) -> &'static str {
    match value {
        orbis_core::transaction::PrivilegeRequirement::None => "Not required",
        orbis_core::transaction::PrivilegeRequirement::Administrator => "Administrator",
    }
}

fn format_size_delta(plan: &OperationPlan) -> String {
    plan.disk_delta_bytes
        .map(|bytes| {
            let sign = if bytes >= 0 { "+" } else { "−" };
            format!("{sign}{}", format_bytes(bytes.unsigned_abs()))
        })
        .or_else(|| plan.download_size_bytes.map(format_bytes))
        .unwrap_or_else(|| "Not available".into())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn beginner_stage_label(stage: orbis_core::progress::ExecutionStage) -> &'static str {
    match stage {
        orbis_core::progress::ExecutionStage::Preparing => "Preparing",
        orbis_core::progress::ExecutionStage::AwaitingConfirmation => "Ready to install",
        orbis_core::progress::ExecutionStage::Authenticating => "Getting permission",
        orbis_core::progress::ExecutionStage::Executing => "Installing",
        orbis_core::progress::ExecutionStage::Verifying => "Checking installation",
        orbis_core::progress::ExecutionStage::SavingResult => "Finishing up",
        orbis_core::progress::ExecutionStage::Completed => "Done",
        orbis_core::progress::ExecutionStage::Failed => "Could not finish",
    }
}

fn beginner_stage_label_for(
    stage: orbis_core::progress::ExecutionStage,
    title: &str,
) -> &'static str {
    if stage == orbis_core::progress::ExecutionStage::Executing {
        let title = title.to_ascii_lowercase();
        if title.contains("remov") {
            return "Removing";
        }
        if title.contains("clean") {
            return "Cleaning up";
        }
        if title.contains("refresh") {
            return "Refreshing";
        }
        if title.contains("updat") {
            return "Updating";
        }
    }
    beginner_stage_label(stage)
}

fn verification_label(value: orbis_core::transaction::VerificationResult) -> &'static str {
    match value {
        orbis_core::transaction::VerificationResult::Verified => "Verified",
        orbis_core::transaction::VerificationResult::PartiallyVerified => "Partially verified",
        orbis_core::transaction::VerificationResult::Failed => "Failed",
    }
}

fn history_token(status: &str) -> Token {
    match status {
        "succeeded" | "completed" => Token::Positive,
        "failed" | "blocked" => Token::Destructive,
        _ => Token::Caution,
    }
}

fn status_label(status: &str) -> String {
    match status {
        "succeeded" => "Completed".into(),
        "failed" => "Failed".into(),
        "planned" => "Planned".into(),
        "executing" => "In progress".into(),
        "partially_verified" => "Limited verification".into(),
        "partially_succeeded" => "Partially completed".into(),
        "cancelled" => "Cancelled".into(),
        "blocked" => "Blocked".into(),
        other => other.to_owned(),
    }
}

fn maintenance_status_label(status: &orbis_core::maintenance::MaintenanceStatus) -> &'static str {
    match status {
        orbis_core::maintenance::MaintenanceStatus::Succeeded => "Completed",
        orbis_core::maintenance::MaintenanceStatus::PartiallySucceeded => "Partially completed",
        orbis_core::maintenance::MaintenanceStatus::Failed => "Failed",
        orbis_core::maintenance::MaintenanceStatus::Cancelled => "Cancelled",
        orbis_core::maintenance::MaintenanceStatus::Blocked => "Blocked",
    }
}

fn history_day_label(timestamp_ms: u64) -> &'static str {
    const DAY_MS: u64 = 86_400_000;
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64 / DAY_MS);
    match today.saturating_sub(timestamp_ms / DAY_MS) {
        0 => "TODAY",
        1 => "YESTERDAY",
        _ => "EARLIER",
    }
}

fn history_time(timestamp_ms: u64) -> String {
    let minutes = timestamp_ms / 60_000 % 1_440;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render_at(width: u16, height: u16) -> String {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(registry, Theme::test(width as usize), tx, rx);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw dashboard");
        terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
    }

    fn render_lines(width: u16, height: u16, unicode: bool) -> Vec<String> {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(width as usize);
        theme.unicode = unicode;
        let mut app = App::new(registry, theme, tx, rx);
        app.sources = Some(registry.sources());
        app.snapshot_loading = false;
        app.updates = Some(UpdateInventoryReport {
            inventories: Vec::new(),
            candidates: Vec::new(),
            issues: Vec::new(),
        });
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw dashboard");
        terminal
            .backend()
            .buffer()
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    fn render_screen_lines(screen: Screen, width: u16, height: u16) -> Vec<String> {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(width as usize);
        theme.unicode = true;
        let mut app = App::new(registry, theme, tx, rx);
        app.screen = screen;
        app.animation = StartupAnimation::disabled();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw screen");
        terminal
            .backend()
            .buffer()
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    fn sample_package() -> Package {
        Package {
            source: PackageSource::Apt,
            provider_id: "btop".into(),
            name: "btop".into(),
            version: Some("1.4.5".into()),
            summary: Some("Resource monitor for CPU, memory, disks and processes".into()),
            description: None,
            installed: Some(true),
            kind: Some(orbis_core::models::PackageKind::CliTool),
            origin: Some("Ubuntu repositories".into()),
            architecture: None,
            homepage: None,
            license: None,
            size_bytes: None,
            metadata: std::collections::BTreeMap::new(),
        }
    }

    fn sample_candidate(source: PackageSource, name: &str) -> UpdateCandidate {
        UpdateCandidate {
            source,
            provider_id: name.into(),
            name: name.into(),
            current_version: Some("1.0".into()),
            available_version: Some("1.1".into()),
            architecture: None,
            scope: None,
            channel: None,
            held: None,
            security_relevance: None,
            notes: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        }
    }

    fn configured_app() -> App<'static> {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(121);
        theme.unicode = true;
        let mut app = App::new(registry, theme, tx, rx);
        let package = sample_package();
        app.sources = Some(vec![
            SourceInfo {
                source: PackageSource::Apt,
                available: true,
                state: "ready".into(),
                backend: Some("Nala".into()),
                capabilities: orbis_core::models::ProviderCapabilities {
                    search: true,
                    info: true,
                    installed_state: true,
                    installed_list: true,
                    mutations: true,
                    install: true,
                    remove: true,
                    updates: true,
                    upgrade: true,
                    refresh: true,
                    cleanup: true,
                    why: true,
                },
                notes: Vec::new(),
            },
            SourceInfo {
                source: PackageSource::Cargo,
                available: true,
                state: "ready".into(),
                backend: Some("Cargo".into()),
                capabilities: orbis_core::models::ProviderCapabilities {
                    search: true,
                    info: true,
                    installed_state: true,
                    installed_list: true,
                    mutations: true,
                    install: true,
                    remove: true,
                    updates: true,
                    upgrade: true,
                    refresh: true,
                    cleanup: false,
                    why: false,
                },
                notes: Vec::new(),
            },
        ]);
        app.updates = Some(UpdateInventoryReport {
            inventories: Vec::new(),
            candidates: vec![sample_candidate(PackageSource::Apt, "curl")],
            issues: Vec::new(),
        });
        app.search_query = "btop".into();
        app.search_results = vec![package.clone()];
        app.selected_package = Some(package.clone());
        app.why = Some(WhyReport {
            package: package.clone(),
            installed_as: "Installed automatically as an APT dependency.".into(),
            used_by: Vec::new(),
            evidence: vec!["APT dependency metadata".into()],
            removal_advice: "APT does not currently consider this package an autoremove candidate."
                .into(),
            orbis_history: Vec::new(),
            notes: Vec::new(),
        });
        let mut plan = OperationPlan::new(
            OperationAction::Install,
            package,
            orbis_core::transaction::InstallScope::System,
        );
        plan.completeness = orbis_core::transaction::PlanCompleteness::Complete;
        plan.confidence = orbis_core::transaction::PlanConfidence::High;
        plan.risk = orbis_core::transaction::RiskLevel::Normal;
        plan.changes.push(orbis_core::transaction::PlannedChange {
            kind: orbis_core::transaction::ChangeKind::Install,
            package_id: "btop".into(),
            name: Some("btop".into()),
            version: Some("1.4.5".into()),
            reason: None,
        });
        let request = OperationRequest {
            action: OperationAction::Install,
            package: PackageRefJson { source: Some(PackageSource::Apt), query: "btop".into() },
            scope: Some(orbis_core::transaction::InstallScope::System),
            channel: None,
        };
        app.plan = Some((request, plan));
        app.maintenance = Some(MaintenancePlan::new(
            MaintenanceAction::Upgrade,
            None,
            vec![orbis_core::maintenance::ProviderMaintenancePlan::blocked(
                PackageSource::Apt,
                MaintenanceAction::Upgrade,
                "Upgrade simulation is unavailable.",
            )],
        ));
        app.progress_title = "Installing btop".into();
        app.progress_context = "APT · System · Administrator access".into();
        app.history.push(orbis_core::transaction::history::HistoryEntry {
            kind: "transaction".into(),
            operation_id: "tx-test-123".into(),
            recorded_at_unix_ms: 1_700_000_000_000,
            action: "Installed".into(),
            source: Some(PackageSource::Apt),
            package: Some("btop".into()),
            scope: Some("system".into()),
            status: "succeeded".into(),
            verification: Some(orbis_core::transaction::VerificationResult::Verified),
            risk: Some(orbis_core::transaction::RiskLevel::Normal),
            message: None,
        });
        app.history_detail =
            Some(serde_json::json!({"operation_id": "tx-test-123", "status": "succeeded"}));
        app.doctor = Some(orbis_core::diagnostics::DoctorReport {
            checks: vec![orbis_core::diagnostics::DiagnosticCheck::passed(
                PackageSource::Apt,
                "APT metadata responds",
            )],
            read_only: true,
        });
        app.doctor_loading = false;
        app
    }

    fn render_configured_screen(screen: Screen) -> Vec<String> {
        let mut app = configured_app();
        app.screen = screen;
        let mut terminal = Terminal::new(TestBackend::new(121, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw configured screen");
        terminal
            .backend()
            .buffer()
            .content
            .chunks(121)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    #[test]
    fn dashboard_renders_at_reference_sizes() {
        for (width, height) in [(121, 24), (80, 24), (100, 30), (140, 40)] {
            let content = render_at(width, height);
            let lines = render_lines(width, height, true);
            assert!(content.contains("ORBIS"));
            assert!(content.contains("What would you like to do?"));
            assert!(content.contains("Find software"));
            assert!(content.contains("Health"));
            assert!(content.contains("Updates"));
            assert!(content.contains("/ Find"));
            assert!(!content.contains("SYSTEM & DESKTOP"));
            assert_eq!(lines.len(), height as usize);
            assert!(lines.last().is_some_and(|line| line.contains("/ Find")));
            assert!(!lines.iter().any(|line| line.contains("candidate(s)")));
        }
    }

    #[test]
    fn dashboard_121x24_has_a_deliberate_full_height_composition() {
        let lines = render_lines(121, 24, true);
        assert!(lines[0].contains("◈ ORBIS"));
        assert!(lines.iter().any(|line| line.contains("What would you like to do?")));
        assert!(lines.iter().any(|line| line.contains("Find software")));
        assert!(lines.iter().any(|line| line.contains("RECENT ACTIVITY")));
        assert!(lines[23].contains("/ Find"));
        assert!(lines[3..22].iter().filter(|line| line.trim().is_empty()).count() < 9);
    }

    #[test]
    fn compact_80x24_uses_the_small_brand_and_keeps_footer_visible() {
        let lines = render_lines(80, 24, false);
        assert!(lines[0].contains("@ ORBIS"));
        assert!(lines.iter().any(|line| line.contains("What would you like to do?")));
        assert!(lines.iter().any(|line| line.contains("Find software")));
        assert!(lines[23].contains("/ Find"));
        assert_eq!(lines.len(), 24);
    }

    #[test]
    fn update_summary_uses_product_language() {
        let report = UpdateInventoryReport {
            inventories: Vec::new(),
            candidates: vec![UpdateCandidate {
                source: PackageSource::Apt,
                provider_id: "btop".into(),
                name: "btop".into(),
                current_version: Some("1.0".into()),
                available_version: Some("1.1".into()),
                architecture: None,
                scope: None,
                channel: None,
                held: None,
                security_relevance: None,
                notes: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
            }],
            issues: Vec::new(),
        };
        assert_eq!(update_summary(Some(&report), false), "1 update available");
        assert!(
            update_group_lines(Theme::test(121), &report, true)
                .iter()
                .any(|line| line.to_string().contains("Ubuntu repositories"))
        );
        assert!(!update_summary(Some(&report), false).contains("candidate"));
    }

    #[test]
    fn every_major_tui_page_has_a_header_content_and_footer() {
        let cases = [
            (Screen::Dashboard, "Find software", "/ Find"),
            (Screen::Search, "SEARCH PACKAGES", "Navigate"),
            (Screen::Package, "Package", "Esc Back"),
            (Screen::Updates, "Updates", "Review plan"),
            (Screen::MaintenanceReview, "plan", "Esc Back"),
            (Screen::Confirm, "REVIEW PACKAGE CHANGE", "Esc Back"),
            (Screen::Progress, "PROVIDER OUTPUT", "Scroll output"),
            (Screen::Result, "Operation failed", "Return"),
            (Screen::Sources, "Checking providers", "Refresh"),
            (Screen::Health, "No diagnostic", "Recheck"),
            (Screen::Advanced, "TOOLS FOR ADVANCED USERS", "Sources"),
            (Screen::History, "History", "Details"),
            (Screen::HistoryDetail, "OPERATION DETAILS", "Esc Back"),
            (Screen::Why, "Why", "Esc Back"),
            (Screen::Help, "Keyboard", "Q Quit"),
        ];
        for (width, height) in [(121, 24), (80, 24), (100, 30), (140, 40)] {
            for (screen, section, footer) in cases {
                let lines = render_screen_lines(screen, width, height);
                assert_eq!(lines.len(), height as usize, "wrong height for {screen:?}");
                assert!(
                    lines.iter().any(|line| line.contains(section)),
                    "missing {section:?} on {screen:?}\n{}",
                    lines.join("\n")
                );
                assert!(
                    lines.last().is_some_and(|line| line.contains(footer)),
                    "missing footer on {screen:?}\n{}",
                    lines.join("\n")
                );
                assert!(lines.iter().take(5).any(|line| line.contains("ORBIS")
                    || line.contains('◈')
                    || line.contains('█')));
            }
        }
    }

    #[test]
    fn search_loading_and_empty_states_use_shared_language() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(121);
        theme.unicode = true;
        let mut app = App::new(registry, theme, tx, rx);
        app.screen = Screen::Search;
        app.search_query = "btop".into();
        app.search_loading = true;
        let mut terminal = Terminal::new(TestBackend::new(121, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw loading search");
        let loading: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(loading.contains("Checking providers"));
        assert!(loading.contains("↑↓ Navigate"));

        app.search_loading = false;
        terminal.draw(|frame| app.draw(frame)).expect("draw empty search");
        let empty: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(empty.contains("No software matched"));
        assert!(!empty.contains("candidate(s)"));
    }

    #[test]
    fn configured_pages_render_primary_content_before_the_footer() {
        let cases = [
            (Screen::Search, "1 result"),
            (Screen::Package, "WHAT IT DOES"),
            (Screen::Updates, "1 update"),
            (Screen::MaintenanceReview, "Update plan"),
            (Screen::Confirm, "THIS WILL"),
            (Screen::Progress, "Installing btop"),
            (Screen::Result, "No result"),
            (Screen::Sources, "discover"),
            (Screen::Health, "SYSTEM & DESKTOP"),
            (Screen::History, "EARLIER"),
            (Screen::HistoryDetail, "OPERATION DETAILS"),
            (Screen::Why, "REQUIRED"),
        ];
        for (screen, expected) in cases {
            let lines = render_configured_screen(screen);
            assert!(
                lines.iter().any(|line| line.contains(expected)),
                "missing {expected:?} on {screen:?}\n{}",
                lines.join("\n")
            );
            assert!(lines.last().is_some_and(|line| !line.trim().is_empty()));
        }
    }

    #[test]
    fn too_small_render_is_explanatory() {
        let content = render_at(48, 12);
        assert!(content.contains("needs a little more room"));
        assert!(content.contains("Minimum recommended"));
    }

    #[test]
    fn progress_screen_renders_stages_and_output() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(registry, Theme::test(80), tx, rx);
        app.screen = Screen::Progress;
        app.progress_title = "Installing btop".into();
        app.progress_stage = orbis_core::progress::ExecutionStage::Executing;
        app.progress_output.push_back("Reading package lists...".into());
        app.progress_output.push_back("Unpacking btop...".into());

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw progress");
        let content: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content.contains("Installing btop"));
        assert!(content.contains("PROGRESS"));
        assert!(content.contains("Installing"));
        assert!(content.contains("PROVIDER OUTPUT"));
        assert!(content.contains("Reading package lists..."));
    }

    #[test]
    fn maintenance_progress_renders_source_activity_and_details() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(121);
        theme.unicode = true;
        let mut app = App::new(registry, theme, tx, rx);
        app.screen = Screen::Progress;
        app.progress_maintenance = true;
        app.progress_title = "Refreshing software information".into();
        app.progress_context = "2 sources · staged safely".into();
        app.progress_stage = orbis_core::progress::ExecutionStage::Executing;
        app.maintenance_provider_status.insert(PackageSource::Apt, "Working…".into());
        app.maintenance_provider_status.insert(PackageSource::Snap, "Waiting".into());
        app.progress_output.push_back("Reading software information…".into());

        let mut terminal = Terminal::new(TestBackend::new(121, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw maintenance progress");
        let content: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content.contains("Refreshing software information"));
        assert!(content.contains("SOURCES"));
        assert!(content.contains("Ubuntu repositories"));
        assert!(content.contains("PROVIDER OUTPUT · DETAILS"));
        assert!(content.contains("↑↓ Scroll details"));
    }

    #[test]
    fn update_check_loading_reserves_its_live_footer() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(121);
        theme.unicode = true;
        let mut app = App::new(registry, theme, tx, rx);
        app.screen = Screen::Updates;
        app.update_check_running = true;
        let mut terminal = Terminal::new(TestBackend::new(121, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw update check");
        let content: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content.contains("Checking software sources"));
        assert!(content.contains("Checking…"));
        assert!(content.contains("◈ ORBIS"));
    }

    #[test]
    fn update_check_renders_inventory_as_each_provider_finishes() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(121);
        theme.unicode = true;
        let mut app = App::new(registry, theme, tx, rx);
        app.screen = Screen::Updates;
        app.update_check_running = true;
        app.accept(WorkerMessage::Progress(
            orbis_core::progress::OperationEvent::ProviderInventory {
                inventory: orbis_core::maintenance::ProviderUpdateInventory {
                    source: PackageSource::Apt,
                    available: true,
                    candidates: vec![sample_candidate(PackageSource::Apt, "curl")],
                    notes: Vec::new(),
                    metadata_state: Some("current_local_index".into()),
                },
            },
        ));
        let mut terminal = Terminal::new(TestBackend::new(121, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw partial update check");
        let content: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content.contains("1 update found so far"));
        assert!(content.contains("curl"));
        assert!(content.contains("Checking…"));
    }

    #[test]
    fn result_screen_renders_summary() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(registry, Theme::test(80), tx, rx);
        app.screen = Screen::Result;
        let target = orbis_core::models::Package {
            source: orbis_core::models::PackageSource::Apt,
            provider_id: "btop".into(),
            name: "btop".into(),
            version: Some("1.0.0".into()),
            summary: Some("Monitor".into()),
            description: None,
            installed: Some(false),
            kind: None,
            origin: None,
            architecture: None,
            homepage: None,
            license: None,
            size_bytes: None,
            metadata: std::collections::BTreeMap::new(),
        };
        let mut plan = orbis_core::transaction::OperationPlan::new(
            orbis_core::transaction::OperationAction::Install,
            target,
            orbis_core::transaction::InstallScope::System,
        );
        plan.operation_id = "tx-test-123".into();
        app.transaction_result = Some(orbis_core::transaction::TransactionResult {
            plan,
            execution: orbis_core::transaction::ExecutionSummary {
                exit_status: Some(0),
                process_succeeded: true,
                message: None,
            },
            verification: orbis_core::transaction::VerificationResult::Verified,
            status: orbis_core::transaction::TransactionStatus::Succeeded,
        });

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw result");
        let content: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content.contains("btop installed successfully"));
        assert!(content.contains("Completed"));
        assert!(content.contains("Verified"));
        assert!(content.contains("tx-test-123"));
    }

    #[test]
    fn startup_animation_steps_progress_correctly() {
        let mut anim = StartupAnimation::for_testing(true);
        assert!(anim.is_active());
        assert_eq!(anim.step_at(50), AnimationStep::Diamond);
        assert_eq!(anim.step_at(140), AnimationStep::Diamond);
        assert_eq!(anim.step_at(180), AnimationStep::Reveal);
        assert_eq!(anim.step_at(440), AnimationStep::Reveal);
        assert_eq!(anim.step_at(500), AnimationStep::Settled);
        assert_eq!(anim.step_at(600), AnimationStep::Settled);
        assert_eq!(anim.step_at(650), AnimationStep::Finished);
        assert!(!anim.is_active());
        assert_eq!(anim.step_at(700), AnimationStep::Finished);
    }

    #[test]
    fn startup_animation_finish_makes_it_inactive() {
        let mut anim = StartupAnimation::for_testing(true);
        assert!(anim.is_active());
        anim.finish();
        assert!(!anim.is_active());
        assert_eq!(anim.step_at(50), AnimationStep::Finished);
    }

    #[test]
    fn startup_animation_disabled_conditions() {
        let ascii_theme = Theme::test(80);
        assert!(!StartupAnimation::should_enable(&ascii_theme));

        let anim = StartupAnimation::disabled();
        assert!(!anim.is_active());
    }

    #[test]
    fn startup_animation_frame_rendering() {
        let registry = Box::leak(Box::new(ProviderRegistry::system()));
        let (tx, rx) = mpsc::channel();
        let mut theme = Theme::test(121);
        theme.unicode = true;
        theme.color = true;
        let mut app = App::new(registry, theme, tx, rx);
        app.animation = StartupAnimation::for_testing(true);

        let mut terminal = Terminal::new(TestBackend::new(121, 24)).expect("test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw frame 0");
        let content0: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content0.contains('◈'));

        app.animation.set_elapsed_ms(300);
        terminal.draw(|frame| app.draw(frame)).expect("draw wordmark");
        let content_wm: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content_wm.contains(" ███  ████"));

        app.animation.set_elapsed_ms(650);
        terminal.draw(|frame| app.draw(frame)).expect("draw finished");
        let content_fin: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(content_fin.contains("Your Linux software, in one place."));
    }
}
