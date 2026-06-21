//! Leptos-фронтенд Шеш-Беш: вся игровая логика — движок `sheshbesh` в WASM.
//! Доска — векторная (SVG); фишки рисуются отдельным слоем keyed-кружков, что даёт
//! плавную анимацию перемещения (CSS-переход `cx`/`cy`). Перед партией — экран
//! настроек (2/3/4 игрока, тип каждой стороны: человек/компьютер). Ход человека
//! собирается кликами (фишка → клетка) по одной кости; компьютерные стороны ходят
//! сами (эвристика).

use gloo_timers::future::TimeoutFuture;
use leptos::prelude::*;
use sheshbesh::board::{
    LOCAL_MOON, LOCAL_MOON_EXIT, LOCAL_PRISON_FAR, LOCAL_PRISON_NEAR, PERIMETER, cell_kind,
};
use sheshbesh::{
    Agent, BOARD_DIM, BOARD_MARGIN, CellKind, DiceRoll, DiceSource, Game, GameState, Heuristic,
    MoonField, Move, MoveKind, PerimeterIdx, Position, RandomDice, Side, apply, checker_cell,
    legal_turns, margin_coord,
};
use wasm_bindgen_futures::spawn_local;

/// Пауза-заставка перед первым ходом партии (мс) — чтобы старт не мелькал.
const INTRO_MS: u32 = 1800;
/// Пауза кадра броска: 3D-кувырок длится до ~2.2с (см. `die3d`) + запас, мс.
const ROLL_ANIM_MS: u32 = 2400;
/// Пауза на кадр «результат броска» — кости остановились (мс).
const HOLD_ROLL_MS: u32 = 1100;
/// Пауза, когда ходов нет: дольше показываем бросок, прежде чем отдать ход.
const HOLD_NOMOVE_MS: u32 = 1800;
/// Пауза на один шаг фишки по клетке, мс.
const HOLD_STEP_MS: u32 = 760;

/// Зерно ГПСЧ из времени браузера (на wasm `SystemTime` недоступен).
fn seed() -> u64 {
    (js_sys::Date::now() as u64) ^ 0x9E37_79B9_7F4A_7C15
}

/// Что выбрано как источник/цель: клетка доски, лоток резерва или плена.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sel {
    Cell(usize, usize),
    Reserve,
    Captured,
}

fn side_color(s: Side) -> &'static str {
    match s {
        Side::A => "#22d3ee",
        Side::B => "#4ade80",
        Side::C => "#e879f9",
        Side::D => "#facc15",
    }
}

// --- Логика ходов ---

/// Один кадр анимации: показываемое состояние доски, (опц.) выпавшие кости, пауза
/// перед его показом (мс), флаг «кости крутятся» и (опц.) реплика комментатора.
#[derive(Clone)]
struct Frame {
    state: GameState,
    roll: Option<DiceRoll>,
    hold: u32,
    rolling: bool,
    note: Option<String>,
}

/// Имя стороны для комментатора: «Игрок L» (человек) или «ИИ L» (компьютер).
fn side_name(side: Side, humans: &[Side]) -> String {
    let who = if humans.contains(&side) {
        "Игрок"
    } else {
        "ИИ"
    };
    format!("{who} {}", side.letter())
}

/// Реплика об одном применённом ходе (съедание/Дом/Луна/Тюрьма/выкуп). Для
/// обычного шага по периметру реплики нет (`None`) — чтобы не засорять ленту.
fn move_note(before: &GameState, after: &GameState, mv: Move, humans: &[Side]) -> Option<String> {
    let owner = before.checkers[mv.checker].owner;
    let name = side_name(owner, humans);
    if mv.kind == MoveKind::Ransom {
        return Some(format!("{name}: выкуп пленной фишки."));
    }
    // Съедание: чья-то фишка стала пленённой именно этим ходом.
    let victim = after.checkers.iter().enumerate().find(|(j, c)| {
        matches!(c.pos, Position::Captured { .. })
            && !matches!(before.checkers[*j].pos, Position::Captured { .. })
    });
    if let Some((j, _)) = victim {
        let v = side_name(before.checkers[j].owner, humans);
        return Some(format!("{name} съедает фишку: {v}!"));
    }
    let event = match mv.kind {
        MoveKind::Enter => "ввод фишки в игру.",
        MoveKind::EnterMoon => "фишка взлетает на Луну!",
        MoveKind::MoonAdvance => "продвижение по дорожке Луны.",
        MoveKind::MoonExit => "сход с Луны.",
        MoveKind::EnterPrison => "фишка угодила в Тюрьму!",
        MoveKind::PrisonRelease => "выход из Тюрьмы.",
        MoveKind::EnterHome => "заход фишки в Дом!",
        MoveKind::HomeAdvance => "фишка идёт вглубь Дома.",
        MoveKind::PrisonPass => "фишка минует Тюрьму.",
        MoveKind::Step | MoveKind::Ransom => return None,
    };
    Some(format!("{name}: {event}"))
}

/// Реплика о броске: что выпало, дубль и отсутствие ходов.
fn roll_note(side: Side, humans: &[Side], roll: DiceRoll, no_move: bool) -> String {
    let [a, b] = roll.values();
    let mut s = format!("{}: выпало {a} и {b}.", side_name(side, humans));
    if roll.is_double() {
        s.push_str(" Дубль — ещё ход!");
    }
    if no_move {
        s.push_str(" Ходить нечем.");
    }
    s
}

/// Реплика в конце серии кадров: только победа. Для перехода к ходу игрока реплики
/// нет (`None`) — её сразу же сменит сообщение о его броске.
fn end_note(game: &Game, humans: &[Side]) -> Option<String> {
    game.winner()
        .map(|w| format!("{} победил! Игра окончена.", side_name(w, humans)))
}

/// Применяет ход `mv` к `state`, попутно добавляя кадры. Если фишка идёт по дорожке,
/// эмитится по кадру на **каждую промежуточную клетку** (фишка «шагает», а не
/// перепрыгивает сразу на конечную). Возвращает состояние после хода.
fn apply_with_frames(
    frames: &mut Vec<Frame>,
    state: GameState,
    mv: Move,
    roll: DiceRoll,
    humans: &[Side],
) -> GameState {
    // Шагаем по клеткам. Стартовый «индекс дорожки» (0..PERIMETER — периметр,
    // PERIMETER..+HOME — клетки Дома вглубь): обычный ход / проход сквозь Тюрьму
    // (от клетки Тюрьмы) / продвижение вглубь Дома (от своей клетки Дома).
    let owner = state.checkers[mv.checker].owner;
    let start = match state.checkers[mv.checker].pos {
        Position::OnTrack { progress } => Some(progress),
        Position::Prison { cell } if mv.kind == MoveKind::PrisonPass => {
            Some(owner.progress_of(cell))
        }
        Position::Home { depth } if mv.kind == MoveKind::HomeAdvance => {
            Some(PERIMETER as u16 + u16::from(depth))
        }
        _ => None,
    };
    if let Some(sp) = start {
        for step in 1..mv.die {
            let idx = sp + u16::from(step);
            // Промежуточная клетка: периметр или клетка Дома (а не «второй круг»).
            let pos = if (idx as usize) < PERIMETER {
                Position::OnTrack { progress: idx }
            } else {
                Position::Home {
                    depth: (idx - PERIMETER as u16) as u8,
                }
            };
            let mut mid = state.clone();
            mid.checkers[mv.checker].pos = pos;
            frames.push(Frame {
                state: mid,
                roll: Some(roll),
                hold: HOLD_STEP_MS,
                rolling: false,
                note: None,
            });
        }
    }
    let after = apply(&state, mv);
    let note = move_note(&state, &after, mv, humans);
    frames.push(Frame {
        state: after.clone(),
        roll: Some(roll),
        hold: HOLD_STEP_MS,
        rolling: false,
        note,
    });
    after
}

/// Разворачивает один ход (бросок + выбранная последовательность) в кадры: сперва
/// «кости брошены», затем по фишке за раз (с пошаговым проходом по клеткам), включая
/// вынужденные ответные ходы захватчиков при выкупе. Продвигает `game`.
fn commit_frames<F>(
    frames: &mut Vec<Frame>,
    game: &mut Game,
    roll: DiceRoll,
    played: Vec<Move>,
    humans: &[Side],
    roll_anim: bool,
    forced: F,
) where
    F: FnMut(&GameState, Side, &[Move]) -> usize,
{
    let side = game.state.to_move;
    let no_move = played.is_empty();
    // Анимация броска нужна, когда кости ещё не показаны (ход ИИ). У игрока-человека
    // бросок уже анимирован в начале его хода — тут только продвигаем фишки.
    if roll_anim {
        // Кадр броска: кубики кувыркаются в 3D (анимация в статусе) и встают на грань.
        frames.push(Frame {
            state: game.state.clone(),
            roll: Some(roll),
            hold: ROLL_ANIM_MS,
            rolling: true,
            note: Some(format!("{} бросает кости…", side_name(side, humans))),
        });
        // …затем кости встают на результат и держатся. Если ходить нечем — дольше.
        frames.push(Frame {
            state: game.state.clone(),
            roll: Some(roll),
            hold: if no_move {
                HOLD_NOMOVE_MS
            } else {
                HOLD_ROLL_MS
            },
            rolling: false,
            note: Some(roll_note(side, humans, roll, no_move)),
        });
    }
    let pre = game.state.clone();
    let outcome = game.commit_turn(roll, played, forced);
    // Анимируем в порядке применения (`applied`): ход игрока, а сразу за выкупом —
    // обязательный ответ захватчика (его фишка — не текущей ходящей стороны).
    let applied = &outcome.applied;
    let mut scratch = pre;
    let mut i = 0;
    while i < applied.len() {
        let mv = applied[i];
        // Вход в Тюрьму, за которым этим же ходом сразу следует проход дальше, —
        // это не заточение, а проход насквозь: шагаем сквозь клетку Тюрьмы, не
        // показывая каземат и не объявляя «угодила в Тюрьму».
        let through = mv.kind == MoveKind::EnterPrison
            && applied
                .get(i + 1)
                .is_some_and(|n| n.kind == MoveKind::PrisonPass && n.checker == mv.checker);
        if through {
            scratch = step_through_prison(frames, scratch, mv, roll);
            i += 1;
            continue;
        }
        let forced_resp = scratch.checkers[mv.checker].owner != side;
        scratch = apply_with_frames(frames, scratch, mv, roll, humans);
        if forced_resp && let Some(last) = frames.last_mut() {
            last.note = Some(format!(
                "{}: обязательный ход на 6 после выкупа.",
                side_name(scratch.checkers[mv.checker].owner, humans)
            ));
        }
        i += 1;
    }
}

/// Анимация шага в Тюрьму, когда за ним сразу следует проход дальше: фишка идёт
/// сквозь клетку Тюрьмы как сквозь обычную (без каземата и реплики), но логически
/// возвращаем настоящее состояние (Тюрьма) — для следующего хода `PrisonPass`.
fn step_through_prison(
    frames: &mut Vec<Frame>,
    state: GameState,
    mv: Move,
    roll: DiceRoll,
) -> GameState {
    if let Position::OnTrack { progress: sp } = state.checkers[mv.checker].pos {
        for step in 1..=mv.die {
            let mut mid = state.clone();
            mid.checkers[mv.checker].pos = Position::OnTrack {
                progress: sp + u16::from(step),
            };
            frames.push(Frame {
                state: mid,
                roll: Some(roll),
                hold: HOLD_STEP_MS,
                rolling: false,
                note: None,
            });
        }
    }
    apply(&state, mv)
}

fn fresh(dice: StoredValue<RandomDice>, active: Vec<Side>) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(active.clone(), d)));
    game.expect("game started")
}

/// Активные стороны для `n` игроков: 2 — противоположные, 3 — A/B/C, 4 — все.
fn active_for(n: usize) -> Vec<Side> {
    match n {
        2 => vec![Side::A, Side::A.opposite()],
        3 => vec![Side::A, Side::B, Side::C],
        _ => Side::ALL.to_vec(),
    }
}

/// Уникальные ходы текущего шага среди полных ходов с префиксом `prefix`.
fn step_opts(turns: &[Vec<Move>], prefix: &[Move]) -> Vec<Move> {
    let step = prefix.len();
    let mut out: Vec<Move> = Vec::new();
    for t in turns {
        if t.len() > step && t[..step] == *prefix && !out.contains(&t[step]) {
            out.push(t[step]);
        }
    }
    out
}

fn move_source(ps: &GameState, m: Move) -> Sel {
    sel_of(ps.checkers[m.checker].owner, ps.checkers[m.checker].pos)
}

fn move_dest(ps: &GameState, m: Move) -> Sel {
    let after = apply(ps, m);
    sel_of(
        after.checkers[m.checker].owner,
        after.checkers[m.checker].pos,
    )
}

fn sel_of(owner: Side, pos: Position) -> Sel {
    match pos {
        Position::Reserve => Sel::Reserve,
        Position::Captured { .. } => Sel::Captured,
        _ => checker_cell(owner, pos).map_or(Sel::Reserve, |(r, c)| Sel::Cell(r, c)),
    }
}

// --- Геометрия ---

/// Центр клетки сетки `(строка, столбец)` в координатах SVG.
fn center_pt((row, col): (usize, usize)) -> (f64, f64) {
    (col as f64 + 0.5, row as f64 + 0.5)
}

/// Точка на квадратичной кривой Безье.
fn bez(p0: (f64, f64), c: (f64, f64), p2: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    (
        u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p2.0,
        u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p2.1,
    )
}

struct FieldGeom {
    field: MoonField,
    pt: (f64, f64),
    digit: char,
}

struct ArcGeom {
    path: String,
    arrow: String,
    fields: Vec<FieldGeom>,
}

/// Дуга Луны стороны `side`: парабола от входа к выходу, выгиб внутрь квадрата.
fn moon_arc(side: Side) -> ArcGeom {
    let c = BOARD_DIM as f64 / 2.0;
    // Концы параболы — в середине внутренней грани клеток входа и выхода Луны
    // (сдвиг от центра на пол-клетки строго к центру доски, по перпендикуляру).
    let edge = |coord| {
        let p = center_pt(coord);
        let d = (c - p.0, c - p.1);
        if d.0.abs() > d.1.abs() {
            (p.0 + d.0.signum() * 0.5, p.1)
        } else {
            (p.0, p.1 + d.1.signum() * 0.5)
        }
    };
    let p0 = edge(margin_coord(side.local_to_perimeter(LOCAL_MOON)));
    let p2 = edge(margin_coord(side.local_to_perimeter(LOCAL_MOON_EXIT)));
    let mid = ((p0.0 + p2.0) / 2.0, (p0.1 + p2.1) / 2.0);
    let dir = (c - mid.0, c - mid.1);
    let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt().max(1e-3);
    let ctrl = (mid.0 + dir.0 / len * 9.0, mid.1 + dir.1 / len * 9.0);
    let path = format!(
        "M {:.2} {:.2} Q {:.2} {:.2} {:.2} {:.2}",
        p0.0, p0.1, ctrl.0, ctrl.1, p2.0, p2.1
    );
    let der = (2.0 * (p2.0 - ctrl.0), 2.0 * (p2.1 - ctrl.1));
    let dl = (der.0 * der.0 + der.1 * der.1).sqrt().max(1e-3);
    let (ux, uy) = (der.0 / dl, der.1 / dl);
    let (bx, by) = (p2.0 - ux * 0.3, p2.1 - uy * 0.3);
    let (px, py) = (-uy, ux);
    let arrow = format!(
        "{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}",
        p2.0,
        p2.1,
        bx + px * 0.14,
        by + py * 0.14,
        bx - px * 0.14,
        by - py * 0.14
    );
    let fields = [
        (MoonField::One, 0.30, '1'),
        (MoonField::Three, 0.5, '3'),
        (MoonField::Six, 0.72, '6'),
    ]
    .into_iter()
    .map(|(field, t, digit)| FieldGeom {
        field,
        digit,
        pt: bez(p0, ctrl, p2, t),
    })
    .collect();
    ArcGeom {
        path,
        arrow,
        fields,
    }
}

fn moon_field_point(side: Side, field: MoonField) -> (f64, f64) {
    moon_arc(side)
        .fields
        .into_iter()
        .find(|f| f.field == field)
        .map(|f| f.pt)
        .expect("moon field")
}

/// Полудлина каземата вдоль стороны и его полуглубина внутрь доски.
const CAGE_HALF_LEN: f64 = 1.0;
const CAGE_HALF_DEPTH: f64 = 0.5;
/// Сдвиг каземата вдоль стороны в сторону Дома.
const CAGE_HOME_SHIFT: f64 = 0.8;
/// Крайние слоты фишек по длинной оси каземата.
const CAGE_SLOT_END: f64 = 0.66;

#[derive(Clone, Copy)]
struct PrisonGeom {
    coord: (usize, usize),
    cage: (f64, f64),
    /// Единичный вектор вдоль стороны в сторону Дома (длинная ось каземата).
    along: (f64, f64),
    /// Каземат на вертикальной стороне (вход слева/справа) — рисуется развёрнутым.
    vertical: bool,
}

/// Тюрьмы всех сторон: клетка-маркер на периметре и «каземат» внутри доски
/// вплотную к ней (по перпендикуляру) и сдвинутый вдоль стороны к Дому.
fn prison_geoms() -> Vec<PrisonGeom> {
    let c = BOARD_DIM as f64 / 2.0;
    let mut out = Vec::new();
    for side in Side::ALL {
        let home = center_pt(margin_coord(side.entry()));
        for local in [LOCAL_PRISON_NEAR, LOCAL_PRISON_FAR] {
            let coord = margin_coord(side.local_to_perimeter(local));
            let p = center_pt(coord);
            let d = (c - p.0, c - p.1); // внутрь, к центру
            // Доминирующая ось — перпендикуляр к стороне; вторая — вдоль стороны.
            let vertical = d.0.abs() > d.1.abs();
            let (inward, along) = if vertical {
                ((d.0.signum(), 0.0), (0.0, (home.1 - p.1).signum()))
            } else {
                ((0.0, d.1.signum()), ((home.0 - p.0).signum(), 0.0))
            };
            // Вплотную к клетке (пол-клетки + полуглубина) и сдвиг к Дому.
            let depth = 0.5 + CAGE_HALF_DEPTH;
            let cage = (
                p.0 + inward.0 * depth + along.0 * CAGE_HOME_SHIFT,
                p.1 + inward.1 * depth + along.1 * CAGE_HOME_SHIFT,
            );
            out.push(PrisonGeom {
                coord,
                cage,
                along,
                vertical,
            });
        }
    }
    out
}

fn prison_geom(coord: (usize, usize)) -> Option<PrisonGeom> {
    prison_geoms().into_iter().find(|p| p.coord == coord)
}

fn prison_cage(coord: (usize, usize)) -> Option<(f64, f64)> {
    prison_geom(coord).map(|p| p.cage)
}

/// Точка слота `k` из `n` по длинной оси каземата (центрирована, симметрична).
fn prison_slot_point(g: &PrisonGeom, k: usize, n: usize) -> (f64, f64) {
    let off = if n > 1 {
        (k as f64 - (n as f64 - 1.0) / 2.0) * (2.0 * CAGE_SLOT_END / (n as f64 - 1.0))
    } else {
        0.0
    };
    (g.cage.0 + g.along.0 * off, g.cage.1 + g.along.1 * off)
}

/// Индекс слота фишки-пленника `owner` (по порядку активных сторон) и их число.
fn prison_slot(state: &GameState, owner: Side) -> (usize, usize) {
    let n = state.active.len().max(1);
    let k = state.active.iter().position(|&s| s == owner).unwrap_or(0);
    (k, n)
}

// Зоны снаружи доски напротив клетки входа Дома (выносы наружу, в клетках):
// резерв ближе всего, плен дальше, бросок костей — ещё дальше.
const RESERVE_OUT: f64 = 1.0;
const CAPTURED_OUT: f64 = 1.85;
const DICE_OUT: f64 = 2.75;
const RESERVE_GAP: f64 = 0.64;
/// Запас viewBox под выносные зоны снаружи доски (в клетках).
const RESERVE_PAD: f64 = 3.3;

/// Единичные оси у клетки входа Дома `side`: наружу (перпендикуляр к стороне) и
/// вдоль стороны.
fn reserve_axes(side: Side) -> ((f64, f64), (f64, f64)) {
    let c = BOARD_DIM as f64 / 2.0;
    let home = center_pt(margin_coord(side.entry()));
    let d = (home.0 - c, home.1 - c); // наружу: от центра к Дому
    if d.0.abs() > d.1.abs() {
        ((d.0.signum(), 0.0), (0.0, 1.0))
    } else {
        ((0.0, d.1.signum()), (1.0, 0.0))
    }
}

/// Центр зоны на выносе `out` наружу от клетки входа Дома стороны.
fn outside_anchor(side: Side, out_dist: f64) -> (f64, f64) {
    let home = center_pt(margin_coord(side.entry()));
    let (out, _) = reserve_axes(side);
    (home.0 + out.0 * out_dist, home.1 + out.1 * out_dist)
}

/// Точка `k`-й из `n` фишек в выносном ряду стороны (вдоль стороны, центрирован).
fn outside_point(side: Side, out_dist: f64, k: usize, n: usize) -> (f64, f64) {
    let a = outside_anchor(side, out_dist);
    let (_, along) = reserve_axes(side);
    let off = (k as f64 - (n.max(1) as f64 - 1.0) / 2.0) * RESERVE_GAP;
    (a.0 + along.0 * off, a.1 + along.1 * off)
}

/// Сторона-владелец клетки Дома (вход или слот) среди активных, иначе `None`.
fn home_side(state: &GameState, coord: (usize, usize)) -> Option<Side> {
    state.active.iter().copied().find(|&s| {
        margin_coord(s.entry()) == coord
            || (0..4u8).any(|d| checker_cell(s, Position::Home { depth: d }) == Some(coord))
    })
}

/// «Гнездо» фишки — для группировки наложенных друг на друга (сдвиг при стопке).
#[derive(PartialEq, Eq)]
enum Slot {
    Cell((usize, usize)),
    Moon(Side, MoonField),
    Off,
}

fn slot_key(pos: Position, owner: Side) -> Slot {
    match pos {
        Position::Moon { side, field } => Slot::Moon(side, field),
        Position::Reserve | Position::Captured { .. } => Slot::Off,
        _ => checker_cell(owner, pos).map_or(Slot::Off, Slot::Cell),
    }
}

/// Координаты фишки `i` на доске и видимость (вне доски → невидима, в лотке).
fn checker_xy(state: &GameState, i: usize) -> (f64, f64, bool) {
    let ch = state.checkers[i];
    // Пленники одной стороны делят слот (накладываются в один кружок со счётчиком),
    // а разные цвета разнесены по длинной оси каземата — без диагональной стопки.
    if let Position::Prison { .. } = ch.pos {
        let coord = checker_cell(ch.owner, ch.pos).expect("prison cell");
        let g = prison_geom(coord).expect("prison geom");
        let (k, n) = prison_slot(state, ch.owner);
        let (x, y) = prison_slot_point(&g, k, n);
        return (x, y, true);
    }
    // Резерв — видимый ряд снаружи доски у Дома (4 фикс. слота = индекс среди своих).
    if ch.pos == Position::Reserve {
        let k = (0..i)
            .filter(|&j| state.checkers[j].owner == ch.owner)
            .count();
        let (x, y) = outside_point(ch.owner, RESERVE_OUT, k, 4);
        return (x, y, true);
    }
    // Плен — у Дома ЗАХВАТЧИКА (его «трофеи»), цвет фишки — владельца. Ряд дальше
    // резерва, центрирован по числу пленённых этим захватчиком.
    if let Position::Captured { captor } = ch.pos {
        let held_by = |p: Position| matches!(p, Position::Captured { captor: c } if c == captor);
        let n = state.checkers.iter().filter(|c| held_by(c.pos)).count();
        let k = (0..i).filter(|&j| held_by(state.checkers[j].pos)).count();
        let (x, y) = outside_point(captor, CAPTURED_OUT, k, n);
        return (x, y, true);
    }
    let (base, visible) = match ch.pos {
        Position::Moon { side, field } => (moon_field_point(side, field), true),
        _ => {
            // На клетке (в т.ч. при проходе сквозь клетку Тюрьмы) — по центру клетки.
            // В каземат попадают только настоящие пленники (обработаны выше).
            let coord = checker_cell(ch.owner, ch.pos).expect("on-board cell");
            (center_pt(coord), true)
        }
    };
    let key = slot_key(ch.pos, ch.owner);
    let rank = (0..i)
        .filter(|&j| slot_key(state.checkers[j].pos, state.checkers[j].owner) == key)
        .count();
    (
        base.0 + rank as f64 * 0.16,
        base.1 - rank as f64 * 0.16,
        visible,
    )
}

// --- Статичная отрисовка доски (без фишек) ---

fn cell_rect(coord: (usize, usize), class: &'static str) -> impl IntoView {
    let (r, c) = coord;
    view! {
        <rect x=c as f64 + 0.08 y=r as f64 + 0.08 width=0.84 height=0.84 rx=0.14 class=class />
    }
}

/// Клетки периметра, слоты Дома (по цвету стороны), дуги Луны и казематы Тюрьмы.
fn static_board(state: &GameState) -> Vec<AnyView> {
    let mut nodes: Vec<AnyView> = Vec::new();

    for abs in 0..PERIMETER {
        let p = PerimeterIdx::new(abs);
        let coord = margin_coord(p);
        match cell_kind(p) {
            CellKind::Corner => nodes.push(cell_rect(coord, "corner").into_any()),
            CellKind::Moon => nodes.push(cell_rect(coord, "moon-end").into_any()),
            CellKind::Prison => nodes.push(cell_rect(coord, "prison").into_any()),
            CellKind::HomeEntrance => {
                let stroke = home_side(state, coord).map_or("#f59e0b", side_color);
                let (r, c) = coord;
                nodes.push(
                    view! {
                        <rect x=c as f64 + 0.08 y=r as f64 + 0.08 width=0.84 height=0.84 rx=0.14
                            class="home-gate" stroke=stroke />
                    }
                    .into_any(),
                );
            }
            CellKind::Plain if p.local() == LOCAL_MOON_EXIT => {
                nodes.push(cell_rect(coord, "moon-end").into_any());
            }
            CellKind::Plain => nodes.push(cell_rect(coord, "track").into_any()),
        }
    }

    // Слоты Дома активных сторон — кружки в цвете игрока.
    for &side in &state.active {
        let col = side_color(side);
        for depth in 0..4u8 {
            if let Some(coord) = checker_cell(side, Position::Home { depth }) {
                let (cx, cy) = center_pt(coord);
                nodes.push(
                    view! { <circle cx=cx cy=cy r=0.34 class="home-slot" stroke=col fill=col /> }
                        .into_any(),
                );
            }
        }
    }

    // Луна: парабола, наконечник и пустые поля 1/3/6 (занятые рисует слой фишек).
    for side in Side::ALL {
        let a = moon_arc(side);
        nodes.push(view! { <path d=a.path class="moon-arc" /> }.into_any());
        nodes.push(view! { <polygon points=a.arrow class="moon-arrow" /> }.into_any());
        for f in &a.fields {
            nodes.push(
                view! {
                    <circle cx=f.pt.0 cy=f.pt.1 r=0.3 class="moon-field" />
                    <text x=f.pt.0 y=f.pt.1 class="moon-num">{f.digit.to_string()}</text>
                }
                .into_any(),
            );
        }
    }

    // Тюрьма: каземат с решёткой (фишки в нём рисует слой фишек).
    for pg in prison_geoms() {
        let (cx, cy) = pg.cage;
        // По длинной оси — вдоль стороны (1.4), по короткой — внутрь доски (1.0).
        // Длинная ось — вдоль стороны (2·CAGE_HALF_LEN), короткая — внутрь доски.
        let (hw, hh) = if pg.vertical {
            (CAGE_HALF_DEPTH, CAGE_HALF_LEN)
        } else {
            (CAGE_HALF_LEN, CAGE_HALF_DEPTH)
        };
        nodes.push(
            view! {
                <rect x=cx - hw y=cy - hh width=2.0 * hw height=2.0 * hh rx=0.1 class="cage" />
            }
            .into_any(),
        );
        for k in 0..3 {
            let off = -0.6 + f64::from(k) * 0.6;
            let (x1, y1, x2, y2) = if pg.vertical {
                (cx - 0.4, cy + off, cx + 0.4, cy + off)
            } else {
                (cx + off, cy - 0.4, cx + off, cy + 0.4)
            };
            nodes.push(view! { <line x1=x1 y1=y1 x2=x2 y2=y2 class="cage-bar" /> }.into_any());
        }
    }

    nodes
}

/// Координаты точек (1..6) на кости в сетке 3×3 (поле 100×100).
fn die_pips(v: u8) -> Vec<(i32, i32)> {
    let (lo, mid, hi) = (28, 50, 72);
    match v {
        1 => vec![(mid, mid)],
        2 => vec![(lo, lo), (hi, hi)],
        3 => vec![(lo, lo), (mid, mid), (hi, hi)],
        4 => vec![(lo, lo), (hi, lo), (lo, hi), (hi, hi)],
        5 => vec![(lo, lo), (hi, lo), (mid, mid), (lo, hi), (hi, hi)],
        _ => vec![(lo, lo), (hi, lo), (lo, mid), (hi, mid), (lo, hi), (hi, hi)],
    }
}

/// Грань игральной кости со значением `v` как маленький SVG.
fn die_face(v: u8) -> impl IntoView {
    view! {
        <svg class="die" viewBox="0 0 100 100">
            <rect x=6 y=6 width=88 height=88 rx=20 class="die-body" />
            {die_pips(v).into_iter()
                .map(|(x, y)| view! { <circle cx=x cy=y r=10 class="die-pip" /> })
                .collect_view()}
        </svg>
    }
}

/// Настоящий 3D-кубик: 6 граней (1..6), который кувыркается и встаёт гранью `v`
/// к зрителю (CSS-анимация `die-roll`; конечный поворот задаётся через `--rx/--ry`).
fn die3d(v: u8) -> impl IntoView {
    // Конечный поворот куба, чтобы нужная грань смотрела вперёд (одна ось на грань).
    let (rx, ry) = match v {
        1 => (0, 0),
        2 => (-90, 0),
        3 => (0, -90),
        4 => (0, 90),
        5 => (90, 0),
        _ => (0, 180), // 6
    };
    // Случайные траектория, длительность и задержка — каждая кость кувыркается
    // по-своему и останавливается не одновременно (сумма < ROLL_ANIM_MS).
    let variant = match (js_sys::Math::random() * 4.0) as u8 {
        0 => "die-roll-a",
        1 => "die-roll-b",
        2 => "die-roll-c",
        _ => "die-roll-d",
    };
    // Случайный рисунок отскоков (несколько прыжков по ходу кувырка, затухают).
    let bounce = match (js_sys::Math::random() * 3.0) as u8 {
        0 => "die-bounce-a",
        1 => "die-bounce-b",
        _ => "die-bounce-c",
    };
    // Независимая случайная длительность из одного широкого диапазона (0.9–2.2с):
    // кости могут встать и почти одновременно, и в заметно разные моменты —
    // настоящий случайный бросок (равномерное вращение делает разницу видимой).
    let dur = 0.9 + js_sys::Math::random() * 1.3;
    let delay = js_sys::Math::random() * 0.12;
    view! {
        // Обёртка с тем же таймингом — кость подпрыгивает по ходу вращения.
        <div
            class="die3d"
            style=format!("animation:{bounce} {dur:.3}s ease-in-out {delay:.3}s forwards")
        >
            <div
                class="cube"
                style=format!(
                    "--rx:{rx}deg;--ry:{ry}deg;\
                     animation:{variant} {dur:.3}s cubic-bezier(.3,.3,.7,1) {delay:.3}s forwards",
                )
            >
                <div class="face f-front">{die_face(1)}</div>
                <div class="face f-back">{die_face(6)}</div>
                <div class="face f-right">{die_face(3)}</div>
                <div class="face f-left">{die_face(4)}</div>
                <div class="face f-top">{die_face(2)}</div>
                <div class="face f-bottom">{die_face(5)}</div>
            </div>
        </div>
    }
}

#[component]
fn App() -> impl IntoView {
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    // Настройки: число игроков и какие стороны управляются человеком.
    let players = RwSignal::new(2usize);
    let humans = RwSignal::new(vec![Side::A]);
    let started = RwSignal::new(false);
    // Пауза: блокирует автоход ИИ и авто-бросок; анимация замирает между кадрами.
    let paused = RwSignal::new(false);
    let game = RwSignal::new(fresh(dice, active_for(2)));
    let roll = RwSignal::new(None::<DiceRoll>);
    let turns = RwSignal::new(Vec::<Vec<Move>>::new());
    let prefix = RwSignal::new(Vec::<Move>::new());
    let sel = RwSignal::new(None::<Sel>);
    // Состояние на начало текущего хода человека (нужно для доигровки конца хода:
    // вынужденного ответа выкупа и передачи очереди), пока доска уже продвинута.
    let turn_start = StoredValue::new(game.get_untracked().state.clone());
    // Идёт ли сейчас проигрывание анимации хода (блокирует ввод и авто-бросок).
    let animating = RwSignal::new(false);
    // Крутятся ли сейчас кости (анимация броска).
    let rolling = RwSignal::new(false);
    // Текст ведущего-комментатора над доской.
    let herald = RwSignal::new(String::new());

    // Проигрывает кадры с паузами, обновляя доску и кости; в конце снимает блокировку.
    // На паузе замирает между кадрами (опрос `paused`).
    let play = move |frames: Vec<Frame>| {
        if frames.is_empty() {
            return;
        }
        animating.set(true);
        spawn_local(async move {
            for frame in frames {
                while paused.get_untracked() {
                    TimeoutFuture::new(150).await;
                }
                // Показываем кадр, затем держим его свою паузу (бросок — дольше шага).
                game.update(|g| g.state = frame.state);
                roll.set(frame.roll);
                rolling.set(frame.rolling);
                if let Some(note) = frame.note {
                    herald.set(note);
                }
                TimeoutFuture::new(frame.hold).await;
            }
            rolling.set(false);
            animating.set(false);
        });
    };

    // Автоход компьютерных сторон: по одному ходу за срабатывание Effect. Цепочка
    // ходов «сама себя» продолжает через сигнал `animating` (после анимации хода он
    // снимается → Effect срабатывает снова). Пауза/победа/ход человека её прерывают.
    Effect::new(move |_| {
        let g = game.get();
        let hs = humans.get_untracked();
        if !started.get()
            || animating.get()
            || paused.get()
            || g.winner().is_some()
            || hs.contains(&g.state.to_move)
        {
            return;
        }
        let mut gg = g.clone();
        let mut roll_v = None;
        dice.update_value(|d| roll_v = Some(d.roll()));
        let r = roll_v.expect("roll");
        let t = legal_turns(&gg.state, r);
        let idx = Heuristic.choose_turn(&gg.state, &t).min(t.len() - 1);
        let played = t[idx].clone();
        let mut frames = Vec::new();
        commit_frames(&mut frames, &mut gg, r, played, &hs, true, |s, c, o| {
            Heuristic.choose_forced(s, c, o)
        });
        frames.push(Frame {
            state: gg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: end_note(&gg, &hs),
        });
        play(frames);
    });

    // Старт партии: держим паузу-заставку (видно «Игра началась»), затем снимаем
    // блокировку — дальше ход подхватят Effect'ы (авто-бросок человека / ход ИИ).
    let kickoff = move || {
        animating.set(true);
        spawn_local(async move {
            TimeoutFuture::new(INTRO_MS).await;
            animating.set(false);
        });
    };

    // Авто-бросок в начале хода игрока-человека (но не во время анимации).
    Effect::new(move |_| {
        let g = game.get();
        let busy = animating.get();
        let pause = paused.get();
        let hs = humans.get_untracked();
        if started.get()
            && !busy
            && !pause
            && g.winner().is_none()
            && hs.contains(&g.state.to_move)
            && roll.get_untracked().is_none()
        {
            let mut r = None;
            dice.update_value(|d| r = Some(d.roll()));
            let r = r.expect("roll");
            turn_start.set_value(g.state.clone());
            let t = legal_turns(&g.state, r);
            let no_move = t.first().is_none_or(Vec::is_empty);
            let [a, b] = r.values();
            let name = side_name(g.state.to_move, &hs);
            // Анимируем бросок игрока так же, как у компьютера: кубики кувыркаются,
            // затем показывается результат и включается выбор хода.
            animating.set(true);
            spawn_local(async move {
                herald.set(format!("{name} бросает кости…"));
                roll.set(Some(r));
                rolling.set(true);
                TimeoutFuture::new(ROLL_ANIM_MS).await;
                rolling.set(false);
                herald.set(if no_move {
                    format!("{name}: выпало {a} и {b}. Ходить нечем — пропуск.")
                } else if r.is_double() {
                    format!("{name}: выпало {a} и {b}. Дубль — ещё ход!")
                } else {
                    format!("{name}: выпало {a} и {b}. Выберите ход.")
                });
                turns.set(t);
                prefix.set(Vec::new());
                sel.set(None);
                animating.set(false);
            });
        }
    });

    // Доигровка хода человека: применённые ходы (`played`) дают `after`; отсюда
    // доигрываем вынужденный ответ выкупа и фиксируем передачу очереди. Дальнейшие
    // ходы компьютера подхватит Effect автоматического хода.
    let finish = move |mut frames: Vec<Frame>, after: GameState, played: Vec<Move>| {
        let r = roll.get_untracked().expect("roll");
        let hs = humans.get_untracked();
        let mut fg = Game::new(turn_start.get_value());
        let outcome = fg.commit_turn(r, played, |_, _, _| 0);
        let mut scratch = after;
        for &fm in &outcome.forced {
            scratch = apply_with_frames(&mut frames, scratch, fm, r, &hs);
        }
        let _ = scratch;
        frames.push(Frame {
            state: fg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: end_note(&fg, &hs),
        });
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        play(frames);
    };

    let click = move |target: Sel| {
        let g = game.get_untracked();
        let hs = humans.get_untracked();
        if animating.get_untracked()
            || paused.get_untracked()
            || g.winner().is_some()
            || !hs.contains(&g.state.to_move)
            || roll.get_untracked().is_none()
        {
            return;
        }
        let r = roll.get_untracked().expect("roll");
        let ps = g.state.clone(); // доска уже продвинута применёнными частями хода
        let pre = prefix.get_untracked();
        let cands = step_opts(&turns.get_untracked(), &pre);
        match sel.get_untracked() {
            None => {
                if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                }
            }
            Some(src) if src == target => sel.set(None),
            Some(src) => {
                let found = cands
                    .iter()
                    .find(|&&m| move_source(&ps, m) == src && move_dest(&ps, m) == target);
                if let Some(&m) = found {
                    let mut np = pre.clone();
                    np.push(m);
                    let total = turns.get_untracked().first().map_or(0, Vec::len);
                    // Выбранную часть хода проигрываем сразу — пошагово, по клеткам.
                    let mut frames = Vec::new();
                    let after = apply_with_frames(&mut frames, ps.clone(), m, r, &hs);
                    if np.len() >= total {
                        finish(frames, after, np);
                    } else {
                        // Сохраняем фокус на той же фишке, если ею можно ходить дальше.
                        let next_src = sel_of(
                            after.checkers[m.checker].owner,
                            after.checkers[m.checker].pos,
                        );
                        let keep = step_opts(&turns.get_untracked(), &np)
                            .iter()
                            .any(|&mv| move_source(&after, mv) == next_src);
                        prefix.set(np);
                        sel.set(keep.then_some(next_src));
                        play(frames);
                    }
                } else if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                } else {
                    sel.set(None);
                }
            }
        }
    };

    // Запуск партии по выбранным настройкам (число игроков и кто человек).
    let start_game = move |_| {
        let active = active_for(players.get_untracked());
        dice.update_value(|d| *d = RandomDice::from_seed(seed()));
        roll.set(None);
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        let g = fresh(dice, active);
        let hs = humans.get_untracked();
        herald.set(format!(
            "Игра началась! Ходит {}.",
            side_name(g.state.to_move, &hs)
        ));
        game.set(g);
        started.set(true);
        kickoff();
    };
    // Назад к настройкам (кнопка «Настройки»).
    let to_settings = move |_| started.set(false);

    // Переключатель типа стороны (человек ↔ компьютер) в настройках.
    let toggle_human = move |side: Side| {
        humans.update(|h| {
            if let Some(i) = h.iter().position(|&s| s == side) {
                h.remove(i);
            } else {
                h.push(side);
            }
        });
    };

    view! {
        <div class="wrap">
            <h1>"Шеш-Беш"</h1>
            // Экран настроек до старта партии: число игроков и тип каждой стороны.
            {move || (!started.get()).then(|| view! {
                <div class="settings">
                    <div class="set-row">
                        <span>"Игроков:"</span>
                        {[2usize, 3, 4].into_iter().map(|n| view! {
                            <button
                                class:on=move || players.get() == n
                                on:click=move |_| { players.set(n); humans.set(vec![Side::A]); }
                            >{n.to_string()}</button>
                        }).collect_view()}
                    </div>
                    {move || {
                        let act = active_for(players.get());
                        act.into_iter().map(|s| {
                            let is_h = move || humans.get().contains(&s);
                            view! {
                                <div class="set-row">
                                    <b style=format!("color:{}", side_color(s))>
                                        {format!("Сторона {}", s.letter())}
                                    </b>
                                    <button class:on=is_h on:click=move |_| toggle_human(s)>
                                        {move || if is_h() { "Человек" } else { "Компьютер" }}
                                    </button>
                                </div>
                            }
                        }).collect_view()
                    }}
                    <button class="primary" on:click=start_game>"Начать игру"</button>
                </div>
            })}

            // Игровой экран после старта.
            {move || started.get().then(|| view! {
            <div class="status">
                <span class="herald">{move || herald.get()}</span>
            </div>
            <div class="controls">
                <button on:click=to_settings>"Настройки"</button>
                <button class:on=move || paused.get() on:click=move |_| paused.update(|p| *p = !*p)>
                    {move || if paused.get() { "▶ Продолжить" } else { "⏸ Пауза" }}
                </button>
                {move || {
                    let g = game.get();
                    let no_moves = roll.get().is_some()
                        && g.winner().is_none()
                        && humans.get().contains(&g.state.to_move)
                        && turns.get().first().is_none_or(Vec::is_empty);
                    no_moves.then(|| view! {
                        <button on:click=move |_| finish(Vec::new(), game.get_untracked().state.clone(), Vec::new())>
                            "Нет ходов — пропустить"
                        </button>
                    })
                }}
            </div>

            <div class="board-area">
            <div class="board-wrap">
            // viewBox с запасом RESERVE_PAD по краям: снаружи доски, напротив каждого
            // Дома, размещены резерв, плен и (у ходящей стороны) кости.
            <svg class="board" viewBox=format!(
                "{o} {o} {s} {s}",
                o = BOARD_MARGIN as f64 - RESERVE_PAD,
                s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD,
            )>
                {move || static_board(&game.get().state)}

                <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                    <circle
                        r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Reserve | Position::Captured { .. }) { 0.3 } else { 0.36 }
                        class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Captured { .. }) {
                            "piece captive"
                        } else {
                            "piece"
                        }
                        fill=move || side_color(game.get().state.checkers[i].owner)
                        cx=move || checker_xy(&game.get().state, i).0
                        cy=move || checker_xy(&game.get().state, i).1
                        opacity=move || if checker_xy(&game.get().state, i).2 { 1.0 } else { 0.0 }
                    />
                </For>

                // Счётчик одноцветных пленников в каземате (>1 — цифра внутри кружка).
                {move || {
                    let g = game.get();
                    let s = &g.state;
                    let mut nodes: Vec<AnyView> = Vec::new();
                    for pg in prison_geoms() {
                        for &side in &s.active {
                            let cnt = s.checkers.iter().filter(|c| {
                                c.owner == side
                                    && matches!(c.pos, Position::Prison { .. })
                                    && checker_cell(c.owner, c.pos) == Some(pg.coord)
                            }).count();
                            if cnt > 1 {
                                let (k, n) = prison_slot(s, side);
                                let (x, y) = prison_slot_point(&pg, k, n);
                                nodes.push(view! {
                                    <text x=x y=y class="cage-count">{cnt.to_string()}</text>
                                }.into_any());
                            }
                        }
                    }
                    nodes
                }}

                {move || {
                    let g = game.get();
                    let pre = prefix.get();
                    let ps = g.state.clone(); // доска уже продвинута применёнными частями
                    let mover = g.state.to_move;
                    let active = !animating.get()
                        && roll.get().is_some()
                        && g.winner().is_none()
                        && humans.get().contains(&mover);
                    let cands = if active { step_opts(&turns.get(), &pre) } else { Vec::new() };
                    let cur = sel.get();

                    let mut srcs: Vec<(usize, usize)> = Vec::new();
                    let mut dsts: Vec<(usize, usize)> = Vec::new();
                    for &m in &cands {
                        if let Sel::Cell(r, c) = move_source(&ps, m)
                            && !srcs.contains(&(r, c))
                        {
                            srcs.push((r, c));
                        }
                        if let Some(s) = cur
                            && move_source(&ps, m) == s
                            && let Sel::Cell(r, c) = move_dest(&ps, m)
                            && !dsts.contains(&(r, c))
                        {
                            dsts.push((r, c));
                        }
                    }
                    let sel_cell = match cur { Some(Sel::Cell(r, c)) => Some((r, c)), _ => None };
                    let pt_of = |coord: (usize, usize)| {
                        prison_cage(coord).unwrap_or_else(|| center_pt(coord))
                    };

                    let mut nodes: Vec<AnyView> = Vec::new();
                    let ring_pt = |cx: f64, cy: f64, cls: &'static str| {
                        view! { <circle cx=cx cy=cy r=0.47 fill="none" class=cls /> }.into_any()
                    };
                    let ring = |coord: (usize, usize), cls: &'static str| {
                        let (cx, cy) = pt_of(coord);
                        ring_pt(cx, cy, cls)
                    };
                    if let Some(rc) = sel_cell {
                        nodes.push(ring(rc, "hl-sel"));
                    }
                    for &rc in &dsts {
                        nodes.push(ring(rc, "hl-dst"));
                    }
                    if cur.is_none() {
                        for &rc in &srcs {
                            nodes.push(ring(rc, "hl-src"));
                        }
                    }
                    // Кликабельные зоны (круг в точке клетки/каземата).
                    let mut hits: Vec<(usize, usize)> = srcs.clone();
                    hits.extend(dsts.iter().copied());
                    if let Some(rc) = sel_cell { hits.push(rc); }
                    for (r, c) in hits {
                        let (cx, cy) = pt_of((r, c));
                        nodes.push(view! {
                            <circle cx=cx cy=cy r=0.5 class="hit" on:click=move |_| click(Sel::Cell(r, c)) />
                        }.into_any());
                    }
                    // Выносные зоны хода (резерв у своего Дома, плен — у Дома
                    // захватчика): подсветка + клик в их центре.
                    let mut zone = |kind: Sel, zx: f64, zy: f64| {
                        let is_src = cands.iter().any(|&m| move_source(&ps, m) == kind);
                        let is_dst = cur.is_some_and(|s| {
                            cands.iter().any(|&m| move_source(&ps, m) == s && move_dest(&ps, m) == kind)
                        });
                        let is_sel = cur == Some(kind);
                        if is_sel {
                            nodes.push(ring_pt(zx, zy, "hl-sel"));
                        } else if is_dst {
                            nodes.push(ring_pt(zx, zy, "hl-dst"));
                        } else if is_src && cur.is_none() {
                            nodes.push(ring_pt(zx, zy, "hl-src"));
                        }
                        if is_sel || is_dst || (is_src && cur.is_none()) {
                            nodes.push(view! {
                                <circle cx=zx cy=zy r=1.1 class="hit" on:click=move |_| click(kind) />
                            }.into_any());
                        }
                    };
                    let (rx, ry) = outside_anchor(mover, RESERVE_OUT);
                    zone(Sel::Reserve, rx, ry);
                    // Пленные хода — у Домов захватчиков; зона на каждом таком Доме.
                    let mut captors: Vec<Side> = Vec::new();
                    for c in &ps.checkers {
                        if c.owner == mover
                            && let Position::Captured { captor } = c.pos
                            && !captors.contains(&captor)
                        {
                            captors.push(captor);
                        }
                    }
                    for captor in captors {
                        let (cx2, cy2) = outside_anchor(captor, CAPTURED_OUT);
                        zone(Sel::Captured, cx2, cy2);
                    }
                    nodes
                }}
            </svg>
            // Кости — HTML-оверлей над доской, у Дома ходящей стороны (дальше плена).
            {move || {
                let g = game.get();
                let to_move = g.state.to_move;
                let spinning = rolling.get();
                roll.get().map(|r| {
                    let (ax, ay) = outside_anchor(to_move, DICE_OUT);
                    let (out, _) = reserve_axes(to_move);
                    let vertical = out.0 != 0.0; // Дом на левой/правой стороне доски
                    let vb_o = BOARD_MARGIN as f64 - RESERVE_PAD;
                    let vb_s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD;
                    let fx = (ax - vb_o) / vb_s * 100.0;
                    let fy = (ay - vb_o) / vb_s * 100.0;
                    let [a, b] = r.values();
                    let inner = if spinning {
                        let cls = if r.is_double() { "dice rolling double" } else { "dice rolling" };
                        view! { <span class=cls>{die3d(a)} {die3d(b)}</span> }.into_any()
                    } else {
                        let cls = if r.is_double() { "dice double" } else { "dice" };
                        view! { <span class=cls>{die_face(a)} {die_face(b)}</span> }.into_any()
                    };
                    view! {
                        <div class="board-dice" class:vert=vertical style=format!("left:{fx:.2}%;top:{fy:.2}%")>{inner}</div>
                    }
                })
            }}
            </div>
            </div>
            })}
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
