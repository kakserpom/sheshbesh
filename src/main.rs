use sheshbesh::{Game, Heuristic, RandomDice, Side, tui};
use std::time::Duration;

fn main() {
    // Визуализация: эвристика играет сама с собой, партия проигрывается покадрово.
    // Темп и предел ходов настраиваются через окружение.
    let delay_ms = env_u64("SHESHBESH_DELAY_MS", 150);
    let max_turns = env_u64("SHESHBESH_MAX_TURNS", 1000) as usize;

    let mut dice = RandomDice::from_entropy();
    let mut game = Game::start(vec![Side::A, Side::A.opposite()], &mut dice);
    let mut agent = Heuristic;

    tui::play_animated(
        &mut game,
        &mut dice,
        &mut agent,
        Duration::from_millis(delay_ms),
        max_turns,
    );
}

/// Читает беззнаковое целое из переменной окружения или возвращает значение по умолчанию.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
