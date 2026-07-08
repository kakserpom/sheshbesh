//! 2-плайный expectiminimax поверх функции ценности [`Value`].
//!
//! Жадный [`best_turn`](crate::value::best_turn) смотрит лишь на ценность позиции
//! ПОСЛЕ своего хода. Здесь добавлен ещё один полу-ход — соперника: для каждого
//! своего хода усредняем по всем **21** различным (взвешенным по вероятности)
//! броскам соперника его лучший для себя ответ и оцениваем получившуюся позицию
//! своими глазами. Дерево: MAX (мы) → CHANCE (бросок соперника) → MAX (соперник),
//! лист — `V(позиция, мы)`.
//!
//! Приближения (осознанные, ради скорости и простоты):
//! - дубль-переброс раскрывается (рекурсия с пределом [`MAX_UNFOLD`] = 2);
//! - вынужденные ответы при выкупе моделируются для captor = `me`;
//! - учитывается лишь **ближайший** соперник — для 3/4 игроков поиск приближён;
//! - свои ходы предварительно отсеиваются по 1-плай-оценке (top-`width`), чтобы не
//!   разворачивать соперника для заведомо слабых вариантов.

use crate::board::Side;
use crate::dice::{DiceRoll, Die};
use crate::moves::{
    Move, MoveKind, apply, forced_six_moves, legal_turns, legal_turns_remaining, remaining_after,
};
use crate::state::{GameState, Position};
use crate::turn::{Agent, next_unfinished_active};
use crate::value::{Value, apply_sequence, best_forced};

/// По умолчанию углубляем столько лучших по 1-плай ходов.
pub const DEFAULT_WIDTH: usize = 8;

/// Сколько дополнительных уровней дубль-переброса раскрывать (0 = не раскрывать).
const MAX_UNFOLD: usize = 2;

/// Все 21 различных броска с весами (вероятность × 36): дубль — вес 1, прочие — 2
/// (порядок костей не важен, поэтому несимметричные пары встречаются вдвое чаще).
fn weighted_rolls() -> Vec<(DiceRoll, f32)> {
    let mut out = Vec::with_capacity(21);
    for a in 1..=6u8 {
        for b in a..=6u8 {
            let roll = DiceRoll::new(Die::new(a).unwrap(), Die::new(b).unwrap());
            out.push((roll, if a == b { 1.0 } else { 2.0 }));
        }
    }
    out
}

/// Ценность позиции `after` (наш афтерстейт, `to_move` — всё ещё мы) с просмотром на
/// один полу-ход соперника вперёд: матожидание по броскам соперника от его лучшего
/// (для него) ответа, оценённого НАШИМИ глазами.
///
/// `depth` — текущая глубина раскрытия дубль-переброса (0 = первый вызов).
fn opponent_reply_value<V: Value>(
    v: &V,
    after: &GameState,
    me: Side,
    rolls: &[(DiceRoll, f32)],
    depth: usize,
) -> f32 {
    let opp = next_unfinished_active(after, me);
    if opp == me {
        // Соперников не осталось (все финишировали) — углублять некого.
        return v.value(after, me);
    }
    let mut board = after.clone();
    board.to_move = opp; // ходы генерируем за соперника
    let mut total = 0.0;
    let mut wsum = 0.0;
    for &(roll, w) in rolls {
        // Соперник выбирает афтерстейт, лучший для СЕБЯ; оцениваем его нашими глазами.
        // `legal_turns` всегда даёт хотя бы один (возможно пустой) ход.
        let mut best_for_opp = f32::NEG_INFINITY;
        let mut leaf_for_me = 0.0;
        for seq in legal_turns(&board, roll) {
            // Применяем последовательность пошагово, вставляя вынужденные ответы
            // при выкупе (если captor = мы).
            let mut reply = board.clone();
            let roll_values = roll.values();
            let mut consumed: u8 = 0;
            for &mv in &seq {
                // Захватчик известен ДО применения выкупа.
                let captor = if mv.kind == MoveKind::Ransom {
                    match reply.checker(mv.checker).pos {
                        Position::Captured { captor } if captor == me => Some(captor),
                        _ => None,
                    }
                } else {
                    None
                };
                reply = apply(&reply, mv);
                consumed += mv.pips;
                // Вынужденный ответный ход захватчика (только если captor = мы).
                if let Some(captor) = captor {
                    let forced = forced_six_moves(&reply, captor);
                    if !forced.is_empty() {
                        let fidx = best_forced(v, &reply, captor, &forced);
                        reply = apply(&reply, forced[fidx]);
                    }
                    // После выкупа и ответа захватчика исходная последовательность
                    // дальше недействительна — состояние изменилось. Пересчитываем
                    // оставшиеся кости и ищем легальные продолжения.
                    let rem = remaining_after(&roll_values, consumed);
                    if !rem.is_empty() {
                        let mut best_reply = reply.clone();
                        let mut best_ov = f32::NEG_INFINITY;
                        for s in legal_turns_remaining(&reply, &rem) {
                            let after = apply_sequence(&reply, &s);
                            let ov = v.value(&after, opp);
                            if ov > best_ov {
                                best_ov = ov;
                                best_reply = after;
                            }
                        }
                        reply = best_reply;
                    }
                    break;
                }
            }
            // Дубль: соперник получает ещё один ход.
            if roll.is_double() && depth < MAX_UNFOLD {
                let ov = v.value(&reply, opp);
                if ov > best_for_opp {
                    best_for_opp = ov;
                    leaf_for_me = opponent_reply_value(v, &reply, me, rolls, depth + 1);
                }
            } else {
                let ov = v.value(&reply, opp);
                if ov > best_for_opp {
                    best_for_opp = ov;
                    leaf_for_me = v.value(&reply, me);
                }
            }
        }
        total += w * leaf_for_me;
        wsum += w;
    }
    total / wsum
}

/// Индекс лучшего хода по 2-плай expectiminimax-оценке. `width` — сколько лучших по
/// 1-плай ходов углублять (см. [`DEFAULT_WIDTH`]).
pub fn best_turn_2ply<V: Value>(
    v: &V,
    width: usize,
    state: &GameState,
    turns: &[Vec<Move>],
) -> usize {
    let me = state.to_move;
    if turns.len() <= 1 {
        return 0;
    }
    // 1-плай оценка каждого хода + сам афтерстейт (его потом углубляем).
    let mut scored: Vec<(usize, f32, GameState)> = turns
        .iter()
        .enumerate()
        .map(|(i, seq)| {
            let after = apply_sequence(state, seq);
            let v1 = v.value(&after, me);
            (i, v1, after)
        })
        .collect();
    // Берём top-`width` по 1-плай (сортировка по убыванию ценности).
    scored.sort_by(|x, y| y.1.total_cmp(&x.1));
    let k = width.max(1).min(scored.len());
    let rolls = weighted_rolls();
    let mut best_idx = scored[0].0;
    let mut best_score = f32::NEG_INFINITY;
    for (i, _, after) in scored.iter().take(k) {
        let score = opponent_reply_value(v, after, me, &rolls, 0);
        if score > best_score {
            best_score = score;
            best_idx = *i;
        }
    }
    best_idx
}

/// Агент: 2-плай expectiminimax поверх ценности `V`. Вынужденный ответ при выкупе
/// выбирается по 1-плай (как и у `ValueAgent`).
pub struct TwoPly<V> {
    pub value: V,
    pub width: usize,
}

impl<V: Value> TwoPly<V> {
    pub fn new(value: V) -> Self {
        Self {
            value,
            width: DEFAULT_WIDTH,
        }
    }
}

impl<V: Value> Agent for TwoPly<V> {
    fn choose_turn(&mut self, state: &GameState, turns: &[Vec<Move>]) -> usize {
        best_turn_2ply(&self.value, self.width, state, turns)
    }

    fn choose_forced(&mut self, state: &GameState, captor: Side, moves: &[Move]) -> usize {
        best_forced(&self.value, state, captor, moves)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Heuristic;
    use crate::dice::Die;
    use crate::eval::match_winrate;
    use crate::value::ValueAgent;

    fn roll(a: u8, b: u8) -> DiceRoll {
        DiceRoll::new(Die::new(a).unwrap(), Die::new(b).unwrap())
    }

    #[test]
    fn weighted_rolls_cover_all_36() {
        let rolls = weighted_rolls();
        assert_eq!(rolls.len(), 21);
        let total: f32 = rolls.iter().map(|&(_, w)| w).sum();
        assert!((total - 36.0).abs() < 1e-3, "сумма весов должна быть 36");
    }

    #[test]
    fn picks_a_valid_turn() {
        let state = GameState::new(vec![Side::A, Side::C], Side::A);
        let turns = legal_turns(&state, roll(6, 6));
        let i = best_turn_2ply(&Heuristic, DEFAULT_WIDTH, &state, &turns);
        assert!(i < turns.len());
    }

    // Дорого: 2-плай поверх той же оценки должен играть НЕ ХУЖЕ 1-плай (обычно лучше).
    #[test]
    #[ignore = "медленный матч на 200 партий; гонять с --release"]
    fn two_ply_beats_one_ply_heuristic() {
        let (two, one) = match_winrate(
            &mut TwoPly::new(Heuristic),
            &mut ValueAgent(Heuristic),
            200,
            7,
            1500,
        );
        assert!(two >= one, "2-плай не должен уступать 1-плай: {two} к {one}");
    }
}
