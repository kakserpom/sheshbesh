//! Эвристический агент: выбирает ход, максимизирующий оценку позиции.
//!
//! Оценка — «жадная» (на один ход вперёд, без перебора будущих бросков): для
//! каждого легального полного хода применяется его последовательность, считается
//! оценка получившейся позиции с точки зрения ходящего, берётся максимум. Этого
//! достаточно, чтобы агент шёл к Дому, ел чужие фишки и не подставлялся зря.

use crate::board::{CellKind, LOCAL_MOON_EXIT, PERIMETER, Side, cell_kind};
use crate::moves::{Move, apply};
use crate::state::{GameState, MoonField, Position};
use crate::turn::Agent;

/// Ценность одной фишки с точки зрения её владельца (больше — ближе к цели).
fn checker_value(owner: Side, pos: Position) -> i32 {
    match pos {
        // Ещё не в игре.
        Position::Reserve => 0,
        // Продвижение по периметру — это и есть прогресс к Дому.
        Position::OnTrack { progress } => progress as i32,
        // На Луне-срезе: считаем по клетке выхода со штрафом за оставшиеся поля
        // (нужны конкретные значения костей, чтобы выбраться).
        Position::Moon { side, field } => {
            let exit = owner.progress_of(side.local_to_perimeter(LOCAL_MOON_EXIT)) as i32;
            let remaining = match field {
                MoonField::One => 3,
                MoonField::Three => 2,
                MoonField::Six => 1,
            };
            exit - remaining * 2
        }
        // В Тюрьме: фишка застряла (нужна «4») — прогресс есть, но со штрафом.
        Position::Prison { cell } => owner.progress_of(cell) as i32 - 12,
        // В Доме — цель достигнута, большой бонус (глубже — чуть ценнее).
        Position::Home { depth } => PERIMETER as i32 + 20 + depth as i32,
        // В плену — очень плохо: нужен выкуп, повторный ввод и весь круг заново.
        Position::Captured { .. } => -25,
    }
}

/// Суммарная ценность фишек стороны.
fn side_score(state: &GameState, side: Side) -> i32 {
    state
        .checkers
        .iter()
        .filter(|c| c.owner == side)
        .map(|c| checker_value(side, c.pos))
        .sum()
}

/// Число бросков из 36, которыми одна фишка соперника бьёт открытую фишку на
/// дистанции `d` клеток (классическая таблица «выстрелов»: прямые + комбинации).
fn shots_for_distance(d: i32) -> i32 {
    match d {
        1 => 11,
        2 => 12,
        3 => 14,
        4 => 15,
        5 => 15,
        6 => 17,
        7 => 6,
        8 => 6,
        9 => 5,
        10 => 3,
        11 => 2,
        12 => 3,
        _ => 0,
    }
}

/// Риск быть съеденным: сумма по открытым фишкам стороны `side` от
/// (вероятность попадания) × (ставка = ценность фишки + 25 за уход в плен).
///
/// Открыта только фишка на обычной клетке периметра; углы, Тюрьма и Луна
/// безопасны. Угрозу создают фишки соперников на дорожке (в пределах 1..12
/// клеток позади) и в резерве (вход по «6» на свою точку входа).
fn capture_risk(state: &GameState, side: Side) -> i32 {
    let opponents: Vec<Side> = state
        .active
        .iter()
        .copied()
        .filter(|&s| s != side)
        .collect();
    let mut total = 0;
    for checker in state.checkers.iter().filter(|c| c.owner == side) {
        let Some(cell) = checker.pos.perimeter_cell(side) else {
            continue;
        };
        if !matches!(cell_kind(cell), CellKind::Plain | CellKind::HomeEntrance) {
            continue;
        }
        let mut shots = 0;
        for &opp in &opponents {
            let cell_in_opp = opp.progress_of(cell) as i32;
            for oc in state.checkers.iter().filter(|c| c.owner == opp) {
                match oc.pos {
                    Position::OnTrack { progress } => {
                        shots += shots_for_distance(cell_in_opp - progress as i32);
                    }
                    // Резервная фишка входит по «6» на точку входа соперника.
                    Position::Reserve if cell_in_opp == 0 => shots += 11,
                    _ => {}
                }
            }
        }
        let shots = shots.min(36);
        let stake = checker_value(side, checker.pos) + 25;
        total += shots * stake / 36;
    }
    total
}

/// Оценка позиции с точки зрения стороны `me`: своё преимущество над сильнейшим
/// соперником за вычетом риска быть съеденным. Съедание чужой фишки роняет её
/// ценность (и поднимает оценку), а собственные открытые фишки — штрафуются.
fn evaluate(state: &GameState, me: Side) -> i32 {
    let mine = side_score(state, me);
    let best_opponent = state
        .active
        .iter()
        .filter(|&&s| s != me)
        .map(|&s| side_score(state, s))
        .max()
        .unwrap_or(0);
    mine - best_opponent - capture_risk(state, me)
}

/// Применяет последовательность ходов к копии состояния (без вынужденных ответных
/// ходов соперника — для оценки этого достаточно).
fn apply_sequence(state: &GameState, seq: &[Move]) -> GameState {
    let mut s = state.clone();
    for &mv in seq {
        s = apply(&s, mv);
    }
    s
}

/// Индекс максимума по ключу (первый при равенстве).
fn argmax_by<T>(items: &[T], mut key: impl FnMut(&T) -> i32) -> usize {
    let mut best = 0;
    let mut best_key = i32::MIN;
    for (i, item) in items.iter().enumerate() {
        let k = key(item);
        if k > best_key {
            best_key = k;
            best = i;
        }
    }
    best
}

/// Эвристический агент: берёт ход с лучшей оценкой позиции.
pub struct Heuristic;

impl Agent for Heuristic {
    fn choose_turn(&mut self, state: &GameState, turns: &[Vec<Move>]) -> usize {
        let me = state.to_move;
        argmax_by(turns, |seq| evaluate(&apply_sequence(state, seq), me))
    }

    fn choose_forced(&mut self, state: &GameState, captor: Side, moves: &[Move]) -> usize {
        argmax_by(moves, |&mv| evaluate(&apply(state, mv), captor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::PerimeterIdx;
    use crate::dice::{DiceRoll, Die};
    use crate::state::Checker;
    use crate::turn::{Game, RandomDice, ScriptedDice};

    fn roll(a: u8, b: u8) -> DiceRoll {
        DiceRoll::new(Die::new(a).unwrap(), Die::new(b).unwrap())
    }

    #[test]
    fn values_order_makes_sense() {
        // Дом > продвинутая фишка > резерв > плен.
        let home = checker_value(Side::A, Position::Home { depth: 0 });
        let advanced = checker_value(Side::A, Position::OnTrack { progress: 60 });
        let reserve = checker_value(Side::A, Position::Reserve);
        let captured = checker_value(Side::A, Position::Captured { captor: Side::C });
        assert!(home > advanced);
        assert!(advanced > reserve);
        assert!(reserve > captured);
    }

    #[test]
    fn prefers_capturing_move() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers.clear();
        // A: фишка 0 на дорожке (progress 0 = abs 9), фишка 1 чуть впереди.
        state.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 0 },
        });
        state.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 6 },
        });
        // C: продвинутая фишка на abs 12 — её можно съесть ходом на 3.
        let target = PerimeterIdx::new(12);
        state.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(target),
            },
        });
        let mut game = Game::new(state);
        // Дубль 3-3: среди максимальных по очкам ходов есть и съедающий.
        let mut dice = ScriptedDice::new(vec![roll(3, 3)]);
        game.play_turn(&mut dice, &mut Heuristic);
        // Эвристика должна была выбрать съедание: фишка C — в плену.
        assert!(matches!(
            game.state.checkers[2].pos,
            Position::Captured { .. }
        ));
    }

    #[test]
    fn capture_risk_sees_exposed_blot() {
        // A-фишка на abs 12 (обычная клетка). C-фишка в 3 клетках позади — угроза.
        let exposed = {
            let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
            s.checkers.clear();
            s.checkers.push(Checker {
                owner: Side::A,
                pos: Position::OnTrack { progress: 3 }, // abs 12
            });
            let cell = PerimeterIdx::new(12);
            let behind = Side::C.progress_of(cell) - 3; // в 3 клетках позади
            s.checkers.push(Checker {
                owner: Side::C,
                pos: Position::OnTrack { progress: behind },
            });
            s
        };
        assert!(capture_risk(&exposed, Side::A) > 0);

        // Та же фишка, но соперник далеко позади (вне досягаемости) — риска нет.
        let safe = {
            let mut s = exposed.clone();
            let cell = PerimeterIdx::new(12);
            s.checkers[1].pos = Position::OnTrack {
                progress: Side::C.progress_of(cell) - 20,
            };
            s
        };
        assert_eq!(capture_risk(&safe, Side::A), 0);
    }

    #[test]
    fn safe_cells_carry_no_risk() {
        // Фишка в Тюрьме не может быть съедена — риск 0, даже если соперник рядом.
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        let prison = Side::A.local_to_perimeter(crate::board::LOCAL_PRISON_NEAR);
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::Prison { cell: prison },
        });
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(prison) - 1,
            },
        });
        assert_eq!(capture_risk(&s, Side::A), 0);
    }

    #[test]
    fn heuristic_self_play_reaches_a_winner() {
        // Партия эвристики против самой себя завершается победителем.
        let mut game = Game::new(GameState::new(vec![Side::A, Side::C], Side::A));
        let mut dice = RandomDice::from_seed(1);
        let winner = game.play_until_winner(&mut dice, &mut Heuristic, 100_000);
        assert!(winner.is_some());
    }
}
