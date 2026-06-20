use std::io::{IsTerminal, stdout};
use std::time::Duration;

use sheshbesh::{Game, Heuristic, RandomDice, Side, app, tui};

fn main() {
    // Темп (пауза после хода ИИ) и предел ходов для ANSI-режима — через окружение.
    let delay = Duration::from_millis(env_u64("SHESHBESH_DELAY_MS", 600));
    let max_turns = env_u64("SHESHBESH_MAX_TURNS", 1000) as usize;
    let sides = vec![Side::A, Side::A.opposite()];
    // Какими сторонами играет человек (буквы через запятую); по умолчанию — A.
    let humans = parse_humans(&sides);
    let mut agent = Heuristic;

    if stdout().is_terminal() {
        // Настоящий терминал — интерактивная игра в ratatui-TUI.
        if let Err(e) = app::run_interactive(sides, &humans, &mut agent, delay) {
            eprintln!("Ошибка TUI: {e}");
        }
    } else {
        // Вывод в пайп/лог — потоковая ANSI-анимация партии ИИ.
        let mut dice = RandomDice::from_entropy();
        let mut game = Game::start(sides, &mut dice);
        tui::play_animated(&mut game, &mut dice, &mut agent, delay, max_turns);
    }
}

/// Стороны-люди из `SHESHBESH_HUMAN` (например `A` или `A,C`); по умолчанию `A`.
/// Оставляются только активные стороны.
fn parse_humans(active: &[Side]) -> Vec<Side> {
    let spec = std::env::var("SHESHBESH_HUMAN").unwrap_or_else(|_| "A".to_string());
    spec.split(',')
        .filter_map(|t| match t.trim().to_ascii_uppercase().as_str() {
            "A" => Some(Side::A),
            "B" => Some(Side::B),
            "C" => Some(Side::C),
            "D" => Some(Side::D),
            _ => None,
        })
        .filter(|s| active.contains(s))
        .collect()
}

/// Читает беззнаковое целое из переменной окружения или возвращает значение по умолчанию.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
