//! Движок ходов: генерация легальных ходов, их применение и правило
//! максимального хода.
//!
//! Каждый `Move` тратит значение **одной** кости на **одну** фишку. Объединение
//! очков на одну фишку (`a + b`) — это просто последовательность из двух ходов
//! этой же фишкой; такие последовательности перебирает `legal_turns`.
//!
//! **Дубль** (право на ещё один ход) и **выкуп пленных** обрабатываются на уровне
//! партии — см. модуль `turn`.
//!
//! **Луна полностью безопасна:** вход на Луну уводит фишку на внутреннюю дорожку и
//! не ест; стоять на клетке-входе нельзя (ни один путь не оставляет там `OnTrack`),
//! а фишка на внутренней дорожке недостижима для съедания (`perimeter_cell == None`).

use crate::board::{
    CellKind, HOME_DEPTH, LOCAL_MOON_EXIT, PERIMETER, PerimeterIdx, Side, cell_kind,
};
use crate::dice::DiceRoll;
use crate::state::{GameState, MoonField, Position};

/// Характер хода (для отображения/отладки; следствие позиции и значения кости).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveKind {
    /// Ввод фишки из резерва на точку входа (по «6»).
    Enter,
    /// Обычный шаг по периметру.
    Step,
    /// Шаг, завершившийся попаданием на Луну (уход на внутреннюю дорожку).
    EnterMoon,
    /// Шаг, завершившийся попаданием в Тюрьму.
    EnterPrison,
    /// Продвижение по внутренней дорожке Луны.
    MoonAdvance,
    /// Выход с Луны (по «6») на клетку выхода.
    MoonExit,
    /// Освобождение из Тюрьмы (по «4»): фишка остаётся на той же клетке.
    PrisonRelease,
    /// Проход сквозь Тюрьму: фишка, только что попавшая на клетку Тюрьмы этим ходом,
    /// идёт дальше ещё одной костью, НЕ оставаясь в плену. Разрешён только костью ≠ 4
    /// (костью «4» вместо этого делается «зашёл-вышел» — `PrisonRelease`). Нужен, чтобы
    /// не нарушалось правило максимального хода, когда обойти Тюрьму нечем (например,
    /// единственная фишка, дубль, Тюрьма ровно на бросок впереди).
    PrisonPass,
    /// Заход в Дом (ровно, по 1 фишке в клетку).
    EnterHome,
    /// Продвижение вглубь Дома (фишка уже в Доме идёт на более глубокую клетку,
    /// освобождая место; перепрыгивать занятые клетки нельзя).
    HomeAdvance,
    /// Выкуп своей пленной фишки (по «6»): она уходит в резерв. Влечёт
    /// обязательный ответный ход захватчика на 6 (обрабатывается в туровом цикле).
    Ransom,
}

/// Один ход: значение кости `die`, потраченное на фишку `checker`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    /// Индекс фишки в `GameState::checkers`.
    pub checker: usize,
    /// Значение использованной кости.
    pub die: u8,
    /// Характер хода.
    pub kind: MoveKind,
}

/// Результат разбора хода: новое положение фишки и список съеденных фишек.
struct Resolved {
    new_pos: Position,
    captures: Vec<usize>,
    kind: MoveKind,
}

/// Индексы фишек соперников, стоящих на клетке периметра `cell`. Союзники (в
/// командном режиме) не считаются соперниками — их фишки не едят.
fn enemies_on(state: &GameState, owner: Side, cell: PerimeterIdx) -> Vec<usize> {
    state
        .checkers
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.owner != owner
                && !state.are_allied(owner, c.owner)
                && c.pos.perimeter_cell(c.owner) == Some(cell)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Стоит ли на клетке периметра `cell` хоть одна фишка (любого игрока).
/// Используется для правила «нельзя проходить через фишки» (и оценкой ИИ).
pub(crate) fn occupied(state: &GameState, cell: PerimeterIdx) -> bool {
    state
        .checkers
        .iter()
        .any(|c| c.pos.perimeter_cell(c.owner) == Some(cell))
}

/// Свободен ли путь для хода фишки `owner` из `progress` на `die` клеток:
/// все промежуточные клетки периметра (не считая конечной) должны быть пусты,
/// кроме углов, через которые проходить можно.
fn path_clear(state: &GameState, owner: Side, progress: u16, die: u8) -> bool {
    let target = progress + die as u16;
    // Промежуточные клетки периметра — до конечной (или до входа в Дом).
    let perim_end = target.min(PERIMETER as u16);
    for i in (progress + 1)..perim_end {
        let abs = owner.entry().advance(i as usize);
        if cell_kind(abs) != CellKind::Corner && occupied(state, abs) {
            return false;
        }
    }
    true
}

/// Занята ли клетка Дома глубины `depth` своей фишкой (в Дом — по 1 в клетку).
fn home_depth_taken(state: &GameState, owner: Side, depth: u8) -> bool {
    state
        .checkers
        .iter()
        .any(|c| c.owner == owner && c.pos == Position::Home { depth })
}

/// Разбирает применение значения `die` к фишке `ci`. `None` — ход нелегален.
#[allow(clippy::too_many_lines)]
fn resolve(state: &GameState, ci: usize, die: u8) -> Option<Resolved> {
    let ch = state.checkers[ci];
    let owner = ch.owner;
    match ch.pos {
        Position::Reserve => {
            if die != 6 {
                return None;
            }
            let entry = owner.entry();
            // Нельзя ввести фишку, если на клетке ввода уже стоит СВОЯ фишка
            // (две свои на точке ввода не ставятся). Чужую — можно съесть, вводя свою.
            let own_on_entry = state
                .checkers
                .iter()
                .any(|c| c.owner == owner && c.pos.perimeter_cell(owner) == Some(entry));
            if own_on_entry {
                return None;
            }
            let captures = enemies_on(state, owner, entry);
            Some(Resolved {
                new_pos: Position::OnTrack { progress: 0 },
                captures,
                kind: MoveKind::Enter,
            })
        }
        Position::OnTrack { progress } => {
            let target = progress + die as u16;
            // Нельзя проходить через чужие/свои фишки (кроме стоящих на углу).
            if !path_clear(state, owner, progress, die) {
                return None;
            }
            if (target as usize) < PERIMETER {
                let abs = owner.entry().advance(target as usize);
                let (new_pos, captures, kind) = match cell_kind(abs) {
                    CellKind::Moon => (
                        Position::Moon {
                            side: abs.side(),
                            field: MoonField::One,
                        },
                        vec![],
                        MoveKind::EnterMoon,
                    ),
                    CellKind::Prison => (
                        Position::Prison { cell: abs },
                        vec![],
                        MoveKind::EnterPrison,
                    ),
                    // На углах не едят.
                    CellKind::Corner => (
                        Position::OnTrack { progress: target },
                        vec![],
                        MoveKind::Step,
                    ),
                    CellKind::Plain | CellKind::HomeEntrance => (
                        Position::OnTrack { progress: target },
                        enemies_on(state, owner, abs),
                        MoveKind::Step,
                    ),
                };
                Some(Resolved {
                    new_pos,
                    captures,
                    kind,
                })
            } else {
                // Заход в Дом: ровно по броску, по 1 фишке в клетку, без перебора
                // (target за пределами Дома → нелегально, и второго круга нет).
                // Дом — «коридор» вглубь: **нельзя перепрыгивать** занятые клетки
                // Дома, поэтому все клетки от входа (глубина 0) до целевой должны
                // быть свободны.
                let depth = (target as usize) - PERIMETER;
                let home_path_clear = (0..depth).all(|d| !home_depth_taken(state, owner, d as u8));
                if depth < HOME_DEPTH
                    && !home_depth_taken(state, owner, depth as u8)
                    && home_path_clear
                {
                    Some(Resolved {
                        new_pos: Position::Home { depth: depth as u8 },
                        captures: vec![],
                        kind: MoveKind::EnterHome,
                    })
                } else {
                    None
                }
            }
        }
        Position::Moon { side, field } => {
            if die != field.required_roll() {
                return None;
            }
            if let Some(next) = field.next() {
                Some(Resolved {
                    new_pos: Position::Moon { side, field: next },
                    captures: vec![],
                    kind: MoveKind::MoonAdvance,
                })
            } else {
                let abs = side.local_to_perimeter(LOCAL_MOON_EXIT);
                Some(Resolved {
                    new_pos: Position::OnTrack {
                        progress: owner.progress_of(abs),
                    },
                    captures: enemies_on(state, owner, abs),
                    kind: MoveKind::MoonExit,
                })
            }
        }
        Position::Prison { cell } => {
            if die != 4 {
                return None;
            }
            // Освобождение: фишка остаётся на клетке Тюрьмы, на выходе не ест.
            Some(Resolved {
                new_pos: Position::OnTrack {
                    progress: owner.progress_of(cell),
                },
                captures: vec![],
                kind: MoveKind::PrisonRelease,
            })
        }
        Position::Captured { .. } => {
            if die != 6 {
                return None;
            }
            // Выкуп: пленная фишка уходит в резерв (потом снова вводится по «6»).
            Some(Resolved {
                new_pos: Position::Reserve,
                captures: vec![],
                kind: MoveKind::Ransom,
            })
        }
        Position::Home { depth } => {
            // Фишка в Доме может идти ГЛУБЖЕ (освобождая мелкие клетки для входа
            // других), но не за пределы Дома (перебор) и не перепрыгивая занятые.
            let nd = depth as u16 + die as u16;
            if nd >= HOME_DEPTH as u16 {
                return None;
            }
            let new_depth = nd as u8;
            if (depth + 1..=new_depth).any(|d| home_depth_taken(state, owner, d)) {
                return None;
            }
            Some(Resolved {
                new_pos: Position::Home { depth: new_depth },
                captures: vec![],
                kind: MoveKind::HomeAdvance,
            })
        }
    }
}

/// Проход сквозь Тюрьму: фишка, только что (этим ходом) попавшая на клетку Тюрьмы,
/// идёт дальше ещё одной костью — как обычный шаг от клетки Тюрьмы, не оставаясь в
/// плену. Разрешён ТОЛЬКО костью ≠ 4 (костью «4» делается «зашёл-вышел», `resolve`).
/// `None` — если так не пройти. Применяется только к «свежим» пленникам
/// (см. `maximal_sequences`); обычный пленник так ходить не может.
fn prison_pass(state: &GameState, ci: usize, die: u8) -> Option<Resolved> {
    if die == 4 {
        return None; // костью «4» — «зашёл-вышел» (остаться), а не проход дальше
    }
    let ch = state.checkers[ci];
    let owner = ch.owner;
    let Position::Prison { cell } = ch.pos else {
        return None;
    };
    let progress = owner.progress_of(cell);
    let target = progress + die as u16;
    if !path_clear(state, owner, progress, die) {
        return None;
    }
    let (new_pos, captures) = if (target as usize) < PERIMETER {
        let abs = owner.entry().advance(target as usize);
        match cell_kind(abs) {
            CellKind::Moon => (
                Position::Moon {
                    side: abs.side(),
                    field: MoonField::One,
                },
                vec![],
            ),
            CellKind::Prison => (Position::Prison { cell: abs }, vec![]),
            CellKind::Corner => (Position::OnTrack { progress: target }, vec![]),
            CellKind::Plain | CellKind::HomeEntrance => (
                Position::OnTrack { progress: target },
                enemies_on(state, owner, abs),
            ),
        }
    } else {
        let depth = (target as usize) - PERIMETER;
        if depth < HOME_DEPTH && !home_depth_taken(state, owner, depth as u8) {
            (Position::Home { depth: depth as u8 }, vec![])
        } else {
            return None;
        }
    };
    Some(Resolved {
        new_pos,
        captures,
        kind: MoveKind::PrisonPass,
    })
}

/// Легален ли конкретный ход в данном состоянии (без паники, в отличие от `apply`).
/// Нужно туровому циклу: ответный ход захватчика при выкупе может сделать
/// нелегальным следующий ход выбранной последовательности — такой ход пропускается.
pub fn move_legal(state: &GameState, mv: Move) -> bool {
    if mv.kind == MoveKind::PrisonPass {
        prison_pass(state, mv.checker, mv.die).is_some()
    } else {
        resolve(state, mv.checker, mv.die).is_some()
    }
}

/// Все легальные ходы стороны `side` для значения кости `die`.
fn moves_for_side(state: &GameState, side: Side, die: u8) -> Vec<Move> {
    state
        .checkers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.owner == side)
        .filter_map(|(ci, _)| {
            resolve(state, ci, die).map(|r| Move {
                checker: ci,
                die,
                kind: r.kind,
            })
        })
        .collect()
}

/// Все легальные ходы текущего игрока для значения кости `die`.
fn moves_for_die(state: &GameState, die: u8) -> Vec<Move> {
    moves_for_side(state, state.to_move, die)
}

/// Обязательные ходы захватчика `captor` на «6» — кандидаты для вынужденного
/// ответного хода при выкупе. Пустой список означает, что ход пропускается.
pub fn forced_six_moves(state: &GameState, captor: Side) -> Vec<Move> {
    moves_for_side(state, captor, 6)
}

/// Все легальные одиночные ходы при наличии костей `remaining` (без учёта
/// правила максимального хода — это «сырой» список того, что доступно сейчас).
pub fn legal_moves(state: &GameState, remaining: &[u8]) -> Vec<Move> {
    let mut seen = Vec::new();
    let mut moves = Vec::new();
    for &die in remaining {
        if seen.contains(&die) {
            continue;
        }
        seen.push(die);
        moves.extend(moves_for_die(state, die));
    }
    moves
}

/// Применяет ход, возвращая новое состояние. Съеденные фишки уходят в плен
/// к стороне ходящей фишки. Очередь хода и расход костей не меняются — это
/// забота вызывающего (см. `legal_turns`). Обязательный ответный ход захватчика
/// при выкупе (`Ransom`) тоже на совести вызывающего (см. `turn`).
///
/// # Panics
/// Если ход нелегален в данном состоянии.
pub fn apply(state: &GameState, mv: Move) -> GameState {
    let resolved = if mv.kind == MoveKind::PrisonPass {
        prison_pass(state, mv.checker, mv.die).expect("apply: нелегальный проход Тюрьмы")
    } else {
        resolve(state, mv.checker, mv.die).expect("apply: нелегальный ход")
    };
    let mut next = state.clone();
    // Съеденные фишки достаются стороне, чья фишка ходит (на обычных ходах это
    // текущий игрок; при вынужденном ответном ходе — захватчик).
    let captor = state.checkers[mv.checker].owner;
    for ci in resolved.captures {
        next.checkers[ci].pos = Position::Captured { captor };
    }
    next.checkers[mv.checker].pos = resolved.new_pos;
    next
}

/// Убирает один экземпляр значения `die` из набора костей.
fn without(remaining: &[u8], die: u8) -> Vec<u8> {
    let mut rest = remaining.to_vec();
    if let Some(pos) = rest.iter().position(|&d| d == die) {
        rest.remove(pos);
    }
    rest
}

/// Сумма очков, использованных последовательностью ходов.
fn pips(seq: &[Move]) -> u8 {
    seq.iter().map(|m| m.die).sum()
}

/// Перебирает все максимальные (нерасширяемые) последовательности ходов.
fn maximal_sequences(
    state: &GameState,
    remaining: &[u8],
    prefix: &mut Vec<Move>,
    out: &mut Vec<Vec<Move>>,
) {
    let mut extended = false;
    let mut tried = Vec::new();
    // «Свежие» пленники — фишки, попавшие в Тюрьму ИМЕННО этим ходом (они уже двигались
    // в этой последовательности). Им разрешён проход дальше костью ≠ 4 (`prison_pass`),
    // чтобы не нарушалось правило максимального хода, когда Тюрьму не обойти. Костью «4»
    // вместо прохода делается «зашёл-вышел» (`resolve`), а фишке из прошлого хода —
    // только выход по «4».
    let fresh: Vec<usize> = (0..state.checkers.len())
        .filter(|&ci| {
            matches!(state.checkers[ci].pos, Position::Prison { .. })
                && prefix.iter().any(|m| m.checker == ci)
        })
        .collect();
    for &die in remaining {
        if tried.contains(&die) {
            continue;
        }
        tried.push(die);
        let mut moves = moves_for_die(state, die);
        for &ci in &fresh {
            if let Some(r) = prison_pass(state, ci, die) {
                moves.push(Move {
                    checker: ci,
                    die,
                    kind: r.kind,
                });
            }
        }
        for mv in moves {
            extended = true;
            let next = apply(state, mv);
            let rest = without(remaining, die);
            prefix.push(mv);
            maximal_sequences(&next, &rest, prefix, out);
            prefix.pop();
        }
    }
    if !extended {
        out.push(prefix.clone());
    }
}

/// Легальные полные ходы за один бросок с учётом правила максимального хода:
/// возвращаются только последовательности, использующие наибольшее число очков.
///
/// Если ни одного хода нет, возвращается один пустой ход (`vec![vec![]]`).
/// Дубль (право на ещё один бросок) обрабатывается на уровне партии, не здесь.
pub fn legal_turns(state: &GameState, roll: DiceRoll) -> Vec<Vec<Move>> {
    let remaining = roll.values().to_vec();
    let mut all = Vec::new();
    maximal_sequences(state, &remaining, &mut Vec::new(), &mut all);
    let best = all.iter().map(|s| pips(s)).max().unwrap_or(0);
    all.into_iter().filter(|s| pips(s) == best).collect()
}

/// Наибольшее число очков, которое можно использовать за данный бросок.
pub fn max_pips(state: &GameState, roll: DiceRoll) -> u8 {
    legal_turns(state, roll).first().map_or(0, |s| pips(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{LOCAL_PRISON_NEAR, Side};
    use crate::dice::Die;
    use crate::state::Checker;

    fn roll(a: u8, b: u8) -> DiceRoll {
        DiceRoll::new(Die::new(a).unwrap(), Die::new(b).unwrap())
    }

    /// Состояние с одним игроком A и заданными положениями фишек.
    fn state_a(positions: &[Position]) -> GameState {
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        for &pos in positions {
            s.checkers.push(Checker {
                owner: Side::A,
                pos,
            });
        }
        s
    }

    #[test]
    fn cannot_pass_through_a_checker() {
        // A на progress 0; своя фишка на progress 2 (промежуточная для хода на 3).
        let s = state_a(&[
            Position::OnTrack { progress: 0 },
            Position::OnTrack { progress: 2 },
        ]);
        // Ход на 3 фишкой 0 проходит через фишку 2 → запрещён.
        assert!(!moves_for_die(&s, 3).iter().any(|m| m.checker == 0));
        // Ход на 1 (без промежуточных) и на 2 (встать рядом/на свою) — допустимы.
        assert!(moves_for_die(&s, 1).iter().any(|m| m.checker == 0));
        // Фишка 2 свободна и может ходить.
        assert!(moves_for_die(&s, 3).iter().any(|m| m.checker == 1));
    }

    #[test]
    fn corner_is_transparent_to_passing() {
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        // A перед углом B (progress 8 = abs 17); ход на 2 проходит через угол abs 18.
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 8 },
        });
        // Чужая фишка стоит на углу — проходить через неё можно.
        let corner = Side::B.start_corner();
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(corner),
            },
        });
        assert!(moves_for_die(&s, 2).iter().any(|m| m.checker == 0));
    }

    #[test]
    fn enter_requires_six() {
        let s = state_a(&[Position::Reserve]);
        assert!(
            moves_for_die(&s, 6)
                .iter()
                .any(|m| m.kind == MoveKind::Enter)
        );
        assert!(moves_for_die(&s, 5).is_empty());
        let after = apply(&s, moves_for_die(&s, 6)[0]);
        assert_eq!(after.checkers[0].pos, Position::OnTrack { progress: 0 });
    }

    #[test]
    fn landing_on_moon_enters_interior() {
        // A на progress 9 (abs 18+? ) — подберём так, чтобы +2 попасть на Луну стороны B.
        // entry A = abs 9; progress 9 → abs 18 (угол B). progress 11 → abs 20 = Луна B.
        let s = state_a(&[Position::OnTrack { progress: 9 }]);
        let mv = moves_for_die(&s, 2);
        assert!(mv.iter().any(|m| m.kind == MoveKind::EnterMoon));
        let after = apply(&s, mv[0]);
        assert_eq!(
            after.checkers[0].pos,
            Position::Moon {
                side: Side::B,
                field: MoonField::One
            }
        );
    }

    #[test]
    fn moon_traverse_and_exit() {
        let mut s = state_a(&[Position::Moon {
            side: Side::B,
            field: MoonField::One,
        }]);
        // 1 → Three
        s = apply(&s, moves_for_die(&s, 1)[0]);
        assert_eq!(
            s.checkers[0].pos,
            Position::Moon {
                side: Side::B,
                field: MoonField::Three
            }
        );
        // неверное значение не двигает
        assert!(moves_for_die(&s, 2).is_empty());
        // 3 → Six
        s = apply(&s, moves_for_die(&s, 3)[0]);
        // 6 → выход на клетку выхода Луны B (abs 34), A-прогресс = 25
        s = apply(&s, moves_for_die(&s, 6)[0]);
        let exit = Side::B.local_to_perimeter(LOCAL_MOON_EXIT);
        assert_eq!(
            s.checkers[0].pos,
            Position::OnTrack {
                progress: Side::A.progress_of(exit)
            }
        );
    }

    #[test]
    fn prison_entry_and_release() {
        // entry A = abs 9; Тюрьма стороны B (ближняя) = abs 18 + 4 = 22; A-прогресс 13.
        let prison_abs = Side::B.local_to_perimeter(LOCAL_PRISON_NEAR);
        let s = state_a(&[Position::OnTrack { progress: 11 }]);
        let mv = moves_for_die(&s, 2);
        assert!(mv.iter().any(|m| m.kind == MoveKind::EnterPrison));
        let jailed = apply(&s, mv[0]);
        assert_eq!(
            jailed.checkers[0].pos,
            Position::Prison { cell: prison_abs }
        );
        // выйти можно только по 4
        assert!(moves_for_die(&jailed, 6).is_empty());
        let freed = apply(&jailed, moves_for_die(&jailed, 4)[0]);
        assert_eq!(
            freed.checkers[0].pos,
            Position::OnTrack {
                progress: Side::A.progress_of(prison_abs)
            }
        );
    }

    #[test]
    fn prison_not_stuck_with_one_checker() {
        // Одна фишка A на progress 11; бросок (2,3): можно перешагнуть Тюрьму (3 → 14,
        // мимо) ИЛИ зайти на неё (2) и пройти насквозь (3 — это не «4»). По правилу
        // максимального хода используются ОБЕ кости, фишка не застревает в Тюрьме.
        let s = state_a(&[Position::OnTrack { progress: 11 }]);
        let turns = legal_turns(&s, roll(2, 3));
        assert!(!turns.is_empty());
        for t in &turns {
            let mut st = s.clone();
            for &m in t {
                st = apply(&st, m);
            }
            assert!(
                !matches!(st.checkers[0].pos, Position::Prison { .. }),
                "при одной фишке нельзя застрять в Тюрьме: {t:?}"
            );
            assert_eq!(pips(t), 5, "обе кости должны играться: {t:?}");
        }
    }

    #[test]
    fn double_passes_through_prison_not_stuck() {
        // Единственная фишка, дубль 5-5, Тюрьма ровно на 5 впереди (progress 8 → 13):
        // обойти нечем, поэтому фишка проходит СКВОЗЬ Тюрьму, тратя ОБЕ пятёрки (правило
        // максимального хода), а не застревает, теряя вторую 5.
        let s = state_a(&[Position::OnTrack { progress: 8 }]);
        let turns = legal_turns(&s, roll(5, 5));
        assert_eq!(max_pips(&s, roll(5, 5)), 10, "должны играться обе пятёрки");
        assert!(!turns.is_empty());
        for t in &turns {
            let mut st = s.clone();
            for &m in t {
                st = apply(&st, m);
            }
            assert!(
                !matches!(st.checkers[0].pos, Position::Prison { .. }),
                "нельзя застрять в Тюрьме при дубле: {t:?}"
            );
        }
    }

    #[test]
    fn prison_entry_then_only_release() {
        // Бросок (2,4), одна фишка A (progress 11). Выбор делается ПЕРВЫМ ходом:
        //  • зайти в Тюрьму (13) и «зашёл-вышел» по «4» — остаться на клетке 13; либо
        //  • перешагнуть Тюрьму (4 → 15, мимо) и пойти дальше (2 → 17).
        // Зайдя в Тюрьму, второй костью пройти дальше уже НЕЛЬЗЯ — только освобождение.
        let s = state_a(&[Position::OnTrack { progress: 11 }]);
        let turns = legal_turns(&s, roll(2, 4));
        let finals: Vec<Position> = turns
            .iter()
            .map(|t| {
                let mut st = s.clone();
                for &m in t {
                    st = apply(&st, m);
                }
                st.checkers[0].pos
            })
            .collect();
        // «зашёл-вышел»: свободна на клетке Тюрьмы (13).
        assert!(finals.contains(&Position::OnTrack { progress: 13 }));
        // перешагнула Тюрьму мимо и ушла дальше (не в Тюрьме и не на клетке 13).
        assert!(
            finals.iter().any(|p| !matches!(p, Position::Prison { .. })
                && *p != Position::OnTrack { progress: 13 })
        );
        // Войдя в Тюрьму, дальше можно ТОЛЬКО освободиться (PrisonRelease).
        for t in &turns {
            if t.first().is_some_and(|m| m.kind == MoveKind::EnterPrison) {
                assert!(t[1..].iter().all(|m| m.kind == MoveKind::PrisonRelease));
            }
        }
    }

    #[test]
    fn cannot_enter_onto_own_checker() {
        // Своя фишка стоит на клетке ввода (progress 0) — вторую из резерва не ввести.
        let s = state_a(&[Position::Reserve, Position::OnTrack { progress: 0 }]);
        let mv = moves_for_die(&s, 6);
        assert!(
            !mv.iter()
                .any(|m| m.checker == 0 && m.kind == MoveKind::Enter),
            "нельзя вводить на свою фишку"
        );
    }

    #[test]
    fn enter_eats_enemy_on_entry() {
        // Чужая фишка на клетке ввода A → ввод разрешён и съедает её.
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::Reserve,
        });
        let entry = Side::A.entry();
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(entry),
            },
        });
        let mv = moves_for_die(&s, 6);
        let enter = *mv
            .iter()
            .find(|m| m.checker == 0 && m.kind == MoveKind::Enter)
            .expect("ввод (со съеданием) разрешён");
        let after = apply(&s, enter);
        assert_eq!(after.checkers[0].pos, Position::OnTrack { progress: 0 });
        assert!(matches!(
            after.checkers[1].pos,
            Position::Captured { captor: Side::A }
        ));
    }

    #[test]
    fn home_entry_no_jump_over_occupied() {
        // Своя фишка в Доме на глубине 0 → заход на глубину 2 (через занятую 0) нельзя.
        let s = state_a(&[
            Position::OnTrack { progress: 70 },
            Position::Home { depth: 0 },
        ]);
        assert!(
            !moves_for_die(&s, 4)
                .iter()
                .any(|m| m.checker == 0 && m.kind == MoveKind::EnterHome),
            "нельзя перепрыгивать занятую клетку Дома"
        );
        // Если путь по Дому свободен — заход на глубину 2 разрешён.
        let s2 = state_a(&[Position::OnTrack { progress: 70 }]);
        assert!(
            moves_for_die(&s2, 4)
                .iter()
                .any(|m| m.checker == 0 && m.kind == MoveKind::EnterHome)
        );
    }

    #[test]
    fn home_advance_deeper_and_no_jump() {
        // Фишка в Доме на глубине 0 может идти глубже (освобождая вход).
        let s = state_a(&[Position::Home { depth: 0 }]);
        let after = apply(&s, moves_for_die(&s, 2)[0]);
        assert_eq!(after.checkers[0].pos, Position::Home { depth: 2 });
        // Но не за пределы Дома (перебор) и не перепрыгивая занятую клетку.
        let s2 = state_a(&[Position::Home { depth: 0 }, Position::Home { depth: 1 }]);
        assert!(
            !moves_for_die(&s2, 3)
                .iter()
                .any(|m| m.checker == 0 && m.kind == MoveKind::HomeAdvance),
            "нельзя перепрыгнуть занятую клетку Дома"
        );
        assert!(moves_for_die(&s, 5).iter().all(|m| m.checker != 0)); // depth 0+5 > 3 — перебор
    }

    #[test]
    fn capture_sends_enemy_to_captivity() {
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        // A на progress 0 (abs 9). Цель шага на 3 → abs 12 (обычная клетка).
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 0 },
        });
        // C стоит на abs 12.
        let target = PerimeterIdx::new(12);
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(target),
            },
        });
        let mv = moves_for_die(&s, 3);
        let after = apply(&s, mv[0]);
        assert_eq!(after.checkers[0].pos, Position::OnTrack { progress: 3 });
        assert_eq!(
            after.checkers[1].pos,
            Position::Captured { captor: Side::A }
        );
    }

    #[test]
    fn allies_do_not_capture_each_other() {
        // Командный режим 2×2: A и C — союзники. A приходит на клетку с фишкой C —
        // съедания нет, обе фишки сосуществуют.
        let mut s = GameState::new(Side::ALL.to_vec(), Side::A).with_teams(true);
        s.checkers.clear();
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 0 },
        });
        let target = PerimeterIdx::new(12);
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(target),
            },
        });
        let mv = moves_for_die(&s, 3);
        let after = apply(&s, mv[0]);
        assert_eq!(after.checkers[0].pos, Position::OnTrack { progress: 3 });
        // Фишка союзника C осталась на месте (не пленена).
        assert!(matches!(after.checkers[1].pos, Position::OnTrack { .. }));
        // А вне командного режима тот же расклад — съедание (контроль).
        let plain = s.with_teams(false);
        let after2 = apply(&plain, moves_for_die(&plain, 3)[0]);
        assert_eq!(
            after2.checkers[1].pos,
            Position::Captured { captor: Side::A }
        );
    }

    #[test]
    fn no_capture_on_corner() {
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        // A progress 9 → abs 18 (угол B).
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 0 },
        });
        let corner = Side::B.start_corner();
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(corner),
            },
        });
        let after = apply(&s, moves_for_die(&s, 9)[0]);
        // Соперник на углу не съеден.
        assert!(matches!(after.checkers[1].pos, Position::OnTrack { .. }));
    }

    #[test]
    fn enter_home_exact_and_occupancy() {
        // progress 70 + 2 = 72 → Home depth 0.
        let s = state_a(&[Position::OnTrack { progress: 70 }]);
        let mv = moves_for_die(&s, 2);
        assert!(mv.iter().any(|m| m.kind == MoveKind::EnterHome));
        assert_eq!(
            apply(&s, mv[0]).checkers[0].pos,
            Position::Home { depth: 0 }
        );
        // Перебор (progress 71 + 6 = 77 → глубина 5) запрещён.
        let s2 = state_a(&[Position::OnTrack { progress: 71 }]);
        assert!(moves_for_die(&s2, 6).is_empty());
        // Занятая клетка Дома блокирует заход на ту же глубину.
        let s3 = state_a(&[
            Position::Home { depth: 0 },
            Position::OnTrack { progress: 70 },
        ]);
        assert!(
            !moves_for_die(&s3, 2)
                .iter()
                .any(|m| m.kind == MoveKind::EnterHome)
        );
    }

    #[test]
    fn max_move_rule_prefers_more_pips() {
        // Один резерв: бросок (6,3) → ввод по 6, затем шаг 3. Должны использоваться обе.
        let s = state_a(&[Position::Reserve]);
        let turns = legal_turns(&s, roll(6, 3));
        assert_eq!(max_pips(&s, roll(6, 3)), 9);
        assert!(turns.iter().all(|t| pips(t) == 9));
        assert!(turns.iter().all(|t| t.len() == 2));

        // Бросок (5,4) без шестёрки: ввести нельзя, ходов нет.
        assert_eq!(max_pips(&s, roll(5, 4)), 0);
        assert_eq!(legal_turns(&s, roll(5, 4)), vec![vec![]]);
    }

    #[test]
    fn ransom_frees_captive_on_six() {
        let s = state_a(&[Position::Captured { captor: Side::C }]);
        // Выкуп только по «6».
        assert!(moves_for_die(&s, 5).is_empty());
        let mv = moves_for_die(&s, 6);
        assert!(mv.iter().any(|m| m.kind == MoveKind::Ransom));
        let after = apply(&s, mv[0]);
        assert_eq!(after.checkers[0].pos, Position::Reserve);
    }

    #[test]
    fn forced_six_moves_lists_captor_options() {
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack { progress: 0 },
        });
        let forced = forced_six_moves(&s, Side::C);
        assert_eq!(forced.len(), 1);
        assert_eq!(forced[0].die, 6);
    }

    #[test]
    fn max_move_uses_only_playable_die() {
        // progress 70: «6» даёт перебор (нелегально), «2» заводит в Дом.
        let s = state_a(&[Position::OnTrack { progress: 70 }]);
        let turns = legal_turns(&s, roll(6, 2));
        assert_eq!(turns.len(), 1);
        assert_eq!(pips(&turns[0]), 2);
        assert_eq!(turns[0][0].kind, MoveKind::EnterHome);
    }
    #[test]
    fn ransom_counts_toward_max_move() {
        // A: пленник + фишка на progress 70. При 6-2 этой фишкой играется только «2»
        // (заход в Дом; «6» — перебор за пределы Дома). Но выкуп пленника тратит «6»,
        // поэтому правило максимального хода ОБЯЗЫВАЕТ выкупить (6) и сходить на 2 —
        // суммарно 8 очков, а не просто 2.
        let s = state_a(&[
            Position::Captured { captor: Side::C },
            Position::OnTrack { progress: 70 },
        ]);
        assert_eq!(max_pips(&s, roll(6, 2)), 8);
        let turns = legal_turns(&s, roll(6, 2));
        assert!(turns.iter().all(|t| {
            t.iter().any(|m| m.kind == MoveKind::Ransom)
                && t.iter().any(|m| m.kind == MoveKind::EnterHome)
        }));
    }
}
