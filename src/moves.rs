//! Движок ходов: генерация легальных ходов, их применение и правило
//! максимального хода.
//!
//! `Move` продвигает **одну** фишку на `pips` — это либо значение **одной** кости,
//! либо **сумма** нескольких костей, объединённых на эту фишку «в один мах»
//! (`a + b`). При объединённом движении эффект имеет только **конечная** клетка:
//! спец-клетки (Луна/Тюрьма), которые фишка лишь **перешагивает** по пути, не
//! срабатывают — вот почему отдельный «проход сквозь Тюрьму» не нужен. Чтобы зайти
//! на спец-клетку (сесть в Тюрьму, улететь на Луну), её надо сделать **конечной**
//! клеткой движения (одиночной костью или объединением). Все варианты — одиночные
//! ходы и объединения — перебирает `legal_turns` с правилом максимального хода.
//!
//! Объединять очки можно только при ходе по периметру (`OnTrack`). Спец-состояния
//! (резерв/Луна/Тюрьма/плен/Дом) требуют **конкретного значения на одной кости**
//! (6 — ввод/выкуп, 4 — выход из Тюрьмы, 1/3/6 — Луна), объединение к ним неприменимо.
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
    /// Заход в Дом (ровно, по 1 фишке в клетку).
    EnterHome,
    /// Продвижение вглубь Дома (фишка уже в Доме идёт на более глубокую клетку,
    /// освобождая место; перепрыгивать занятые клетки нельзя).
    HomeAdvance,
    /// Выкуп своей пленной фишки (по «6»): она уходит в резерв. Влечёт
    /// обязательный ответный ход захватчика на 6 (обрабатывается в туровом цикле).
    Ransom,
}

/// Один ход: фишка `checker` продвигается на `pips` клеток. `pips` — это значение
/// одной кости либо сумма нескольких костей, объединённых на эту фишку (только при
/// ходе по периметру; см. модульный комментарий).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    /// Индекс фишки в `GameState::checkers`.
    pub checker: usize,
    /// Число клеток продвижения (значение одной кости или сумма объединённых).
    pub pips: u8,
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

/// Стоит ли на клетке периметра `cell` СВОЯ фишка (та же сторона). Нельзя встать
/// второй своей фишкой на обычную клетку — исключения только угол и Тюрьма.
fn own_on(state: &GameState, owner: Side, cell: PerimeterIdx) -> bool {
    state
        .checkers
        .iter()
        .any(|c| c.owner == owner && c.pos.perimeter_cell(c.owner) == Some(cell))
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
    // Заход в Дом пересекает клетку ВХОДА (ворота, прогресс 0) и поворачивает внутрь;
    // эта клетка — тоже промежуточная, её нельзя перепрыгивать (ворота — не угол).
    // Без проверки чужая фишка, стоящая на клетке входа, «перепрыгивалась».
    if target >= PERIMETER as u16 && occupied(state, owner.entry()) {
        return false;
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

/// Разбирает продвижение фишки `ci` на `pips` клеток. Диспетчер по положению:
/// фишка на периметре (`OnTrack`) может идти на сумму нескольких костей (`pips`),
/// перешагивая спец-клетки; фишка в спец-состоянии — только на значение **одной**
/// кости (`pips` тогда = это значение). `None` — ход нелегален.
fn resolve_move(state: &GameState, ci: usize, pips: u8) -> Option<Resolved> {
    match state.checkers[ci].pos {
        Position::OnTrack { .. } => advance_on_track(state, ci, pips),
        _ => resolve_special(state, ci, pips),
    }
}

/// Продвижение фишки по периметру на `pips` клеток (значение одной кости или сумма
/// объединённых). Эффект — только у **конечной** клетки: спец-клетки на пути просто
/// перешагиваются (они либо пусты, как клетка-вход Луны, либо — занятая Тюрьма —
/// блокируют проход как обычная фишка). Чтобы **зайти** на спец-клетку, она должна
/// быть конечной.
fn advance_on_track(state: &GameState, ci: usize, pips: u8) -> Option<Resolved> {
    if pips == 0 {
        return None;
    }
    let ch = state.checkers[ci];
    let owner = ch.owner;
    let Position::OnTrack { progress } = ch.pos else {
        return None;
    };
    let target = progress + pips as u16;
    // Нельзя проходить через чужие/свои фишки (кроме стоящих на углу). Промежуточная
    // клетка Тюрьмы, если на ней сидит пленник, блокирует — это и есть блок Тюрьмой.
    if !path_clear(state, owner, progress, pips) {
        return None;
    }
    if (target as usize) < PERIMETER {
        let abs = owner.entry().advance(target as usize);
        let kind = cell_kind(abs);
        // Нельзя встать ВТОРОЙ своей фишкой на одну клетку. Исключения: угол
        // (там фишки соседствуют) и Тюрьма (вход/«зашёл-вышел»). Союзники —
        // не «свои», им можно сосуществовать (это ловит `own_on` по стороне).
        if matches!(kind, CellKind::Plain | CellKind::HomeEntrance) && own_on(state, owner, abs) {
            return None;
        }
        let (new_pos, captures, kind) = match kind {
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
        if depth < HOME_DEPTH && !home_depth_taken(state, owner, depth as u8) && home_path_clear {
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

/// Ход фишки из спец-состояния (резерв/Луна/Тюрьма/плен/Дом) **одной** костью `die`.
/// Объединение очков сюда неприменимо: каждое из этих состояний требует конкретного
/// значения на одной кости.
fn resolve_special(state: &GameState, ci: usize, die: u8) -> Option<Resolved> {
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
        Position::OnTrack { .. } => None,
    }
}

/// Может ли сторона `side` сыграть хоть один ход костью `die` (любой своей фишкой).
/// Признак «гибкости» для обучаемой оценки: чем больше значений костей удаётся
/// использовать, тем эффективнее ходы (см. `encode`).
pub fn can_play(state: &GameState, side: Side, die: u8) -> bool {
    state
        .checkers
        .iter()
        .enumerate()
        .any(|(ci, c)| c.owner == side && resolve_move(state, ci, die).is_some())
}

/// Легален ли конкретный ход в данном состоянии (без паники, в отличие от `apply`).
/// Нужно туровому циклу: ответный ход захватчика при выкупе может сделать
/// нелегальным следующий ход выбранной последовательности — такой ход пропускается.
pub fn move_legal(state: &GameState, mv: Move) -> bool {
    resolve_move(state, mv.checker, mv.pips).is_some()
}

/// Все легальные одиночные ходы стороны `side` костью `die` (одно её значение).
fn moves_for_side(state: &GameState, side: Side, die: u8) -> Vec<Move> {
    state
        .checkers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.owner == side)
        .filter_map(|(ci, _)| {
            resolve_move(state, ci, die).map(|r| Move {
                checker: ci,
                pips: die,
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
    let resolved = resolve_move(state, mv.checker, mv.pips).expect("apply: нелегальный ход");
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

/// Убирает из набора костей по одному экземпляру каждого значения из `consumed`.
fn without(remaining: &[u8], consumed: &[u8]) -> Vec<u8> {
    let mut rest = remaining.to_vec();
    for &die in consumed {
        if let Some(pos) = rest.iter().position(|&d| d == die) {
            rest.remove(pos);
        }
    }
    rest
}

/// Сумма очков, использованных последовательностью ходов.
fn pips(seq: &[Move]) -> u8 {
    seq.iter().map(|m| m.pips).sum()
}

/// Кандидаты одного «маха» из набора костей `remaining`: каждое — `(pips, costs)`,
/// где `pips` — на сколько клеток продвинуться, а `costs` — какие кости это тратит.
/// Помимо одиночных значений сюда входит **объединение** костей на одну фишку (сумма
/// всех костей `remaining`): фишка проходит этот путь «в один мах», и промежуточные
/// спец-клетки не срабатывают. Объединения применимы только к ходу по периметру
/// (см. `moves_for_motion`). Дубль за один ход — это 2 кости (право на ещё один ход
/// обрабатывается перебросом в туровом цикле), поэтому объединение тут всегда `a + b`.
fn motion_candidates(remaining: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
    // Одиночные значения (по одному на каждое различное значение кости).
    let mut singles: Vec<u8> = remaining.to_vec();
    singles.sort_unstable();
    singles.dedup();
    for &d in &singles {
        out.push((d, vec![d]));
    }
    // Объединение всех оставшихся костей в один мах (сумму). Для разных значений это
    // a+b; для одинаковых (дубль) — 2d. Цикл общий — на случай >2 костей он дал бы и
    // частичные суммы 2d..kd, но за один ход костей всегда ровно две.
    if remaining.len() >= 2 {
        if remaining.iter().all(|&d| d == remaining[0]) {
            let d = remaining[0];
            for k in 2..=remaining.len() {
                out.push((d * k as u8, vec![d; k]));
            }
        } else {
            let sum: u8 = remaining.iter().sum();
            out.push((sum, remaining.to_vec()));
        }
    }
    out
}

/// Ходы текущего игрока, продвигающие одну фишку на `pips` ценой костей `costs`.
/// Объединение нескольких костей (`costs.len() > 1`) допустимо только для фишек на
/// периметре; спец-состояниям нужна ровно одна кость нужного значения.
fn moves_for_motion(state: &GameState, pips: u8, combined: bool) -> Vec<Move> {
    state
        .checkers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.owner == state.to_move)
        .filter_map(|(ci, c)| {
            if combined && !matches!(c.pos, Position::OnTrack { .. }) {
                return None;
            }
            resolve_move(state, ci, pips).map(|r| Move {
                checker: ci,
                pips,
                kind: r.kind,
            })
        })
        .collect()
}

/// Перебирает все максимальные (нерасширяемые) последовательности ходов. Каждый шаг —
/// это «мах» одной фишки на значение одной кости **или** на сумму объединённых костей
/// (последнее — только для фишек на периметре, см. `motion_candidates`).
fn maximal_sequences(
    state: &GameState,
    remaining: &[u8],
    prefix: &mut Vec<Move>,
    out: &mut Vec<Vec<Move>>,
) {
    let mut extended = false;
    for (pips, costs) in motion_candidates(remaining) {
        for mv in moves_for_motion(state, pips, costs.len() > 1) {
            extended = true;
            let next = apply(state, mv);
            let rest = without(remaining, &costs);
            prefix.push(mv);
            maximal_sequences(&next, &rest, prefix, out);
            prefix.pop();
        }
    }
    if !extended {
        out.push(prefix.clone());
    }
}

/// Легальные полные ходы из набора **оставшихся** костей `remaining` с учётом правила
/// максимального хода: только последовательности, использующие наибольшее число очков.
/// Если ходов нет — один пустой ход (`vec![vec![]]`). Нужно для **по-шагового** хода
/// (после выкупа остаток костей переспрашивается у текущего состояния — см. `play_turn`).
pub fn legal_turns_remaining(state: &GameState, remaining: &[u8]) -> Vec<Vec<Move>> {
    let mut all = Vec::new();
    maximal_sequences(state, remaining, &mut Vec::new(), &mut all);
    let best = all.iter().map(|s| pips(s)).max().unwrap_or(0);
    all.into_iter().filter(|s| pips(s) == best).collect()
}

/// Легальные полные ходы за один бросок (обе кости) — частный случай `legal_turns_remaining`.
///
/// Если ни одного хода нет, возвращается один пустой ход (`vec![vec![]]`).
/// Дубль (право на ещё один бросок) обрабатывается на уровне партии, не здесь.
pub fn legal_turns(state: &GameState, roll: DiceRoll) -> Vec<Vec<Move>> {
    legal_turns_remaining(state, &roll.values())
}

/// Оставшиеся кости после хода на `pips` очков: убирает подмножество костей, сумма
/// которого равна `pips`. Для бросков игры (две кости либо две одинаковых) такое
/// подмножество единственно — либо одна кость со значением `pips`, либо несколько,
/// объединённых «в один мах». Нужно по-шаговому ходу для пересчёта остатка.
pub fn remaining_after(remaining: &[u8], pips: u8) -> Vec<u8> {
    if let Some(i) = remaining.iter().position(|&d| d == pips) {
        let mut r = remaining.to_vec();
        r.remove(i);
        return r;
    }
    let mut need = pips;
    remaining
        .iter()
        .copied()
        .filter(|&d| {
            if need >= d {
                need -= d;
                false // тратится этим объединённым ходом
            } else {
                true
            }
        })
        .collect()
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
    fn moon_advances_twice_in_one_turn() {
        // Сдвоенный ход по Луне за один бросок: с поля «1» бросок 1-3 ведёт сразу на «6»
        // ([MoonAdvance(1), MoonAdvance(3)]); с поля «3» бросок 3-6 — сразу выход
        // ([MoonAdvance(3), MoonExit(6)]). Раньше GUI не показывал такую связку.
        let s = state_a(&[Position::Moon {
            side: Side::B,
            field: MoonField::One,
        }]);
        let turns = legal_turns(&s, roll(1, 3));
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].iter().map(|m| m.kind).collect::<Vec<_>>(),
            vec![MoveKind::MoonAdvance, MoveKind::MoonAdvance]
        );
        assert!(turns[0].iter().all(|m| m.checker == 0)); // одна и та же фишка → связка

        let s3 = state_a(&[Position::Moon {
            side: Side::B,
            field: MoonField::Three,
        }]);
        let turns = legal_turns(&s3, roll(3, 6));
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].iter().map(|m| m.kind).collect::<Vec<_>>(),
            vec![MoveKind::MoonAdvance, MoveKind::MoonExit]
        );
    }

    #[test]
    fn cannot_stack_two_own_on_plain_cell() {
        // Своя фишка A на progress 12 (обычная клетка). Второй своей фишкой (с 10) на
        // неё встать НЕЛЬЗЯ; на пустую клетку 14 — можно.
        assert_eq!(
            cell_kind(Side::A.entry().advance(12)),
            CellKind::Plain,
            "тест полагается на обычную клетку"
        );
        let s = state_a(&[
            Position::OnTrack { progress: 12 },
            Position::OnTrack { progress: 10 },
        ]);
        let mv = moves_for_die(&s, 2);
        assert!(
            !mv.iter().any(|m| m.checker == 1),
            "нельзя встать второй своей фишкой на занятую своей клетку"
        );
        assert!(mv.iter().any(|m| m.checker == 0), "на пустую 14 — можно");
        // Если клетка 12 пуста — на неё встать можно.
        let s2 = state_a(&[Position::OnTrack { progress: 10 }]);
        assert!(moves_for_die(&s2, 2).iter().any(|m| m.checker == 0));
    }

    #[test]
    fn cannot_pass_through_own_on_plain() {
        // Своя фишка A на progress 15 (обычная клетка) — пройти СКВОЗЬ неё нельзя:
        // ход с 14 на 16 (промежуточная 15) нелегален. (Через занятый угол — можно,
        // это покрыто `path_clear`/углами отдельно.)
        assert_eq!(cell_kind(Side::A.entry().advance(15)), CellKind::Plain);
        let s = state_a(&[
            Position::OnTrack { progress: 15 },
            Position::OnTrack { progress: 14 },
        ]);
        assert!(
            !moves_for_die(&s, 2).iter().any(|m| m.checker == 1),
            "нельзя проходить сквозь свою фишку на обычной клетке"
        );
        // Без своей фишки на 15 ход 14→16 легален.
        let s2 = state_a(&[Position::OnTrack { progress: 14 }]);
        assert!(moves_for_die(&s2, 2).iter().any(|m| m.checker == 0));
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
        // Одна фишка A на progress 11; бросок (2,3): объединив кости (2+3=5 «в один мах»)
        // или перешагнув Тюрьму одиночной костью, фишка проходит МИМО клетки Тюрьмы (13).
        // По правилу максимального хода играются ОБЕ кости, фишка не застревает в Тюрьме.
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
        // одиночная 5 встаёт на Тюрьму, но объединение костей (5+5=10 «в один мах»)
        // перешагивает её, тратя ОБЕ пятёрки (правило максимального хода), — фишка не
        // застревает, теряя вторую 5.
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
    fn double_passes_through_moon_not_stuck() {
        // Одна фишка A (progress 6); Луна стороны B — на A-progress 11. Бросок 5-5:
        // одиночная 5 (6→11) встаёт на Луну и улетает внутрь, но объединение 5+5=10
        // перешагивает её (6→16). Правило максимума выбирает перелёт (10 > 5) — фишка
        // НЕ застревает на Луне (та же симметрия, что и с Тюрьмой).
        let s = state_a(&[Position::OnTrack { progress: 6 }]);
        assert_eq!(max_pips(&s, roll(5, 5)), 10, "должны играться обе пятёрки");
        for t in legal_turns(&s, roll(5, 5)) {
            let mut st = s.clone();
            for &m in &t {
                st = apply(&st, m);
            }
            assert!(
                !matches!(st.checkers[0].pos, Position::Moon { .. }),
                "нельзя застрять на Луне при дубле: {t:?}"
            );
        }
    }

    #[test]
    fn single_die_enters_moon_when_it_is_the_landing() {
        // Та же фишка перед Луной, но ход на 5 одной костью (6→11) — Луна КОНЕЧНАЯ
        // клетка, поэтому фишка туда заходит (объединять не с чем).
        let s = state_a(&[Position::OnTrack { progress: 6 }]);
        let mv = moves_for_die(&s, 5);
        assert!(mv.iter().any(|m| m.kind == MoveKind::EnterMoon));
    }

    #[test]
    fn may_enter_special_with_one_die_and_move_another() {
        // Фишка 0 (progress 11) может ЗАЙТИ в Тюрьму (13) костью «2», а фишка 1
        // (progress 0) — сходить костью «3». Это легально и соблюдает максимум (обе
        // кости сыграны, 5 очков): «встать одной, сходить другой».
        let s = state_a(&[
            Position::OnTrack { progress: 11 },
            Position::OnTrack { progress: 0 },
        ]);
        let turns = legal_turns(&s, roll(2, 3));
        assert!(
            turns.iter().any(|t| {
                pips(t) == 5 && {
                    let mut st = s.clone();
                    for &m in t {
                        st = apply(&st, m);
                    }
                    matches!(st.checkers[0].pos, Position::Prison { .. })
                        && matches!(st.checkers[1].pos, Position::OnTrack { progress: 3 })
                }
            }),
            "должна быть возможность сесть в Тюрьму одной фишкой и сходить другой"
        );
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
    fn cannot_jump_enemy_on_home_entrance() {
        // Чужая фишка (C) стоит на клетке ВХОДА в Дом игрока A. Фишка A перед воротами
        // (progress 70) не может зайти в Дом, «перепрыгнув» её — ворота тоже промежуточная.
        let entry = Side::A.entry();
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.checkers.clear();
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 70 },
        });
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(entry),
            },
        });
        // Ход на 4 (70 → глубина 2) пересекает занятую клетку входа → нелегален.
        assert!(
            !moves_for_die(&s, 4)
                .iter()
                .any(|m| m.checker == 0 && m.kind == MoveKind::EnterHome),
            "нельзя зайти в Дом через занятую чужой фишкой клетку входа"
        );
        // Уберём чужую фишку с ворот (в резерв) — заход снова разрешён.
        s.checkers[1].pos = Position::Reserve;
        assert!(
            moves_for_die(&s, 4)
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
        assert_eq!(forced[0].pips, 6);
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
