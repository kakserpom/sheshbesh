use std::io::{IsTerminal, stdout};
use std::time::Duration;

use sheshbesh::{Game, Heuristic, RandomDice, Side, app, tui};

fn main() {
    // Темп и предел ходов настраиваются через окружение.
    let delay = Duration::from_millis(env_u64("SHESHBESH_DELAY_MS", 150));
    let max_turns = env_u64("SHESHBESH_MAX_TURNS", 1000) as usize;
    let sides = vec![Side::A, Side::A.opposite()];
    let mut agent = Heuristic;

    if stdout().is_terminal() {
        // Настоящий терминал — интерактивный TUI на ratatui.
        if let Err(e) = app::run(sides, &mut agent, delay) {
            eprintln!("Ошибка TUI: {e}");
        }
    } else {
        // Вывод в пайп/лог — потоковая ANSI-анимация.
        let mut dice = RandomDice::from_entropy();
        let mut game = Game::start(sides, &mut dice);
        tui::play_animated(&mut game, &mut dice, &mut agent, delay, max_turns);
    }
}

/// Читает беззнаковое целое из переменной окружения или возвращает значение по умолчанию.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
