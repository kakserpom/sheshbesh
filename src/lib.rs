//! Шеш-Беш — самобытный вариант нард.
//!
//! Этот crate содержит модель доски и состояния игры. Правила — в `CLAUDE.md`
//! (раздел «Правила игры»); здесь закодирована только их структурная часть.
//! Генерация ходов и движок правил будут добавлены отдельно.

pub mod ai;
pub mod board;
pub mod dice;
pub mod encode;
pub mod eval;
pub mod learn;
pub mod moves;
pub mod render;
pub mod state;
pub mod turn;
pub mod value;

// Терминальные интерфейсы — только с фичей `tui` (тянут crossterm/ratatui).
// Без неё крейт остаётся чистым движком, пригодным для компиляции в WASM.
#[cfg(feature = "tui")]
pub mod app;
#[cfg(feature = "tui")]
pub mod tui;

pub use ai::Heuristic;
pub use board::{CellKind, PerimeterIdx, Side};
pub use dice::{DiceRoll, Die};
pub use encode::{FEATURES, encode, encode_into, rotate_state};
pub use eval::{match_winrate, play_game, play_game_2p, winrate_ffa, winrate_teams};
pub use learn::{LinearValue, MlpValue, TdConfig, TdModel, train, train_mlp, train_with};
pub use moves::{Move, MoveKind, apply, legal_moves, legal_turns, max_pips};
pub use render::{
    BOARD_DIM, BOARD_MARGIN, Glyph, board_glyphs, checker_cell, margin_coord, render,
};
pub use state::{Checker, GameState, MoonField, Position};
pub use turn::{
    Agent, DiceSource, FirstChoice, Game, RandomDice, ScriptedDice, TurnOutcome, determine_first,
    next_active_side, team_of, winners_of,
};
pub use value::{Value, ValueAgent, best_forced, best_turn};
