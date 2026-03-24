use anyhow::{anyhow, Result};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::LazyLock;

static MATCHER: LazyLock<SkimMatcherV2> = LazyLock::new(|| SkimMatcherV2::default().ignore_case());

const PICKER_PROMPT: &str = "Select repositories for the workspace";
const HELP_MESSAGE: &str = "↑↓ move, space toggle, enter submit, esc cancel, type to filter";
const EMPTY_FILTER_MESSAGE: &str = "No repositories match the current filter.";
const EMPTY_SELECTION_MESSAGE: &str = "Select at least one repository.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoPromptOption {
    pub(crate) label: String,
    pub(crate) repo_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerOutcome {
    Continue,
    Submit(Vec<PathBuf>),
    Cancel,
}

#[derive(Debug, Clone)]
pub(crate) struct RepoPickerState {
    options: Vec<RepoPromptOption>,
    query: String,
    filtered_indices: Vec<usize>,
    checked_indices: BTreeSet<usize>,
    highlighted_index: usize,
    validation_message: Option<&'static str>,
}

impl RepoPickerState {
    pub(crate) fn new(options: Vec<RepoPromptOption>) -> Self {
        let mut state = Self {
            filtered_indices: (0..options.len()).collect(),
            options,
            query: String::new(),
            checked_indices: BTreeSet::new(),
            highlighted_index: 0,
            validation_message: None,
        };
        state.refresh_filter();
        state
    }

    #[cfg(test)]
    pub(crate) fn filtered_indices(&self) -> &[usize] {
        &self.filtered_indices
    }

    #[cfg(test)]
    pub(crate) fn checked_indices(&self) -> &BTreeSet<usize> {
        &self.checked_indices
    }

    #[cfg(test)]
    pub(crate) fn highlighted_repo_root(&self) -> Option<&PathBuf> {
        let option_index = *self.filtered_indices.get(self.highlighted_index)?;
        Some(&self.options[option_index].repo_root)
    }

    pub(crate) fn status_message(&self) -> Option<&'static str> {
        if let Some(message) = self.validation_message {
            Some(message)
        } else if self.filtered_indices.is_empty() {
            Some(EMPTY_FILTER_MESSAGE)
        } else {
            None
        }
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> PickerOutcome {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return PickerOutcome::Continue;
        }

        match key_event.code {
            KeyCode::Esc => PickerOutcome::Cancel,
            KeyCode::Enter => self.submit(),
            KeyCode::Up => {
                self.clear_validation();
                self.move_highlight_up();
                PickerOutcome::Continue
            }
            KeyCode::Down => {
                self.clear_validation();
                self.move_highlight_down();
                PickerOutcome::Continue
            }
            KeyCode::Backspace => {
                self.clear_validation();
                if self.query.pop().is_some() {
                    self.refresh_filter();
                }
                PickerOutcome::Continue
            }
            KeyCode::Char(' ') if key_event.modifiers.is_empty() => {
                self.clear_validation();
                self.toggle_highlighted();
                PickerOutcome::Continue
            }
            KeyCode::Char(character)
                if key_event.modifiers.is_empty() || key_event.modifiers == KeyModifiers::SHIFT =>
            {
                self.clear_validation();
                self.query.push(character);
                self.refresh_filter();
                PickerOutcome::Continue
            }
            _ => PickerOutcome::Continue,
        }
    }

    pub(crate) fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let layout = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let header = Paragraph::new(Line::from(PICKER_PROMPT.bold()));
        frame.render_widget(header, layout[0]);

        let filter = Paragraph::new(self.query.as_str()).block(
            Block::default()
                .title("Filter")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        );
        frame.render_widget(filter, layout[1]);
        self.render_cursor(frame, layout[1]);

        self.render_list(frame, layout[2]);

        let status = self.status_message().unwrap_or("");
        let status_style = if self.validation_message.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        frame.render_widget(Paragraph::new(status).style(status_style), layout[3]);

        let help = Paragraph::new(HELP_MESSAGE).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(help, layout[4]);
    }

    fn render_cursor(&self, frame: &mut Frame, area: Rect) {
        let inner = area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let cursor_x = inner
            .x
            .saturating_add(self.query.chars().count() as u16)
            .min(inner.x.saturating_add(inner.width.saturating_sub(1)));
        frame.set_cursor_position((cursor_x, inner.y));
    }

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        if self.filtered_indices.is_empty() {
            let empty_state = Paragraph::new(EMPTY_FILTER_MESSAGE)
                .block(Block::default().title("Repositories").borders(Borders::ALL))
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty_state, area);
            return;
        }

        let visible_height = area.height.saturating_sub(2) as usize;
        let offset = self.visible_offset(visible_height);
        let visible = &self.filtered_indices
            [offset..self.filtered_indices.len().min(offset + visible_height)];

        let items = visible
            .iter()
            .map(|index| {
                let option = &self.options[*index];
                let checkbox = if self.checked_indices.contains(index) {
                    "[x]"
                } else {
                    "[ ]"
                };
                ListItem::new(format!("{checkbox} {}", option.label))
            })
            .collect::<Vec<_>>();

        let list = List::new(items)
            .block(Block::default().title("Repositories").borders(Borders::ALL))
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        let mut list_state =
            ListState::default().with_selected(Some(self.highlighted_index - offset));
        frame.render_stateful_widget(list, area, &mut list_state);
    }

    fn visible_offset(&self, visible_height: usize) -> usize {
        if visible_height == 0 || self.highlighted_index < visible_height {
            0
        } else {
            self.highlighted_index + 1 - visible_height
        }
    }

    fn clear_validation(&mut self) {
        self.validation_message = None;
    }

    fn move_highlight_up(&mut self) {
        if self.filtered_indices.is_empty() || self.highlighted_index == 0 {
            return;
        }
        self.highlighted_index -= 1;
    }

    fn move_highlight_down(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }

        let max_index = self.filtered_indices.len() - 1;
        if self.highlighted_index < max_index {
            self.highlighted_index += 1;
        }
    }

    fn toggle_highlighted(&mut self) {
        let Some(option_index) = self.filtered_indices.get(self.highlighted_index).copied() else {
            return;
        };

        if !self.checked_indices.insert(option_index) {
            self.checked_indices.remove(&option_index);
        }
    }

    fn submit(&mut self) -> PickerOutcome {
        if self.checked_indices.is_empty() {
            self.validation_message = Some(EMPTY_SELECTION_MESSAGE);
            return PickerOutcome::Continue;
        }

        PickerOutcome::Submit(
            self.options
                .iter()
                .enumerate()
                .filter_map(|(index, option)| {
                    self.checked_indices
                        .contains(&index)
                        .then(|| option.repo_root.clone())
                })
                .collect(),
        )
    }

    fn refresh_filter(&mut self) {
        if self.query.is_empty() {
            self.filtered_indices = (0..self.options.len()).collect();
        } else {
            let mut matches = self
                .options
                .iter()
                .enumerate()
                .filter_map(|(index, option)| {
                    MATCHER
                        .fuzzy_match(&option.label, &self.query)
                        .map(|score| (index, score))
                })
                .collect::<Vec<_>>();
            matches.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            self.filtered_indices = matches.into_iter().map(|(index, _)| index).collect();
        }

        self.highlighted_index = 0;
    }
}

pub(crate) fn prompt_for_repo_selection(options: Vec<RepoPromptOption>) -> Result<Vec<PathBuf>> {
    let mut terminal = ratatui::init();
    let _terminal_guard = TerminalGuard;
    run_picker(&mut terminal, RepoPickerState::new(options))
}

fn run_picker(terminal: &mut DefaultTerminal, mut state: RepoPickerState) -> Result<Vec<PathBuf>> {
    loop {
        terminal.draw(|frame| state.render(frame))?;

        let event = event::read()?;
        let Event::Key(key_event) = event else {
            continue;
        };

        match state.handle_key_event(key_event) {
            PickerOutcome::Continue => continue,
            PickerOutcome::Submit(selected) => return Ok(selected),
            PickerOutcome::Cancel => {
                return Err(anyhow!("interactive repo selection was canceled"))
            }
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

#[cfg(test)]
mod tests {
    use super::{PickerOutcome, RepoPickerState, RepoPromptOption, HELP_MESSAGE};
    use anyhow::Result;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::Terminal;
    use std::path::PathBuf;

    #[test]
    fn fuzzy_filter_preserves_checked_selection() {
        let mut state = sample_state();

        assert_eq!(
            state.handle_key_event(char_key('b')),
            PickerOutcome::Continue
        );
        assert_eq!(
            state.handle_key_event(char_key('t')),
            PickerOutcome::Continue
        );
        assert_eq!(state.filtered_indices(), &[1]);
        assert_eq!(state.handle_key_event(space_key()), PickerOutcome::Continue);
        assert_eq!(state.checked_indices().len(), 1);
        assert!(state.checked_indices().contains(&1));

        assert_eq!(
            state.handle_key_event(backspace_key()),
            PickerOutcome::Continue
        );
        assert_eq!(
            state.handle_key_event(backspace_key()),
            PickerOutcome::Continue
        );
        assert_eq!(state.filtered_indices(), &[0, 1, 2]);
        assert!(state.checked_indices().contains(&1));
    }

    #[test]
    fn arrow_keys_move_highlighted_repo() {
        let mut state = sample_state();

        assert_eq!(
            state.highlighted_repo_root(),
            Some(&PathBuf::from("/tmp/alpha"))
        );
        assert_eq!(state.handle_key_event(down_key()), PickerOutcome::Continue);
        assert_eq!(
            state.highlighted_repo_root(),
            Some(&PathBuf::from("/tmp/beta"))
        );
        assert_eq!(state.handle_key_event(down_key()), PickerOutcome::Continue);
        assert_eq!(
            state.highlighted_repo_root(),
            Some(&PathBuf::from("/tmp/gamma"))
        );
        assert_eq!(state.handle_key_event(up_key()), PickerOutcome::Continue);
        assert_eq!(
            state.highlighted_repo_root(),
            Some(&PathBuf::from("/tmp/beta"))
        );
    }

    #[test]
    fn space_toggles_highlighted_repo() {
        let mut state = sample_state();

        assert_eq!(state.handle_key_event(down_key()), PickerOutcome::Continue);
        assert_eq!(state.handle_key_event(space_key()), PickerOutcome::Continue);
        assert!(state.checked_indices().contains(&1));
        assert_eq!(state.handle_key_event(space_key()), PickerOutcome::Continue);
        assert!(!state.checked_indices().contains(&1));
    }

    #[test]
    fn enter_requires_selection_before_submitting() {
        let mut state = sample_state();

        assert_eq!(state.handle_key_event(enter_key()), PickerOutcome::Continue);
        assert_eq!(
            state.status_message(),
            Some("Select at least one repository.")
        );

        assert_eq!(state.handle_key_event(down_key()), PickerOutcome::Continue);
        assert_eq!(state.handle_key_event(space_key()), PickerOutcome::Continue);
        assert_eq!(
            state.handle_key_event(enter_key()),
            PickerOutcome::Submit(vec![PathBuf::from("/tmp/beta")])
        );
    }

    #[test]
    fn escape_cancels_picker() {
        let mut state = sample_state();
        assert_eq!(state.handle_key_event(esc_key()), PickerOutcome::Cancel);
    }

    #[test]
    fn render_shows_filter_checkbox_and_help() -> Result<()> {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend)?;
        let mut state = sample_state();
        state.handle_key_event(char_key('b'));
        state.handle_key_event(char_key('t'));
        state.handle_key_event(space_key());

        terminal.draw(|frame| state.render(frame))?;
        let rendered = format!("{}", terminal.backend());

        assert!(rendered.contains("Select repositories for the workspace"));
        assert!(rendered.contains("Filter"));
        assert!(rendered.contains("bt"));
        assert!(rendered.contains("[x] beta"));
        assert!(rendered.contains(HELP_MESSAGE));

        Ok(())
    }

    fn sample_state() -> RepoPickerState {
        RepoPickerState::new(vec![
            RepoPromptOption {
                label: "alpha".into(),
                repo_root: PathBuf::from("/tmp/alpha"),
            },
            RepoPromptOption {
                label: "beta".into(),
                repo_root: PathBuf::from("/tmp/beta"),
            },
            RepoPromptOption {
                label: "gamma".into(),
                repo_root: PathBuf::from("/tmp/gamma"),
            },
        ])
    }

    fn char_key(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
    }

    fn backspace_key() -> KeyEvent {
        KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn down_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    }

    fn enter_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn esc_key() -> KeyEvent {
        KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn space_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)
    }

    fn up_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
    }
}
