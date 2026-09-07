//! Interactive terminal presentation for Orbis.
//!
//! The application owns only presentation state. Provider reads are performed by
//! short-lived read-only workers and mutations cross the same plan/executor
//! boundary as the ordinary CLI.

use std::{
    collections::VecDeque,
    io::{self, IsTerminal},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use orbis_core::{
    ProviderRegistry,
    explain::build_brief,
    maintenance::{MaintenanceAction, MaintenancePlan, UpdateInventoryReport, WhyReport},
    models::{Package, PackageSource, SourceInfo},
    transaction::{OperationAction, OperationPlan, OperationRequest, PackageRefJson},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    cli::Command,
    commands,
    render::theme::{StageState, Theme, Token},
};

const MIN_WIDTH: u16 = 70;
const MIN_HEIGHT: u16 = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Dashboard,
    Search,
    Package,
    Updates,
    Sources,
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
    Snapshot(Vec<SourceInfo>, UpdateInventoryReport),
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

fn terminal_capable() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| term != "dumb")
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
    search_query: String,
    search_results: Vec<Package>,
    selected: usize,
    search_loading: bool,
    snapshot_loading: bool,
    selected_package: Option<Package>,
    why: Option<WhyReport>,
    history: Vec<orbis_core::transaction::history::HistoryEntry>,
    history_selected: usize,
    history_detail: Option<serde_json::Value>,
    plan: Option<(OperationRequest, OperationPlan)>,
    maintenance: Option<MaintenancePlan>,
    why_loading: bool,
    loading_plan: bool,
    error: Option<String>,
    quit: bool,
    // Progress screen state
    progress_stage: orbis_core::progress::ExecutionStage,
    progress_title: String,
    progress_output: VecDeque<String>,
    progress_scroll: usize,
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
            search_query: String::new(),
            search_results: Vec::new(),
            selected: 0,
            search_loading: false,
            snapshot_loading: false,
            selected_package: None,
            why: None,
            history: Vec::new(),
            history_selected: 0,
            history_detail: None,
            plan: None,
            maintenance: None,
            why_loading: false,
            loading_plan: false,
            error: None,
            quit: false,
            progress_stage: orbis_core::progress::ExecutionStage::Planning,
            progress_title: String::new(),
            progress_output: VecDeque::new(),
            progress_scroll: 0,
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
            if dirty || received {
                terminal.draw(|frame| self.draw(frame))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(100))?
                && let event = event::read()?
            {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
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
            WorkerMessage::Snapshot(sources, updates) => {
                self.sources = Some(sources);
                self.updates = Some(updates);
                self.snapshot_loading = false;
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
                Ok(plan) => self.maintenance = Some(plan),
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
                    self.progress_stage = orbis_core::progress::ExecutionStage::Completed;
                    self.maintenance_result = Some(result);
                    self.screen = Screen::Result;
                    self.refresh();
                }
                Err(error) => {
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
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let registry = ProviderRegistry::system();
            let sources = registry.sources();
            let updates = registry.updates(None);
            let _ = tx.send(WorkerMessage::Snapshot(sources, updates));
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
        self.previous = self.screen;
        self.screen = screen;
        self.error = None;
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
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('u')) {
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
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn handle_dashboard(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('/') => {
                self.search_query.clear();
                self.open(Screen::Search);
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('u' | 'U') => self.open(Screen::Updates),
            KeyCode::Char('s' | 'S') => self.open(Screen::Sources),
            KeyCode::Char('h' | 'H') => self.open(Screen::History),
            _ => {}
        }
    }

    fn handle_navigation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Dashboard,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open(Screen::Help),
            KeyCode::Char('r') => self.refresh(),
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
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('u' | 'U') => self.request_maintenance(),
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

    fn request_maintenance(&mut self) {
        self.maintenance = None;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result =
                ProviderRegistry::system().maintenance_plan(MaintenanceAction::Upgrade, None);
            let _ = tx.send(WorkerMessage::Maintenance(result));
        });
        self.open(Screen::MaintenanceReview);
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
            let Some((request, plan)) = self.plan.clone() else { return };
            if !plan.executable() {
                self.error =
                    Some("This plan is blocked or incomplete and cannot be confirmed.".into());
                return;
            }
            self.progress_stage = orbis_core::progress::ExecutionStage::Planning;
            self.progress_title = format!("{} {}", plan.action.label(), plan.target.name);
            self.progress_output.clear();
            self.progress_scroll = 0;
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
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
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
        match self.screen {
            Screen::Dashboard => self.draw_dashboard(frame, area),
            Screen::Search => self.draw_search(frame, area),
            Screen::Package => self.draw_package(frame, area),
            Screen::Updates => self.draw_updates(frame, area),
            Screen::Sources => self.draw_sources(frame, area),
            Screen::History => self.draw_history(frame, area),
            Screen::HistoryDetail => self.draw_history_detail(frame, area),
            Screen::Why => self.draw_why(frame, area),
            Screen::Help => {
                self.draw_dashboard(frame, area);
                self.draw_help(frame, area);
            }
            Screen::Confirm => {
                self.draw_package(frame, area);
                self.draw_confirm(frame, area);
            }
            Screen::MaintenanceReview => {
                self.draw_updates(frame, area);
                self.draw_maintenance(frame, area);
            }
            Screen::Progress => self.draw_progress(frame, area),
            Screen::Result => {
                self.draw_package(frame, area);
                self.draw_result(frame, area);
            }
        }
    }

    fn shell(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        subtitle: &str,
        body: Vec<Line<'static>>,
    ) {
        self.shell_scrolled(frame, area, title, subtitle, body, 0);
    }

    fn shell_scrolled(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        title: &str,
        subtitle: &str,
        body: Vec<Line<'static>>,
        scroll: u16,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(1), Constraint::Length(2)])
            .split(area);
        let header = Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                self.theme.brand_compact(),
                self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  {title} · {subtitle}"),
                self.theme.style(Token::Muted),
            )),
        ]));
        frame.render_widget(header, chunks[0]);
        frame.render_widget(
            Paragraph::new(Text::from(body)).scroll((scroll, 0)).wrap(Wrap { trim: false }),
            chunks[1],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "/ Search   U Updates   S Sources   H History   ? Help   Q Quit",
                self.theme.style(Token::Muted),
            ))),
            chunks[2],
        );
    }

    fn draw_dashboard(&self, frame: &mut Frame<'_>, area: Rect) {
        let brand_lines = self.theme.brand_full();
        let header_height = (brand_lines.len() as u16) + 2;
        let chunks = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
        let mut header_lines: Vec<Line<'static>> = brand_lines
            .iter()
            .map(|line| Line::from(Span::styled(*line, self.theme.style(Token::Primary))))
            .collect();
        header_lines.push(Line::from(Span::styled(
            format!("  {}", Theme::brand_tagline()),
            self.theme.style(Token::Muted),
        )));
        let header = Paragraph::new(Text::from(header_lines));
        frame.render_widget(header, chunks[0]);
        match layout_class(area.width) {
            LayoutClass::Compact => {
                let mut lines = self.provider_lines(true);
                lines.push(Line::from(""));
                lines.extend(self.provider_lines(false));
                frame.render_widget(
                    Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                    chunks[1],
                );
            }
            LayoutClass::Normal | LayoutClass::Wide => {
                let columns = if matches!(layout_class(area.width), LayoutClass::Wide) {
                    Layout::horizontal([Constraint::Percentage(44), Constraint::Percentage(56)])
                        .split(chunks[1])
                } else {
                    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(chunks[1])
                };
                frame.render_widget(
                    Paragraph::new(Text::from(self.provider_lines(true)))
                        .wrap(Wrap { trim: false }),
                    columns[0],
                );
                frame.render_widget(
                    Paragraph::new(Text::from(self.provider_lines(false)))
                        .wrap(Wrap { trim: false }),
                    columns[1],
                );
            }
        }
        let update_line = match &self.updates {
            None if self.snapshot_loading => "Updates   checking…".to_owned(),
            None => "Updates   waiting for provider state".to_owned(),
            Some(report) if report.candidates.is_empty() => "Updates   none confirmed".to_owned(),
            Some(report) => {
                format!("Updates   {} candidate(s) across available providers", report.total())
            }
        };
        let footer = Text::from(vec![
            Line::from(Span::styled(update_line, self.theme.style(Token::Secondary))),
            Line::from(Span::styled(
                "  r refreshes local/provider state · no catalog mutation",
                self.theme.style(Token::Muted),
            )),
        ]);
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }

    fn provider_lines(&self, system: bool) -> Vec<Line<'static>> {
        let title = if system { "SYSTEM & DESKTOP" } else { "DEVELOPER TOOLS" };
        let mut lines = vec![Line::from(Span::styled(title, self.theme.style(Token::Section)))];
        let sources = self.sources.as_deref().unwrap_or(&[]);
        for source in sources.iter().filter(|source| {
            matches!(
                source.source,
                PackageSource::Apt | PackageSource::Flatpak | PackageSource::Snap
            ) == system
        }) {
            let token = match source.state.as_str() {
                "ready" => Token::Positive,
                "unavailable" => Token::Unavailable,
                _ => Token::Caution,
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", self.theme.mark(token)), self.theme.style(token)),
                Span::styled(
                    format!("{:<10}", source.source.label()),
                    self.theme.style(Token::Provider),
                ),
                Span::styled(source.state.clone(), self.theme.style(token)),
            ]));
        }
        if sources.is_empty() {
            lines.push(Line::from(Span::styled(
                "  checking provider state…",
                self.theme.style(Token::Muted),
            )));
        }
        lines
    }

    fn draw_search(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = vec![
            Line::from(vec![
                Span::styled("SEARCH  ", self.theme.style(Token::Section)),
                Span::styled(
                    format!("{}▌", self.search_query),
                    self.theme.style(Token::Foreground),
                ),
            ]),
            Line::from(""),
        ];
        if self.search_loading {
            body.push(Line::from(Span::styled(
                "checking providers…",
                self.theme.style(Token::Muted),
            )));
        } else if self.search_results.is_empty() && !self.search_query.is_empty() {
            body.push(Line::from(
                "No matches. Try a broader query or a provider-qualified CLI command.",
            ));
        }
        for (index, package) in self.search_results.iter().enumerate() {
            let selected = index == self.selected;
            let status = match package.installed {
                Some(true) => "installed",
                Some(false) => "available",
                None => "state unknown",
            };
            let name = truncate(&package.name, 27, self.theme.unicode);
            body.push(Line::from(vec![
                Span::styled(
                    format!("{} {name:<28}", if selected { ">" } else { " " }),
                    if selected {
                        self.theme.style(Token::Selected)
                    } else {
                        self.theme.style(Token::Foreground)
                    },
                ),
                Span::styled(
                    format!(" {:<8} {status}", package.source.label()),
                    self.theme.style(Token::Provider),
                ),
            ]));
            if let Some(summary) = &package.summary {
                body.push(Line::from(Span::styled(
                    format!("    {summary}"),
                    self.theme.style(Token::Muted),
                )));
            }
        }
        self.shell(frame, area, "Search", "type to search · Enter opens details", body);
    }

    fn draw_package(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(package) = &self.selected_package else {
            self.shell(
                frame,
                area,
                "Package",
                "nothing selected",
                vec![Line::from("Select a package from Search.")],
            );
            return;
        };
        let brief = build_brief(package.clone());
        let mut body = vec![
            Line::from(Span::styled(package.name.clone(), self.theme.style(Token::Primary))),
            Line::from(format!(
                "{} · {} · {}",
                package.source,
                package.version.as_deref().unwrap_or("version unknown"),
                package.installed.map_or("state unknown", |value| if value {
                    "installed"
                } else {
                    "available"
                })
            )),
            Line::from(""),
            Line::from(Span::styled("What it does", self.theme.style(Token::Section))),
        ];
        body.extend(brief.paragraphs.into_iter().map(Line::from));
        if let Some(caution) = brief.caution {
            body.push(Line::from(Span::styled(
                format!("Context  {caution}"),
                self.theme.style(Token::Caution),
            )));
        }
        body.extend([
            Line::from(""),
            Line::from("I Install / Update    R Remove    W Why    Esc Back"),
        ]);
        self.shell(frame, area, "Orbis Brief", "provider-aware package detail", body);
    }

    fn draw_updates(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = vec![Line::from(Span::styled("UPDATES", self.theme.style(Token::Section)))];
        match &self.updates {
            None => body.push(Line::from(Span::styled(
                "checking provider inventories…",
                self.theme.style(Token::Muted),
            ))),
            Some(report) if report.candidates.is_empty() => {
                body.push(Line::from("No confirmed updates were found."))
            }
            Some(report) => {
                body.push(Line::from(format!(
                    "{} candidate(s) across provider sources",
                    report.total()
                )));
                let mut previous_source = None;
                for candidate in &report.candidates {
                    if previous_source != Some(candidate.source) {
                        body.push(Line::from(Span::styled(
                            candidate.source.label(),
                            self.theme.style(Token::Provider),
                        )));
                        previous_source = Some(candidate.source);
                    }
                    body.push(Line::from(format!(
                        "  {:<26} {} → {}  {}",
                        truncate(&candidate.name, 26, self.theme.unicode),
                        candidate.current_version.as_deref().unwrap_or("current"),
                        candidate.available_version.as_deref().unwrap_or("latest"),
                        candidate.source
                    )));
                }
            }
        }
        if let Some(report) = &self.updates {
            for inventory in &report.inventories {
                if inventory
                    .metadata_state
                    .as_deref()
                    .is_some_and(|state| state.contains("incomplete") || state.contains("unknown"))
                {
                    body.push(Line::from(Span::styled(
                        format!(
                            "  {} update status incomplete; unknown is not zero",
                            inventory.source
                        ),
                        self.theme.style(Token::Caution),
                    )));
                }
            }
        }
        body.extend([
            Line::from(""),
            Line::from("U Review upgrade plan    r Refresh view    Esc Back"),
        ]);
        self.shell(frame, area, "Updates", "unified read-only inventory", body);
    }

    fn draw_sources(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body =
            vec![Line::from(Span::styled("PROVIDERS", self.theme.style(Token::Section)))];
        if let Some(sources) = &self.sources {
            for source in sources {
                body.push(Line::from(vec![
                    Span::styled(
                        format!("  {:<10}", source.source.label()),
                        self.theme.style(Token::Provider),
                    ),
                    Span::styled(
                        format!("{:<13}", source.state),
                        state_style(self.theme, &source.state),
                    ),
                    Span::styled(
                        source.backend.clone().unwrap_or_default(),
                        self.theme.style(Token::Muted),
                    ),
                ]));
                body.push(Line::from(Span::styled(
                    format!("      Supports  {}", capability_summary(source)),
                    self.theme.style(Token::Muted),
                )));
                if let Some(note) = source.notes.first() {
                    body.push(Line::from(Span::styled(
                        format!("      {note}"),
                        self.theme.style(Token::Muted),
                    )));
                }
            }
        } else {
            body.push(Line::from("  checking provider state…"));
        }
        self.shell(frame, area, "Sources", "capabilities and limitations", body);
    }

    fn draw_history(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = vec![Line::from(Span::styled("TIMELINE", self.theme.style(Token::Section)))];
        if self.history.is_empty() {
            body.push(Line::from("No Orbis operations recorded yet."));
        }
        for (index, entry) in self.history.iter().enumerate() {
            let selected = index == self.history_selected;
            let line = format!(
                "{} {:<16} {:<10} {}{}",
                if selected { ">" } else { " " },
                entry.operation_id,
                entry.status,
                entry.source.map_or("Orbis", |source| source.label()),
                entry.package.as_deref().map_or(String::new(), |package| format!(" · {package}")),
            );
            body.push(Line::from(Span::styled(
                line,
                if selected {
                    self.theme.style(Token::Selected)
                } else {
                    self.theme.style(Token::Foreground)
                },
            )));
        }
        body.extend([Line::from(""), Line::from("Enter Details    ↑/↓ or j/k Move    Esc Back")]);
        self.shell_scrolled(
            frame,
            area,
            "History",
            "sanitized local records",
            body,
            self.history_selected.saturating_sub(8) as u16,
        );
    }

    fn draw_history_detail(&self, frame: &mut Frame<'_>, area: Rect) {
        let value = self.history_detail.as_ref().map_or_else(
            || "No history record selected.".to_owned(),
            |value| serde_json::to_string_pretty(value).unwrap_or_default(),
        );
        self.shell(
            frame,
            area,
            "History detail",
            "sanitized record",
            value.lines().map(str::to_owned).map(Line::from).collect(),
        );
    }

    fn draw_why(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut body = Vec::new();
        if self.why_loading {
            body.push(Line::from(Span::styled(
                "checking provider explanation…",
                self.theme.style(Token::Muted),
            )));
        } else if let Some(report) = &self.why {
            body.extend([
                Line::from(Span::styled(
                    report.package.name.clone(),
                    self.theme.style(Token::Primary),
                )),
                Line::from(""),
                Line::from("Why is this installed?"),
                Line::from(report.installed_as.clone()),
            ]);
            if !report.used_by.is_empty() {
                body.extend([
                    Line::from(""),
                    Line::from(Span::styled("Used by", self.theme.style(Token::Section))),
                ]);
                body.extend(report.used_by.iter().map(|consumer| {
                    Line::from(format!("  {} · {}", consumer.name, consumer.relationship))
                }));
            }
            body.extend([
                Line::from(""),
                Line::from(Span::styled("Removal context", self.theme.style(Token::Section))),
                Line::from(report.removal_advice.clone()),
            ]);
        } else {
            body.push(Line::from("No explanation is available."));
        }
        self.shell(frame, area, "Why", "evidence-aware explanation", body);
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
            Line::from("r           refresh read-only state"),
            Line::from("u           review upgrade plan"),
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
        let mut lines = vec![Line::from(Span::styled(
            "Review before changing package state",
            self.theme.style(Token::Primary),
        ))];
        if self.loading_plan {
            lines.push(Line::from("Building provider plan…"));
        } else if let Some((_, plan)) = &self.plan {
            lines.extend([
                Line::from(format!("{} {}", plan.action.label(), plan.target.name)),
                Line::from(format!("Source       {}", plan.target.source)),
                Line::from(format!("Scope        {}", plan.scope.label())),
                Line::from(format!(
                    "Plan         {} · {}",
                    completeness_label(plan.completeness),
                    confidence_label(plan.confidence)
                )),
                Line::from(format!(
                    "Privilege    {}",
                    if plan.privilege == orbis_core::transaction::PrivilegeRequirement::None {
                        "none"
                    } else {
                        "administrator"
                    }
                )),
            ]);
            lines.extend(plan.warnings.iter().map(|warning| {
                Line::from(Span::styled(
                    format!("! {}", warning.message),
                    self.theme.style(Token::Caution),
                ))
            }));
            lines.push(Line::from(""));
            lines.push(Line::from(if plan.executable() {
                "Enter / Y Confirm     Esc Cancel"
            } else {
                "Blocked plan          Esc Back"
            }));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(self.theme.style(Token::Primary))
                        .style(self.theme.style(Token::Surface)),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_maintenance(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered(area, 76, 18);
        frame.render_widget(Clear, popup);
        let mut lines =
            vec![Line::from(Span::styled("Upgrade plan review", self.theme.style(Token::Primary)))];
        if let Some(plan) = &self.maintenance {
            lines.push(Line::from(format!(
                "{} provider plan(s) · risk {}",
                plan.providers.len(),
                plan.risk.label()
            )));
            lines.extend(plan.providers.iter().map(|provider| {
                Line::from(format!(
                    "  {:<9} {} candidate(s) · {}",
                    provider.source.label(),
                    provider.candidates.len(),
                    if provider.executable() { "executable" } else { "not executable" }
                ))
            }));
            lines.extend([Line::from(""), Line::from("This review is read-only. Esc closes it.")]);
        } else {
            lines.push(Line::from("Building coordinated plan…"));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(self.theme.style(Token::Primary))
                        .style(self.theme.style(Token::Surface)),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn draw_progress(&self, frame: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::vertical([
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(area);

        let header = Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                self.theme.brand_compact(),
                self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  {}", self.progress_title),
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD),
            )),
        ]));
        frame.render_widget(header, chunks[0]);

        let stages = orbis_core::progress::ExecutionStage::transaction_stages();
        let mut stage_lines =
            vec![Line::from(Span::styled("STAGES", self.theme.style(Token::Section)))];
        let current = self.progress_stage;
        let mut found_current = false;
        for &stage in stages {
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
            let style = if is_current {
                self.theme.style(Token::Foreground).add_modifier(Modifier::BOLD)
            } else if is_past {
                self.theme.style(Token::Foreground)
            } else {
                self.theme.style(Token::Muted)
            };

            stage_lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", self.theme.stage_mark(state)),
                    self.theme.style(token),
                ),
                Span::styled(
                    if is_current && !stage.is_terminal() {
                        format!("{} …", stage.label())
                    } else {
                        stage.label().to_string()
                    },
                    style,
                ),
            ]));
        }
        frame.render_widget(Paragraph::new(Text::from(stage_lines)), chunks[1]);

        let output_lines: Vec<Line<'static>> = self
            .progress_output
            .iter()
            .map(|line| Line::from(Span::styled(line.clone(), self.theme.style(Token::Muted))))
            .collect();

        let visible_height = chunks[2].height.saturating_sub(2) as usize;
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
            .title(Span::styled(" PROVIDER OUTPUT ", self.theme.style(Token::Section)));

        let output_paragraph =
            Paragraph::new(Text::from(output_lines)).block(output_block).scroll((scroll_offset, 0));
        frame.render_widget(output_paragraph, chunks[2]);

        let footer = Line::from(Span::styled(
            "  ↑/↓ or j/k scroll log output",
            self.theme.style(Token::Muted),
        ));
        frame.render_widget(Paragraph::new(footer), chunks[3]);
    }

    fn draw_result(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered(area, 72, 18);
        frame.render_widget(Clear, popup);

        let mut lines = Vec::new();

        if let Some(result) = &self.transaction_result {
            let (status_text, token) = match result.status {
                orbis_core::transaction::TransactionStatus::Succeeded => {
                    ("Completed", Token::Positive)
                }
                orbis_core::transaction::TransactionStatus::PartiallyVerified => {
                    ("Completed with limited verification", Token::Caution)
                }
                orbis_core::transaction::TransactionStatus::Failed => {
                    ("Failed", Token::Destructive)
                }
            };

            lines.push(Line::from(Span::styled(
                format!("{} {}", result.plan.action.label(), result.plan.target.name),
                self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Status          ", self.theme.style(Token::Muted)),
                Span::styled(status_text, self.theme.style(token).add_modifier(Modifier::BOLD)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Source          ", self.theme.style(Token::Muted)),
                Span::styled(result.plan.target.source.label(), self.theme.style(Token::Provider)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Verification    ", self.theme.style(Token::Muted)),
                Span::styled(
                    match result.verification {
                        orbis_core::transaction::VerificationResult::Verified => "verified",
                        orbis_core::transaction::VerificationResult::PartiallyVerified => {
                            "partially verified"
                        }
                        orbis_core::transaction::VerificationResult::Failed => "failed",
                    },
                    self.theme.style(Token::Foreground),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Transaction ID  ", self.theme.style(Token::Muted)),
                Span::styled(&result.plan.operation_id, self.theme.style(Token::Foreground)),
            ]));
            if let Some(msg) = &result.execution.message {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("  Detail          ", self.theme.style(Token::Muted)),
                    Span::styled(msg.clone(), self.theme.style(Token::Caution)),
                ]));
            }
        } else if let Some(error) = &self.error {
            lines.push(Line::from(Span::styled(
                "Operation Failed",
                self.theme.style(Token::Destructive).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Error: ", self.theme.style(Token::Destructive)),
                Span::styled(error.clone(), self.theme.style(Token::Foreground)),
            ]));
        } else if let Some(result) = &self.maintenance_result {
            lines.push(Line::from(Span::styled(
                format!("Maintenance {}", result.action.label()),
                self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Status          ", self.theme.style(Token::Muted)),
                Span::styled(format!("{:?}", result.status), self.theme.style(Token::Positive)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Operation ID    ", self.theme.style(Token::Muted)),
                Span::styled(&result.operation_id, self.theme.style(Token::Foreground)),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Enter / Esc return to package",
            self.theme.style(Token::Muted),
        )));

        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(self.theme.style(Token::Primary))
                        .style(self.theme.style(Token::Surface)),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }
}

fn state_style(theme: Theme, state: &str) -> ratatui::style::Style {
    theme.style(match state {
        "ready" => Token::Positive,
        "unavailable" => Token::Unavailable,
        _ => Token::Caution,
    })
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

fn completeness_label(value: orbis_core::transaction::PlanCompleteness) -> &'static str {
    match value {
        orbis_core::transaction::PlanCompleteness::Complete => "complete",
        orbis_core::transaction::PlanCompleteness::Partial => "partial",
        orbis_core::transaction::PlanCompleteness::Unknown => "unknown",
    }
}

fn confidence_label(value: orbis_core::transaction::PlanConfidence) -> &'static str {
    match value {
        orbis_core::transaction::PlanConfidence::High => "high",
        orbis_core::transaction::PlanConfidence::Medium => "medium",
        orbis_core::transaction::PlanConfidence::Low => "low",
    }
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

    #[test]
    fn dashboard_renders_at_reference_sizes() {
        for (width, height) in [(80, 24), (100, 30), (140, 40)] {
            let content = render_at(width, height);
            assert!(content.contains("ORBIS"));
            assert!(content.contains("SYSTEM & DESKTOP"));
            assert!(content.contains("DEVELOPER TOOLS"));
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
        assert!(content.contains("STAGES"));
        assert!(content.contains("Executing"));
        assert!(content.contains("PROVIDER OUTPUT"));
        assert!(content.contains("Reading package lists..."));
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
        assert!(content.contains("Install btop"));
        assert!(content.contains("Completed"));
        assert!(content.contains("verified"));
        assert!(content.contains("tx-test-123"));
    }
}
