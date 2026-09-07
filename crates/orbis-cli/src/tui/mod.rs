//! Interactive terminal presentation for Orbis.
//!
//! The application owns only presentation state. Provider reads are performed by
//! short-lived read-only workers and mutations cross the same plan/executor
//! boundary as the ordinary CLI.

use std::{
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
    render::theme::{Theme, Token},
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
    registry: &'a ProviderRegistry,
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
}

impl<'a> App<'a> {
    fn new(
        registry: &'a ProviderRegistry,
        theme: Theme,
        tx: Sender<WorkerMessage>,
        rx: Receiver<WorkerMessage>,
    ) -> Self {
        Self {
            registry,
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
            match commands::execute_confirmed_transaction(self.registry, &request, plan) {
                Ok(result) => {
                    self.error =
                        Some(format!("Transaction completed · {}", result.plan.operation_id));
                    self.screen = Screen::Package;
                    self.refresh();
                }
                Err(error) => self.error = Some(error),
            }
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
            Line::from(vec![
                Span::styled(
                    format!("{} ", self.theme.mark(Token::Primary)),
                    self.theme.style(Token::Primary),
                ),
                Span::styled(
                    "ORBIS",
                    self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
                ),
            ]),
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
        let chunks =
            Layout::vertical([Constraint::Length(4), Constraint::Min(1), Constraint::Length(3)])
                .split(area);
        let header = Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled(
                    format!("{} ", self.theme.mark(Token::Primary)),
                    self.theme.style(Token::Primary),
                ),
                Span::styled(
                    "ORBIS",
                    self.theme.style(Token::Primary).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "  Your Linux software, in one place.",
                self.theme.style(Token::Muted),
            )),
        ]));
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
}
