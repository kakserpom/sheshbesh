//! Эвристический агент: выбирает ход, максимизирующий оценку позиции.
//!
//! Оценка — «жадная» (на один ход вперёд, без перебора будущих бросков): для
//! каждого легального полного хода применяется его последовательность, считается
//! оценка получившейся позиции с точки зрения ходящего, берётся максимум. Этого
//! достаточно, чтобы агент шёл к Дому, ел чужие фишки и не подставлялся зря.

use crate::board::{CellKind, LOCAL_MOON_EXIT, PERIMETER, Side, cell_kind};
use crate::moves::{Move, occupied};
use crate::state::{GameState, MoonField, Position};
use crate::turn::Agent;
use crate::value::{Value, best_forced, best_turn};

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
        .checkers()
        .iter()
        .filter(|c| c.owner == side)
        .map(|c| checker_value(side, c.pos))
        .sum()
}

/// Число бросков из 36, которыми фишка `ao` с прогресса `p` дотягивается до цели
/// на дистанции `d` клеток (1..12) — с учётом блокировки: прямой выстрел требует
/// свободного пути, комбинация — ещё и проходимой промежуточной остановки.
/// Углы прозрачны для прохода; на Луну/Тюрьму промежуточно встать нельзя.
fn shots_to(state: &GameState, ao: Side, p: u16, d: i32) -> i32 {
    if !(1..=12).contains(&d) {
        return 0;
    }
    let d = d as usize;
    let abs = |k: usize| ao.entry().advance(p as usize + k);

    // Предвычислим для дистанций 1..=d: блокирует ли клетка проход и можно ли на
    // ней промежуточно остановиться (обычная/угол/вход в Дом — да; Луна/Тюрьма — нет).
    let mut blocks = [false; 13];
    let mut stop_kind = [false; 13];
    for k in 1..=d {
        let kind = cell_kind(abs(k));
        blocks[k] = kind != CellKind::Corner && occupied(state, abs(k));
        stop_kind[k] = matches!(
            kind,
            CellKind::Plain | CellKind::Corner | CellKind::HomeEntrance
        );
    }
    // Свободны ли промежуточные клетки lo..=hi-1.
    let seg_clear = |lo: usize, hi: usize| (lo..hi).all(|k| !blocks[k]);
    // Можно ли промежуточно остановиться на дистанции a (для комбинации).
    let stoppable = |a: usize| a < d && stop_kind[a] && seg_clear(1, a);

    let direct_ok = d <= 6 && seg_clear(1, d);
    let mut shots = 0;
    for x in 1..=6usize {
        for y in 1..=6usize {
            let hit = (direct_ok && (x == d || y == d))
                || (x + y == d
                    && ((stoppable(x) && seg_clear(x + 1, d))
                        || (stoppable(y) && seg_clear(y + 1, d))));
            if hit {
                shots += 1;
            }
        }
    }
    shots
}

/// Угроза фишкам стороны `victim` со стороны `attacker`: сумма по открытым
/// фишкам `victim` от (вероятность попадания фишками `attacker`) × (ставка =
/// ценность фишки + 25 за пленение).
///
/// Открыта только фишка на обычной клетке периметра; углы, Тюрьма и Луна
/// безопасны. Бьют фишки `attacker` на дорожке (в пределах 1..12 клеток позади)
/// и в резерве (вход по «6» на свою точку входа).
fn threats_against(state: &GameState, victim: Side, attacker: Side) -> i32 {
    let mut total = 0;
    for checker in state.checkers().iter().filter(|c| c.owner == victim) {
        let Some(cell) = checker.pos.perimeter_cell(victim) else {
            continue;
        };
        if !matches!(cell_kind(cell), CellKind::Plain | CellKind::HomeEntrance) {
            continue;
        }
        let cell_in_attacker = attacker.progress_of(cell) as i32;
        let mut shots = 0;
        for oc in state.checkers().iter().filter(|c| c.owner == attacker) {
            match oc.pos {
                Position::OnTrack { progress } => {
                    shots += shots_to(
                        state,
                        attacker,
                        progress,
                        cell_in_attacker - progress as i32,
                    );
                }
                // Резервная фишка входит по «6» на точку входа атакующего.
                Position::Reserve if cell_in_attacker == 0 => shots += 11,
                _ => {}
            }
        }
        let shots = shots.min(36);
        let stake = checker_value(victim, checker.pos) + 25;
        total += shots * stake / 36;
    }
    total
}

/// Стороны-соперники для `side` (союзники в командном режиме исключены — они не
/// едят друг друга, поэтому не создают угроз и не оцениваются как враги).
fn opponents_of(state: &GameState, side: Side) -> impl Iterator<Item = Side> + '_ {
    state
        .active
        .iter()
        .copied()
        .filter(move |&s| s != side && !state.are_allied(side, s))
}

/// Риск быть съеденным: суммарная угроза моим фишкам со стороны всех соперников.
fn capture_risk(state: &GameState, side: Side) -> i32 {
    opponents_of(state, side)
        .map(|opp| threats_against(state, side, opp))
        .sum()
}

/// Угрозы, которые `side` создаёт чужим открытым фишкам, — по всем соперникам.
fn threat_value(state: &GameState, side: Side) -> i32 {
    opponents_of(state, side)
        .map(|opp| threats_against(state, opp, side))
        .sum()
}

/// Вес атакующего бонуса относительно защитного штрафа: угроза реализуется лишь
/// на следующем своём ходу (соперник успевает увести фишку), поэтому — наполовину.
const THREAT_BONUS_NUM: i32 = 1;
const THREAT_BONUS_DEN: i32 = 2;

/// Оценка позиции с точки зрения стороны `me`: преимущество над сильнейшим
/// соперником, минус риск быть съеденным, плюс (слабее) свои угрозы чужим
/// открытым фишкам.
fn evaluate(state: &GameState, me: Side) -> i32 {
    let mine = side_score(state, me);
    let best_opponent = opponents_of(state, me)
        .map(|s| side_score(state, s))
        .max()
        .unwrap_or(0);
    mine - best_opponent - capture_risk(state, me)
        + threat_value(state, me) * THREAT_BONUS_NUM / THREAT_BONUS_DEN
}

/// Эвристический агент: берёт ход с лучшей оценкой позиции.
pub struct Heuristic;

/// Эвристика как функция ценности: ручная оценка `evaluate`.
impl Value for Heuristic {
    fn value(&self, state: &GameState, side: Side) -> f32 {
        evaluate(state, side) as f32
    }
}

impl Agent for Heuristic {
    fn choose_turn(&mut self, state: &GameState, turns: &[Vec<Move>]) -> usize {
        best_turn(self, state, turns)
    }

    fn choose_forced(&mut self, state: &GameState, captor: Side, moves: &[Move]) -> usize {
        best_forced(self, state, captor, moves)
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
    fn shots_account_for_blocked_lines() {
        // Атакующий A на progress 0 (abs 9); цель на дистанции 4 (abs 13).
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.clear_checkers();
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 0 },
        });
        let clear = shots_to(&s, Side::A, 0, 4);
        assert!(clear > 0);

        // Блокер на дистанции 2 (abs 11) перекрывает прямой выстрел и комбинации.
        let blocker = PerimeterIdx::new(11);
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(blocker),
            },
        });
        let blocked = shots_to(&s, Side::A, 0, 4);
        assert!(blocked < clear, "блокер должен снижать число выстрелов");

        // Дистанция вне досягаемости — выстрелов нет.
        assert_eq!(shots_to(&s, Side::A, 0, 20), 0);
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
        state.clear_checkers();
        // A: фишка 0 на дорожке (progress 0 = abs 9), фишка 1 чуть впереди.
        state.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 0 },
        });
        state.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 6 },
        });
        // C: продвинутая фишка на abs 12 — её можно съесть ходом на 3.
        let target = PerimeterIdx::new(12);
        state.push_checker(Checker {
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
            game.state.checker(2).pos,
            Position::Captured { .. }
        ));
    }

    #[test]
    fn capture_risk_sees_exposed_blot() {
        // A-фишка на abs 12 (обычная клетка). C-фишка в 3 клетках позади — угроза.
        let exposed = {
            let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
            s.clear_checkers();
            s.push_checker(Checker {
                owner: Side::A,
                pos: Position::OnTrack { progress: 3 }, // abs 12
            });
            let cell = PerimeterIdx::new(12);
            let behind = Side::C.progress_of(cell) - 3; // в 3 клетках позади
            s.push_checker(Checker {
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
            s.set_checker_pos(1, Position::OnTrack {
                progress: Side::C.progress_of(cell) - 20,
            });
            s
        };
        assert_eq!(capture_risk(&safe, Side::A), 0);
    }

    #[test]
    fn threat_value_sees_attackable_enemy_blot() {
        // C-фишка открыта на abs 12; моя (A) фишка в 3 клетках позади — угроза.
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.clear_checkers();
        let cell = PerimeterIdx::new(12);
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(cell),
            },
        });
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack {
                progress: Side::A.progress_of(cell) - 3,
            },
        });
        // Для A это угроза по чужой фишке; для C — риск (симметрия).
        assert!(threat_value(&s, Side::A) > 0);
        assert_eq!(threat_value(&s, Side::A), capture_risk(&s, Side::C));
    }

    #[test]
    fn safe_cells_carry_no_risk() {
        // Фишка в Тюрьме не может быть съедена — риск 0, даже если соперник рядом.
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.clear_checkers();
        let prison = Side::A.local_to_perimeter(crate::board::LOCAL_PRISON_NEAR);
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::Prison { cell: prison },
        });
        s.push_checker(Checker {
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
