//! Интерактивный TUI на ratatui: доска слева, статус и панель справа.
//!
//! Партия проигрывается автоматически с заданным темпом; управление с клавиатуры:
//! `space` — пауза/продолжить, `s` — один ход (в т.ч. на паузе), `q`/`Esc` — выход.
//! Требует настоящего терминала (альт-экран); при выводе в пайп используйте
//! ANSI-анимацию из модуля [`crate::tui`].

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, init, restore};

use crate::board::{HOME_DEPTH, PERIMETER, PerimeterIdx, SIDE_LEN, Side};
use crate::render::{cell_coord, landmark, owners_on};
use crate::state::{GameState, Position};
use crate::turn::{Agent, Game, RandomDice, TurnOutcome};

/// Цвет стороны на доске.
fn side_color(side: Side) -> Color {
    match side {
        Side::A => Color::LightCyan,
        Side::B => Color::LightGreen,
        Side::C => Color::LightMagenta,
        Side::D => Color::LightYellow,
    }
}

/// Спан с буквой стороны в её цвете.
fn side_span(side: Side) -> Span<'static> {
    Span::styled(
        side.letter().to_string(),
        Style::default().fg(side_color(side)),
    )
}

/// Спаны одной клетки периметра (суммарно 2 видимых символа).
fn cell_spans(state: &GameState, abs: PerimeterIdx) -> Vec<Span<'static>> {
    let owners = owners_on(state, abs);
    if owners.is_empty() {
        return vec![Span::styled(
            format!("{} ", landmark(abs)),
            Style::default().fg(Color::DarkGray),
        )];
    }
    let mut distinct: Vec<Side> = Vec::new();
    for &s in &owners {
        if !distinct.contains(&s) {
            distinct.push(s);
        }
    }
    if distinct.len() == 1 {
        vec![Span::styled(
            format!("{}{}", distinct[0].letter(), owners.len().min(9)),
            Style::default().fg(side_color(distinct[0])),
        )]
    } else {
        distinct.iter().take(2).map(|&s| side_span(s)).collect()
    }
}

/// Доска как набор строк-спанов для `Paragraph`.
fn board_lines(state: &GameState) -> Vec<Line<'static>> {
    let mut grid: Vec<Vec<Vec<Span>>> = vec![vec![vec![Span::raw("  ")]; SIDE_LEN]; SIDE_LEN];
    for abs in 0..PERIMETER {
        let (r, c) = cell_coord(abs);
        grid[r][c] = cell_spans(state, PerimeterIdx::new(abs));
    }
    grid.into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (i, cell) in row.into_iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.extend(cell);
            }
            Line::from(spans)
        })
        .collect()
}

/// Панель «вне доски» по каждой стороне.
fn panel_lines(state: &GameState) -> Vec<Line<'static>> {
    state
        .active
        .iter()
        .map(|&side| {
            let own = || state.checkers.iter().filter(move |c| c.owner == side);
            let reserve = own().filter(|c| c.pos == Position::Reserve).count();
            let mut home = String::new();
            for depth in 0..HOME_DEPTH as u8 {
                home.push(if own().any(|c| c.pos == Position::Home { depth }) {
                    '#'
                } else {
                    '_'
                });
            }
            let moon: String = own()
                .filter_map(|c| match c.pos {
                    Position::Moon { field, .. } => Some(match field.required_roll() {
                        1 => '1',
                        3 => '3',
                        _ => '6',
                    }),
                    _ => None,
                })
                .collect();
            let captured = own()
                .filter(|c| matches!(c.pos, Position::Captured { .. }))
                .count();
            Line::from(vec![
                side_span(side),
                Span::raw(format!(
                    ": резерв {reserve} | дом [{home}] | луна [{moon}] | плен {captured}"
                )),
            ])
        })
        .collect()
}

/// Строки статуса: чей ход, последний ход, победитель, подсказки.
fn status_lines(game: &Game, last: Option<&TurnOutcome>, paused: bool) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw("Ход: "),
        side_span(game.state.to_move),
        Span::raw(if paused { "   [ПАУЗА]" } else { "" }),
    ])];

    if let Some(o) = last {
        let [a, b] = o.roll.values();
        let mut spans = vec![Span::raw("Последний: "), side_span(o.side)];
        spans.push(Span::raw(format!(" [{a}+{b}], ходов {}", o.played.len())));
        if o.again {
            spans.push(Span::raw(" • дубль"));
        }
        if !o.forced.is_empty() {
            spans.push(Span::raw(" • выкуп"));
        }
        lines.push(Line::from(spans));
    }

    if let Some(w) = game.winner() {
        lines.push(Line::from(vec![
            Span::raw("Победила сторона "),
            side_span(w),
        ]));
    }

    lines.push(Line::raw(""));
    lines.extend(panel_lines(&game.state));
    lines.push(Line::raw(""));
    lines.push(Line::raw("[space] пауза  [s] шаг  [q] выход"));
    lines
}

/// Рисует один кадр.
fn draw(frame: &mut Frame, game: &Game, last: Option<&TurnOutcome>, paused: bool) {
    let [left, right] =
        Layout::horizontal([Constraint::Length(58), Constraint::Min(20)]).areas(frame.area());
    frame.render_widget(
        Paragraph::new(board_lines(&game.state)).block(Block::bordered().title("Шеш-Беш")),
        left,
    );
    frame.render_widget(
        Paragraph::new(status_lines(game, last, paused)).block(Block::bordered().title("Статус")),
        right,
    );
}

/// Запускает интерактивный TUI: эвристика (или иной `agent`) играет с темпом
/// `step`. Возвращает победителя, если партия завершилась до выхода.
pub fn run<A: Agent>(active: Vec<Side>, agent: &mut A, step: Duration) -> io::Result<Option<Side>> {
    let mut dice = RandomDice::from_entropy();
    let mut game = Game::start(active, &mut dice);
    let mut last: Option<TurnOutcome> = None;
    let mut paused = false;

    let mut terminal = init();
    let result = loop {
        terminal.draw(|f| draw(f, &game, last.as_ref(), paused))?;

        // Ждём событие не дольше `step`; по таймауту — авто-ход.
        if event::poll(step)? {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(game.winner()),
                    KeyCode::Char(' ') => paused = !paused,
                    KeyCode::Char('s') if game.winner().is_none() => {
                        last = Some(game.play_turn(&mut dice, agent));
                    }
                    _ => {}
                }
            }
        } else if !paused && game.winner().is_none() {
            last = Some(game.play_turn(&mut dice, agent));
        }
    };

    restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Содержимое всех спанов строк одной плоской строкой.
    fn flatten(lines: &[Line]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn board_lines_have_landmarks_and_colored_checker() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::OnTrack { progress: 0 };
        let lines = board_lines(&state);
        let text = flatten(&lines);
        for mark in ['+', 'M', 'J', 'h'] {
            assert!(text.contains(mark), "нет landmark {mark}");
        }
        // Фишка A окрашена в свой цвет.
        let a_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.starts_with('A'))
            .expect("должен быть спан с фишкой A");
        assert_eq!(a_span.style.fg, Some(side_color(Side::A)));
    }

    #[test]
    fn status_shows_turn_and_winner() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        for (depth, c) in state
            .checkers
            .iter_mut()
            .filter(|c| c.owner == Side::A)
            .enumerate()
        {
            c.pos = Position::Home { depth: depth as u8 };
        }
        let game = Game::new(state);
        let text = flatten(&status_lines(&game, None, false));
        assert!(text.contains("Ход: "));
        assert!(text.contains("Победила сторона"));
        assert!(text.contains("[space] пауза"));
    }
}
