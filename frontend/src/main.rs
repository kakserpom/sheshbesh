//! Leptos-фронтенд Шеш-Беш: вся игровая логика — движок `sheshbesh` в WASM.
//! Доска — векторная (SVG); фишки рисуются отдельным слоем keyed-кружков, что даёт
//! плавную анимацию перемещения (CSS-переход `cx`/`cy`). Человек играет стороной A
//! против эвристики; ход собирается кликами (фишка → клетка) по одной кости.

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

/// Пауза на кадр «кости брошены» — крутятся кости (мс).
const HOLD_ROLL_MS: u32 = 1600;
/// Пауза на один шаг фишки по клетке, мс.
const HOLD_STEP_MS: u32 = 520;

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
/// перед его показом (мс) и флаг «кости сейчас крутятся» (анимация броска).
#[derive(Clone)]
struct Frame {
    state: GameState,
    roll: Option<DiceRoll>,
    hold: u32,
    rolling: bool,
}

/// Применяет ход `mv` к `state`, попутно добавляя кадры. Если фишка идёт по дорожке,
/// эмитится по кадру на **каждую промежуточную клетку** (фишка «шагает», а не
/// перепрыгивает сразу на конечную). Возвращает состояние после хода.
fn apply_with_frames(
    frames: &mut Vec<Frame>,
    state: GameState,
    mv: Move,
    roll: DiceRoll,
) -> GameState {
    if let Position::OnTrack { progress: sp } = state.checkers[mv.checker].pos {
        for step in 1..mv.die {
            let mut mid = state.clone();
            mid.checkers[mv.checker].pos = Position::OnTrack {
                progress: sp + u16::from(step),
            };
            frames.push(Frame {
                state: mid,
                roll: Some(roll),
                hold: HOLD_STEP_MS,
                rolling: false,
            });
        }
    }
    let after = apply(&state, mv);
    frames.push(Frame {
        state: after.clone(),
        roll: Some(roll),
        hold: HOLD_STEP_MS,
        rolling: false,
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
    forced: F,
) where
    F: FnMut(&GameState, Side, &[Move]) -> usize,
{
    frames.push(Frame {
        state: game.state.clone(),
        roll: Some(roll),
        hold: HOLD_ROLL_MS,
        rolling: true,
    });
    let pre = game.state.clone();
    let outcome = game.commit_turn(roll, played, forced);
    let mut scratch = pre;
    let mut forced_moves = outcome.forced.iter();
    for mv in &outcome.played {
        let ransom = mv.kind == MoveKind::Ransom;
        scratch = apply_with_frames(frames, scratch, *mv, roll);
        if ransom && let Some(&fm) = forced_moves.next() {
            scratch = apply_with_frames(frames, scratch, fm, roll);
        }
    }
}

/// Дополняет `frames` ходами ИИ (всех не-человеческих сторон, с дублями и выкупами)
/// от текущего состояния `game` до хода человека. Продвигает `game`.
fn ai_frames(game: &mut Game, dice: StoredValue<RandomDice>, human: Side, frames: &mut Vec<Frame>) {
    let mut guard = 0;
    while game.winner().is_none() && game.state.to_move != human && guard < 4000 {
        guard += 1;
        let mut roll = None;
        dice.update_value(|d| roll = Some(d.roll()));
        let roll = roll.expect("roll");
        let turns = legal_turns(&game.state, roll);
        let idx = Heuristic
            .choose_turn(&game.state, &turns)
            .min(turns.len() - 1);
        let played = turns[idx].clone();
        commit_frames(frames, game, roll, played, |s, c, o| {
            Heuristic.choose_forced(s, c, o)
        });
    }
}

fn fresh(dice: StoredValue<RandomDice>) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(vec![Side::A, Side::A.opposite()], d)));
    game.expect("game started")
}

fn after_prefix(game: &Game, prefix: &[Move]) -> GameState {
    let mut s = game.state.clone();
    for &m in prefix {
        s = apply(&s, m);
    }
    s
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

fn count_pos(s: &GameState, side: Side, want_reserve: bool) -> usize {
    s.checkers
        .iter()
        .filter(|c| {
            c.owner == side
                && if want_reserve {
                    c.pos == Position::Reserve
                } else {
                    matches!(c.pos, Position::Captured { .. })
                }
        })
        .count()
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
    let (base, visible) = match ch.pos {
        Position::Reserve | Position::Captured { .. } => {
            (center_pt(margin_coord(ch.owner.entry())), false)
        }
        Position::Moon { side, field } => (moon_field_point(side, field), true),
        _ => {
            let coord = checker_cell(ch.owner, ch.pos).expect("on-board cell");
            (prison_cage(coord).unwrap_or_else(|| center_pt(coord)), true)
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

/// Лоток плена «казематом»: тёмная решётчатая коробка с пленёнными фишками
/// (нейтральный цвет — в отличие от красной Тюрьмы на доске).
fn captured_cage(side: Side, n: usize) -> impl IntoView {
    let col = side_color(side);
    let slots = i32::try_from(n.max(1)).unwrap_or(1);
    let h = 22;
    let w = 8 + 16 * slots;
    let dots = (0..i32::try_from(n).unwrap_or(0))
        .map(|k| view! { <circle cx=12 + 16 * k cy=11 r=6 fill=col /> })
        .collect_view();
    let bars = (0..=slots)
        .map(|k| view! { <line x1=4 + 16 * k y1=4 x2=4 + 16 * k y2=h - 4 class="cc-bar" /> })
        .collect_view();
    view! {
        <svg class="cage-chip" viewBox=format!("0 0 {w} {h}")>
            <rect x=2 y=2 width=w - 4 height=h - 4 rx=4 class="cc-body" />
            {dots}
            {bars}
        </svg>
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

#[component]
fn App() -> impl IntoView {
    let human = Side::A;
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    let game = RwSignal::new(fresh(dice));
    let roll = RwSignal::new(None::<DiceRoll>);
    let turns = RwSignal::new(Vec::<Vec<Move>>::new());
    let prefix = RwSignal::new(Vec::<Move>::new());
    let sel = RwSignal::new(None::<Sel>);
    // Идёт ли сейчас проигрывание анимации хода (блокирует ввод и авто-бросок).
    let animating = RwSignal::new(false);
    // Крутятся ли сейчас кости (анимация броска).
    let rolling = RwSignal::new(false);

    // Проигрывает кадры с паузами, обновляя доску и кости; в конце снимает блокировку.
    let play = move |frames: Vec<Frame>| {
        if frames.is_empty() {
            return;
        }
        animating.set(true);
        spawn_local(async move {
            for frame in frames {
                // Показываем кадр, затем держим его свою паузу (бросок — дольше шага).
                game.update(|g| g.state = frame.state);
                roll.set(frame.roll);
                rolling.set(frame.rolling);
                TimeoutFuture::new(frame.hold).await;
            }
            rolling.set(false);
            animating.set(false);
        });
    };

    // Если ход за оппонентом (ИИ) — собрать кадры и проиграть их пошагово.
    let run_ai = move || {
        let mut g = game.get_untracked();
        if g.winner().is_some() || g.state.to_move == human {
            return;
        }
        let mut frames = Vec::new();
        ai_frames(&mut g, dice, human, &mut frames);
        frames.push(Frame {
            state: g.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
        });
        play(frames);
    };

    // Авто-бросок в начале хода человека (но не во время анимации).
    Effect::new(move |_| {
        let g = game.get();
        let busy = animating.get();
        if !busy
            && g.winner().is_none()
            && g.state.to_move == human
            && roll.get_untracked().is_none()
        {
            let mut r = None;
            dice.update_value(|d| r = Some(d.roll()));
            let r = r.expect("roll");
            roll.set(Some(r));
            turns.set(legal_turns(&g.state, r));
            prefix.set(Vec::new());
            sel.set(None);
        }
    });

    let commit = move |seq: Vec<Move>| {
        let r = roll.get_untracked().expect("roll");
        // Ход человека анимируется так же пошагово, затем продолжает ИИ.
        let mut g = game.get_untracked();
        let mut frames = Vec::new();
        commit_frames(&mut frames, &mut g, r, seq, |_, _, _| 0);
        ai_frames(&mut g, dice, human, &mut frames);
        frames.push(Frame {
            state: g.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
        });
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        play(frames);
    };

    let click = move |target: Sel| {
        let g = game.get_untracked();
        if animating.get_untracked()
            || g.winner().is_some()
            || g.state.to_move != human
            || roll.get_untracked().is_none()
        {
            return;
        }
        let pre = prefix.get_untracked();
        let ps = after_prefix(&g, &pre);
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
                    if np.len() >= total {
                        commit(np);
                    } else {
                        prefix.set(np);
                        sel.set(None);
                    }
                } else if cands.iter().any(|&m| move_source(&ps, m) == target) {
                    sel.set(Some(target));
                } else {
                    sel.set(None);
                }
            }
        }
    };

    let on_new = move |_| {
        dice.update_value(|d| *d = RandomDice::from_seed(seed()));
        roll.set(None);
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        game.set(fresh(dice));
        // Если по розыгрышу первый ход достался ИИ — проиграть его сразу.
        run_ai();
    };

    // На старте партии первый ход может принадлежать ИИ — анимируем его.
    run_ai();

    // Лотки сторон (резерв + плен-каземат) — построчно, слева от доски.
    let render_trays = move || {
        let g = game.get();
        let pre = prefix.get();
        let ps = after_prefix(&g, &pre);
        let active = roll.get().is_some() && g.winner().is_none() && g.state.to_move == human;
        let cands = if active {
            step_opts(&turns.get(), &pre)
        } else {
            Vec::new()
        };
        let cur = sel.get();
        let to_move = g.state.to_move;
        let cur_roll = roll.get();
        let spinning = rolling.get();
        g.state.active.clone().into_iter().map(|side| {
            // Класс лотка и кликабельность для выбранного типа (резерв/плен).
            let state_of = |kind: Sel| {
                let is_human = side == human;
                let is_src = is_human && cands.iter().any(|&m| move_source(&ps, m) == kind);
                let is_dst = is_human && matches!(cur, Some(s) if cands.iter().any(|&m| move_source(&ps, m) == s && move_dest(&ps, m) == kind));
                let is_sel = cur == Some(kind);
                let cls = if is_sel { "chip sel" }
                    else if is_dst { "chip dst" }
                    else if is_src { "chip src" }
                    else { "chip" };
                (cls, is_src || is_dst || is_sel)
            };
            let res_n = count_pos(&ps, side, true);
            let (res_cls, res_click) = state_of(Sel::Reserve);
            // Резерв — только точки (число фишек видно по ним).
            let dots = (0..res_n).map(|_| view! {
                <span class="dot" style=format!("background:{}", side_color(side)) />
            }).collect_view();
            let cap_n = count_pos(&ps, side, false);
            let (cap_cls, cap_click) = state_of(Sel::Captured);
            // Кости — на строке стороны, чей сейчас ход (с анимацией броска/дубля).
            let dice = (side == to_move).then_some(cur_roll).flatten().map(|r| {
                let [a, b] = r.values();
                let mut cls = String::from("dice");
                if spinning {
                    cls.push_str(" rolling");
                }
                if r.is_double() {
                    cls.push_str(" double");
                }
                view! { <span class=cls>{die_face(a)} {die_face(b)}</span> }
            });
            view! {
                <div class="tray-row" class:active=side == to_move>
                    <b style=format!("color:{}", side_color(side))>{side.letter().to_string()}</b>
                    <span class=res_cls on:click=move |_| if res_click { click(Sel::Reserve) }>
                        "резерв " {dots}
                    </span>
                    // Плен — каземат (нейтрального цвета) с пленёнными фишками.
                    <span class=cap_cls on:click=move |_| if cap_click { click(Sel::Captured) }>
                        {captured_cage(side, cap_n)}
                    </span>
                    {dice}
                </div>
            }
        }).collect_view()
    };

    view! {
        <div class="wrap">
            <h1>"Шеш-Беш"</h1>
            <div class="status">
                {move || {
                    let g = game.get();
                    match g.winner() {
                        Some(w) => format!("Победила сторона {}", w.letter()),
                        None => format!("Ход стороны {}", g.state.to_move.letter()),
                    }
                }}
            </div>
            <div class="controls">
                <button on:click=on_new>"Новая игра"</button>
                {move || {
                    let g = game.get();
                    let no_moves = roll.get().is_some()
                        && g.winner().is_none()
                        && g.state.to_move == human
                        && turns.get().first().is_none_or(Vec::is_empty);
                    no_moves.then(|| view! {
                        <button on:click=move |_| commit(Vec::new())>"Нет ходов — пропустить"</button>
                    })
                }}
            </div>

            <div class="main">
            <div class="trays">{render_trays}</div>
            <div class="board-area">
            // Кадрируем viewBox по содержимому: пустые поля BOARD_MARGIN по краям
            // (нужные движку/TUI) обрезаем, оставляя лишь тонкий зазор 0.5.
            <svg class="board" viewBox=format!(
                "{o} {o} {s} {s}",
                o = BOARD_MARGIN as f64 - 0.5,
                s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 1.0,
            )>
                {move || static_board(&game.get().state)}

                <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                    <circle
                        r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. }) { 0.3 } else { 0.36 }
                        class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. }) {
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
                    let ps = after_prefix(&g, &pre);
                    let active = roll.get().is_some()
                        && g.winner().is_none()
                        && g.state.to_move == human;
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
                    let ring = |coord: (usize, usize), cls: &'static str| {
                        let (cx, cy) = pt_of(coord);
                        view! { <circle cx=cx cy=cy r=0.47 fill="none" class=cls /> }.into_any()
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
                    nodes
                }}
            </svg>
            </div>
            </div>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
