use std::collections::HashMap;

use crate::{
    compiler::{ColumnRef, ConstraintSet},
    pretty::Pretty,
    structs::Handle,
};
use anyhow::{bail, Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use itertools::Itertools;
use ratatui::{prelude::*, widgets::*};
use regex_lite::Regex;

type Backend = CrosstermBackend<std::io::Stdout>;
type Frame<'a> = ratatui::Frame<'a, Backend>;
type StdTerminal = Terminal<Backend>;

const CONTEXT: isize = 50;

mod forth;
mod widgets;

struct ModuleView {
    /// The name of the associated module
    name: String,
    /// A cache of this module columns
    columns: Vec<(ColumnRef, Handle)>,
    /// Current offset in the table view
    shift: isize,
    /// Size of the longest column in the module
    size: isize,

    /// A list of regexps; only columns matching at least one of them will be displayed
    regexps: Vec<Regex>,
    /// The current indices of the subset of columns to show from `columns`
    to_show: Vec<usize>,

    /// If any, the latest used Forth/scan expression
    last_scan: String,
}
impl ModuleView {
    fn from_cs(cs: &ConstraintSet, name: &str) -> ModuleView {
        let mut max_size = 0;
        let columns: Vec<(ColumnRef, Handle)> = cs
            .columns
            .iter_module(name)
            .sorted_by(|c, d| c.1.handle.name.cmp(&d.1.handle.name))
            .map(|c| {
                max_size = max_size.max(cs.columns.len(&c.0).unwrap_or_default());
                (c.0, c.1.handle.clone())
            })
            .collect();
        let currently_shown = (0..columns.len()).collect();
        ModuleView {
            name: name.to_owned(),
            columns,
            shift: 0,
            size: max_size as isize - 1,

            regexps: Vec::new(),
            to_show: currently_shown,

            last_scan: String::new(),
        }
    }

    fn goto(&mut self, i: isize) {
        self.shift = i.clamp(0, self.size);
    }

    fn left(&mut self, x: isize) {
        self.shift -= self.shift.min(x);
    }

    fn right(&mut self, x: isize) {
        self.shift = (self.shift + x).min(self.size);
    }

    fn home(&mut self) {
        self.shift = 0;
    }

    fn end(&mut self) {
        self.shift = self.size;
    }

    fn current_columns(&self) -> impl Iterator<Item = &(ColumnRef, Handle)> {
        self.to_show.iter().map(|i| &self.columns[*i])
    }

    fn filter(&mut self, regexps: Vec<Regex>) {
        self.regexps = regexps;
        self.to_show = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(i, (_, handle))| {
                if self.regexps.is_empty()
                    || self
                        .regexps
                        .iter()
                        .any(|regex| regex.is_match(&handle.name))
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();
    }

    fn clear_filter(&mut self) {
        self.filter(Vec::new());
    }

    fn render(&self, cs: &ConstraintSet, f: &mut Frame, target: Rect) {
        let span = 0.max(self.shift)..(self.shift + CONTEXT).min(self.size) + 1;
        // max width for each column; defaults to 3
        let mut maxes = vec![3; span.len() + 1];

        let block = Block::new().borders(Borders::NONE);

        let table = Table::new(self.current_columns().map(|(column_ref, h)| {
            maxes[0] = maxes[0].max(h.name.len());
            Row::new(
                std::iter::once(
                    Cell::from(h.name.to_owned()).style(Style::default().blue().bold()),
                )
                .chain(span.clone().enumerate().map(|(k, i)| {
                    let is = cs
                        .columns
                        .get(column_ref, i, false)
                        .map(|x| x.pretty_with_base(cs.columns.get_col(column_ref).unwrap().base))
                        .unwrap_or(".".to_string());
                    maxes[k + 1] = maxes[k + 1].max(is.len());
                    Cell::from(is)
                })),
            )
            .style(Style::default().white())
        }));

        let widths = maxes
            .iter()
            .map(|w| Constraint::Length(*w as u16))
            .collect::<Vec<_>>();
        let table = table
            .header(
                Row::new(std::iter::once(String::new()).chain(span.map(|i| i.to_string())))
                    .style(Style::default().bold().blue()),
            )
            .widths(&widths)
            .block(block);
        f.render_widget(table, target);
    }
}

struct Inspector<'a> {
    cs: &'a ConstraintSet,
    modules: Vec<ModuleView>,
    current_module: usize,
    minibuffer: Rect,
    message: Span<'a>,
}
impl<'a> Inspector<'a> {
    fn from_cs(cs: &'a ConstraintSet) -> Result<Self> {
        let r = Inspector {
            cs,
            modules: cs
                .columns
                .modules()
                .iter()
                .sorted()
                .map(|n| ModuleView::from_cs(cs, n))
                .collect(),
            current_module: 0,
            minibuffer: Default::default(),
            message: Span::from(""),
        };
        if r.modules.is_empty() {
            bail!("no modules found in provided constraint system");
        } else {
            Ok(r)
        }
    }

    fn current_module(&self) -> &ModuleView {
        self.modules.get(self.current_module).unwrap()
    }

    fn current_module_mut(&mut self) -> &mut ModuleView {
        self.modules.get_mut(self.current_module).unwrap()
    }

    fn render_tabs(&self, f: &mut Frame, place: Rect) {
        let titles = self.modules.iter().map(|t| t.name.clone()).collect();
        let tabs = Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .title(Line::from(vec![
                        "← ".into(),
                        "[S-TAB]".yellow().bold(),
                        " Modules ".into(),
                        "[TAB]".yellow().bold(),
                        " →".into(),
                    ])),
            )
            .select(self.current_module)
            // .style(Style::default().dark_gray())
            .highlight_style(
                Style::default()
                    .white()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED),
            );
        f.render_widget(tabs, place);
    }

    fn render_columns(&self, f: &mut Frame, target: Rect) {
        self.current_module().render(self.cs, f, target);
    }

    fn render_help(&self, f: &mut Frame) {
        let titles = vec![
            "[g]".yellow().bold(),
            "oto".into(),
            " :: ".dark_gray(),
            "[f]".yellow().bold(),
            "ilter".into(),
            " :: ".dark_gray(),
            "clear ".into(),
            "[F]".yellow().bold(),
            "ilter".into(),
            " :: ".dark_gray(),
            "[s]".yellow().bold(),
            "can".into(),
            " :: ".dark_gray(),
            // "[p]".yellow().bold(),
            // "lookup".into(),
            // " :: ".into(),
            "[q]".red().bold(),
            "uit".into(),
        ];
        f.render_widget(
            Paragraph::new(vec![Line::from(titles), Line::from(self.message.clone())])
                .block(Block::default().title("Commands").borders(Borders::TOP)),
            self.minibuffer,
        );
    }

    fn render(&mut self, f: &mut Frame) {
        let size = f.size();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(size);
        self.minibuffer = chunks[2];

        let block = Block::default();
        f.render_widget(block, size);
        self.render_tabs(f, chunks[0]);
        self.render_columns(f, chunks[1]);
        self.render_help(f);
    }

    fn prev(&mut self) {
        self.current_module = if self.current_module == 0 {
            self.modules.len() - 1
        } else {
            self.current_module - 1
        }
    }

    fn next(&mut self) {
        self.current_module = (self.current_module + 1) % self.modules.len();
    }

    fn run(&mut self, terminal: &mut StdTerminal) -> Result<()> {
        loop {
            terminal.draw(|term| self.render(term))?;
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('s') => {
                            let mut t = Terminal::with_options(
                                CrosstermBackend::new(std::io::stdout()),
                                TerminalOptions {
                                    viewport: Viewport::Fixed(self.minibuffer),
                                },
                            )
                            .unwrap();
                            let column_cache = self
                                .current_module()
                                .columns
                                .iter()
                                .map(|(r, h)| (h.name.clone(), r.clone()))
                                .collect::<HashMap<_, _>>();
                            let is = widgets::scan::ScanInput::new(
                                &self.current_module().name,
                                &self.current_module().last_scan,
                                &column_cache,
                            )
                            .run(
                                &mut t,
                                &|i, r| self.cs.columns.get_raw(r, i, false).copied(),
                                self.current_module().size,
                                self.minibuffer,
                            );
                            if let Some((exp, is)) = is {
                                self.current_module_mut().last_scan = exp.clone();
                                if is.is_empty() {
                                    self.message = "Not found".red();
                                } else {
                                    self.message = Span::from(format!(
                                        "'{}' found at {}",
                                        exp,
                                        is.iter().join(" ")
                                    ))
                                    .green();
                                    self.current_module_mut().goto(is[0]);
                                }
                            }
                            let _ = terminal.clear();
                        }
                        KeyCode::Char('g') => {
                            let mut t = Terminal::with_options(
                                CrosstermBackend::new(std::io::stdout()),
                                TerminalOptions {
                                    viewport: Viewport::Fixed(self.minibuffer),
                                },
                            )
                            .unwrap();
                            let i = widgets::number::NumberInput::new("Go to column...")
                                .run(&mut t, self.minibuffer);
                            if let Some(i) = i {
                                self.current_module_mut().goto(i);
                            }
                            let _ = terminal.clear();
                        }
                        KeyCode::Char('f') => {
                            let mut t = Terminal::with_options(
                                CrosstermBackend::new(std::io::stdout()),
                                TerminalOptions {
                                    viewport: Viewport::Fixed(self.minibuffer),
                                },
                            )
                            .unwrap();
                            let regexs = widgets::regexp::RegexpInput::new(
                                "Filter columns matching",
                                self.current_module()
                                    .regexps
                                    .iter()
                                    .map(|regexp| regexp.to_string())
                                    .join(" "),
                            )
                            .run(&mut t, self.minibuffer);
                            if let Some(regexs) = regexs {
                                self.current_module_mut().filter(regexs);
                            }
                            let _ = terminal.clear();
                        }
                        KeyCode::Char('F') => self.current_module_mut().clear_filter(),
                        KeyCode::BackTab => {
                            self.prev();
                        }
                        KeyCode::Tab => {
                            if key.modifiers == KeyModifiers::SHIFT {
                                self.prev();
                            } else {
                                self.next();
                            }
                        }
                        KeyCode::Left => {
                            self.current_module_mut().left(1);
                        }
                        KeyCode::Right => {
                            self.current_module_mut().right(1);
                        }
                        KeyCode::PageUp => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                self.current_module_mut().left(1000);
                            } else {
                                self.current_module_mut().left(100);
                            }
                        }
                        KeyCode::PageDown => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                self.current_module_mut().right(1000);
                            } else {
                                self.current_module_mut().right(100);
                            }
                        }
                        KeyCode::Home => {
                            self.current_module_mut().home();
                        }
                        KeyCode::End => {
                            self.current_module_mut().end();
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

pub(crate) fn inspect(cs: &ConstraintSet) -> Result<()> {
    let mut inspector = Inspector::from_cs(cs)?;
    let mut terminal = setup_terminal()?;
    inspector.run(&mut terminal)?;
    restore_terminal(&mut terminal)?;
    Ok(())
}

fn setup_terminal() -> Result<StdTerminal> {
    let mut stdout = std::io::stdout();
    enable_raw_mode().context("failed to enable raw mode")?;
    execute!(stdout, EnterAlternateScreen).context("unable to enter alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("creating terminal failed")
}

fn restore_terminal(terminal: &mut StdTerminal) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("unable to switch to main screen")?;
    terminal.show_cursor().context("unable to show cursor")
}
