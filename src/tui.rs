//! Терминальная визуализация: цветной рендер доски (ANSI) и покадровое
//! проигрывание партии с очисткой экрана.
//!
//! Без внешних зависимостей — используются ANSI-escape напрямую. Каждая сторона
//! окрашена своим цветом; landmark-клетки приглушены. Видимая ширина каждого
//! токена ровно 2 символа (escape-коды ширины не добавляют), так что колонки
//! выравниваются.

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use crate::board::{HOME_DEPTH, PERIMETER, PerimeterIdx, SIDE_LEN, Side};
use crate::render::{cell_coord, landmark, owners_on};
use crate::state::{GameState, Position};
use crate::turn::{Agent, DiceSource, Game};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CLEAR: &str = "\x1b[2J\x1b[H";

/// ANSI-цвет стороны.
fn side_color(side: Side) -> &'static str {
    match side {
        Side::A => "\x1b[96m", // ярко-голубой
        Side::B => "\x1b[92m", // ярко-зелёный
        Side::C => "\x1b[95m", // ярко-пурпурный
        Side::D => "\x1b[93m", // ярко-жёлтый
    }
}

/// Буква стороны в её цвете.
fn colored_side(side: Side) -> String {
    format!("{}{}{}", side_color(side), side.letter(), RESET)
}

/// Цветной токен клетки (ровно 2 видимых символа).
fn colored_token(state: &GameState, abs: PerimeterIdx) -> String {
    let owners = owners_on(state, abs);
    if owners.is_empty() {
        return format!("{DIM}{} {RESET}", landmark(abs));
    }
    let mut distinct: Vec<Side> = Vec::new();
    for &s in &owners {
        if !distinct.contains(&s) {
            distinct.push(s);
        }
    }
    if distinct.len() == 1 {
        // Одна сторона: буква + число фишек.
        format!(
            "{}{}{}{}",
            side_color(distinct[0]),
            distinct[0].letter(),
            owners.len().min(9),
            RESET,
        )
    } else {
        // Совместное стояние: до двух букв, каждая своим цветом.
        let mut out = String::new();
        for &s in distinct.iter().take(2) {
            out.push_str(side_color(s));
            out.push(s.letter());
        }
        out.push_str(RESET);
        out
    }
}

/// Цветной квадрат периметра.
fn colored_board(state: &GameState) -> String {
    let mut grid = vec![vec![String::from("  "); SIDE_LEN]; SIDE_LEN];
    for abs in 0..PERIMETER {
        let (r, c) = cell_coord(abs);
        grid[r][c] = colored_token(state, PerimeterIdx::new(abs));
    }
    grid.iter()
        .map(|row| row.join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Цветная панель «вне доски» по каждой активной стороне.
fn colored_panel(state: &GameState) -> String {
    let mut lines = Vec::new();
    for &side in &state.active {
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

        lines.push(format!(
            "Сторона {}: резерв {reserve} | дом [{home}] | луна [{moon}] | плен {captured}",
            colored_side(side),
        ));
    }
    lines.join("\n")
}

/// Полный кадр: очистка экрана, заголовок, доска, панель.
pub fn frame(state: &GameState, header: &str) -> String {
    format!(
        "{CLEAR}{header}\n\n{}\n\n{}",
        colored_board(state),
        colored_panel(state),
    )
}

/// Заголовок по итогу хода.
fn turn_header(turns: usize, outcome: &crate::turn::TurnOutcome) -> String {
    let [a, b] = outcome.roll.values();
    format!(
        "Ход {turns}: {} бросил [{a}+{b}], ходов {}{}{}",
        colored_side(outcome.side),
        outcome.played.len(),
        if outcome.again { " • дубль" } else { "" },
        if outcome.forced.is_empty() {
            ""
        } else {
            " • выкуп"
        },
    )
}

/// Проигрывает партию покадрово: между кадрами пауза `delay`, не более
/// `max_turns` ходов. Кадры пишутся в stdout с очисткой экрана.
pub fn play_animated<D, A>(
    game: &mut Game,
    dice: &mut D,
    agent: &mut A,
    delay: Duration,
    max_turns: usize,
) where
    D: DiceSource,
    A: Agent,
{
    let mut out = io::stdout();
    let intro = format!(
        "Шеш-Беш — право первого хода: {}",
        colored_side(game.state.to_move),
    );
    let _ = write!(out, "{}", frame(&game.state, &intro));
    let _ = out.flush();
    thread::sleep(delay);

    let mut turns = 0;
    while game.winner().is_none() && turns < max_turns {
        let outcome = game.play_turn(dice, agent);
        turns += 1;
        let _ = write!(out, "{}", frame(&game.state, &turn_header(turns, &outcome)));
        let _ = out.flush();
        thread::sleep(delay);
    }

    let footer = match game.winner() {
        Some(s) => format!("Победила сторона {}", colored_side(s)),
        None => format!("За {turns} ходов победитель не определился"),
    };
    let _ = writeln!(out, "{}", frame(&game.state, &footer));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_clear_colors_and_content() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::OnTrack { progress: 0 };
        let f = frame(&state, "ЗАГОЛОВОК");
        assert!(f.starts_with(CLEAR)); // очистка экрана
        assert!(f.contains("ЗАГОЛОВОК"));
        assert!(f.contains(side_color(Side::A))); // цвет фишки A
        assert!(f.contains("Сторона"));
    }

    #[test]
    fn token_visible_width_is_two() {
        // Видимая ширина (без ANSI-кодов) должна быть ровно 2 — для выравнивания.
        let state = GameState::new(vec![Side::A, Side::C], Side::A);
        let strip = |s: String| {
            // грубое удаление ANSI-последовательностей вида \x1b[..m
            let mut out = String::new();
            let mut chars = s.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\x1b' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(ch);
                }
            }
            out
        };
        // Пустая клетка-угол.
        let corner = strip(colored_token(&state, Side::A.start_corner()));
        assert_eq!(corner.chars().count(), 2);
    }
}
