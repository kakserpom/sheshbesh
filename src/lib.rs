//! Шеш-Беш — самобытный вариант нард.
//!
//! Этот crate содержит модель доски и состояния игры. Правила — в `CLAUDE.md`
//! (раздел «Правила игры»); здесь закодирована только их структурная часть.
//! Генерация ходов и движок правил будут добавлены отдельно.

pub mod ai;
pub mod app;
pub mod board;
pub mod dice;
pub mod moves;
pub mod render;
pub mod state;
pub mod tui;
pub mod turn;

pub use ai::Heuristic;
pub use board::{CellKind, PerimeterIdx, Side};
pub use dice::{DiceRoll, Die};
pub use moves::{Move, MoveKind, apply, legal_moves, legal_turns, max_pips};
pub use render::render;
pub use state::{Checker, GameState, MoonField, Position};
pub use turn::{
    Agent, DiceSource, FirstChoice, Game, RandomDice, ScriptedDice, TurnOutcome, determine_first,
    next_active_side,
};
