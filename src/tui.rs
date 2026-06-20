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

use crate::board::{HOME_DEPTH, Side};
use crate::render::{Glyph, board_glyphs, board_scale};
use crate::state::{GameState, Position};
use crate::turn::{Agent, DiceSource, Game};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const MOON: &str = "\x1b[94m"; // ярко-синий — элементы Луны
const PRISON: &str = "\x1b[91m"; // ярко-красный — клетки Тюрьмы
const CAPTIVE_BG: &str = "\x1b[41m"; // красный фон — пленённая фишка
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

/// Цветной 1-символьный токен глифа сетки доски.
fn glyph_token(glyph: Glyph) -> String {
    match glyph {
        Glyph::Empty => " ".to_string(),
        Glyph::Landmark(ch) => format!("{DIM}{ch}{RESET}"),
        Glyph::Moon(ch) => format!("{MOON}{ch}{RESET}"),
        Glyph::Prison(ch) => format!("{PRISON}{ch}{RESET}"),
        Glyph::Checker(side) => format!("{}{}{RESET}", side_color(side), side.letter()),
        Glyph::Captive(side) => format!("{CAPTIVE_BG}{}{}{RESET}", side_color(side), side.letter()),
        Glyph::Overflow => "+".to_string(),
    }
}

/// Цветной квадрат периметра с внешними полями (см. [`crate::render::board_glyphs`]).
fn colored_board(state: &GameState) -> String {
    let pad = " ".repeat(board_scale()); // хвост клетки (горизонтальный масштаб)
    board_glyphs(state)
        .into_iter()
        .map(|row| {
            let mut line = String::new();
            for g in row {
                line.push_str(&glyph_token(g));
                line.push_str(&pad);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `n` маркеров фишки стороны через пробел (или `—`, если ноль).
fn markers(side: Side, n: usize) -> String {
    if n == 0 {
        return format!("{DIM}—{RESET}");
    }
    let one = format!("{}{}{RESET}", side_color(side), side.letter());
    vec![one; n].join(" ")
}

/// Цветная панель «вне доски»: резерв, Дом и плен — маркерами фишек.
fn colored_panel(state: &GameState) -> String {
    let mut lines = Vec::new();
    for &side in &state.active {
        let own = || state.checkers.iter().filter(move |c| c.owner == side);
        let reserve = own().filter(|c| c.pos == Position::Reserve).count();
        let captured = own()
            .filter(|c| matches!(c.pos, Position::Captured { .. }))
            .count();

        let home: String = (0..HOME_DEPTH as u8)
            .map(|depth| {
                if own().any(|c| c.pos == Position::Home { depth }) {
                    format!("{}{}{RESET}", side_color(side), side.letter())
                } else {
                    format!("{DIM}o{RESET}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        lines.push(format!(
            "{}  резерв: {}   дом: {home}   плен: {}",
            colored_side(side),
            markers(side, reserve),
            markers(side, captured),
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
        assert!(f.contains("резерв:")); // панель статуса
        assert!(f.contains("плен:"));
    }

    #[test]
    fn board_renders_landmarks_and_checkers() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::OnTrack { progress: 0 };
        let board = colored_board(&state);
        // Видимый текст без ANSI-кодов содержит landmark-символы и маркер фишки A.
        let mut visible = String::new();
        let mut chars = board.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                visible.push(ch);
            }
        }
        for mark in ['+', 'M', 'J', 'h', 'A'] {
            assert!(visible.contains(mark), "нет символа {mark}");
        }
    }
}
