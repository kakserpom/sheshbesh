//! Оценка позиций и выбор хода по функции ценности — общая база для эвристики и
//! будущего обучаемого ИИ.
//!
//! `Value` — функция ценности позиции с точки зрения стороны. Агент (`ValueAgent`)
//! выбирает ход, максимизирующий ценность **получившейся** позиции (афтерстейта).
//! Это та же схема, что и у `Heuristic`, но с произвольной (в т.ч. обучаемой)
//! оценкой: афтерстейт детерминирован (кости уже известны), а случайность
//! следующего броска функция ценности усредняет неявно — классический приём
//! TD-Gammon для игр с костями.

use crate::board::Side;
use crate::moves::{Move, apply};
use crate::state::GameState;
use crate::turn::Agent;

/// Функция ценности позиции с точки зрения стороны `side` (больше — лучше для неё).
pub trait Value {
    /// Оценка позиции `state` для стороны `side`.
    fn value(&self, state: &GameState, side: Side) -> f32;
}

/// Применяет последовательность ходов к копии состояния (без вынужденных ответных
/// ходов соперника — для оценки афтерстейта этого достаточно).
pub fn apply_sequence(state: &GameState, seq: &[Move]) -> GameState {
    let mut s = state.clone();
    for &mv in seq {
        s = apply(&s, mv);
    }
    s
}

/// Индекс максимума по ключу (первый при равенстве).
fn argmax_by<T>(items: &[T], mut key: impl FnMut(&T) -> f32) -> usize {
    let mut best = 0;
    let mut best_key = f32::NEG_INFINITY;
    for (i, item) in items.iter().enumerate() {
        let k = key(item);
        if k > best_key {
            best_key = k;
            best = i;
        }
    }
    best
}

/// Индекс лучшего полного хода по ценности афтерстейта (с точки зрения ходящего).
pub fn best_turn<V: Value>(v: &V, state: &GameState, turns: &[Vec<Move>]) -> usize {
    let me = state.to_move;
    argmax_by(turns, |seq| v.value(&apply_sequence(state, seq), me))
}

/// Индекс лучшего вынужденного ответного хода захватчика по ценности афтерстейта.
pub fn best_forced<V: Value>(v: &V, state: &GameState, captor: Side, moves: &[Move]) -> usize {
    argmax_by(moves, |&mv| v.value(&apply(state, mv), captor))
}

/// Агент, выбирающий ход жадно по функции ценности `V` (на афтерстейтах).
pub struct ValueAgent<V>(pub V);

impl<V: Value> Agent for ValueAgent<V> {
    fn choose_turn(&mut self, state: &GameState, turns: &[Vec<Move>]) -> usize {
        best_turn(&self.0, state, turns)
    }

    fn choose_forced(&mut self, state: &GameState, captor: Side, moves: &[Move]) -> usize {
        best_forced(&self.0, state, captor, moves)
    }
}
