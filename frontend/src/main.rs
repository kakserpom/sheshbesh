//! Leptos-фронтенд Шеш-Беш: вся игровая логика — движок `sheshbesh` в WASM.
//! Доска — векторная (SVG); фишки рисуются отдельным слоем keyed-кружков, что даёт
//! плавную анимацию перемещения (CSS-переход `cx`/`cy`). Человек играет стороной A
//! против эвристики; ход собирается кликами (фишка → клетка) по одной кости.

use leptos::prelude::*;
use sheshbesh::board::{
    LOCAL_MOON, LOCAL_MOON_EXIT, LOCAL_PRISON_FAR, LOCAL_PRISON_NEAR, PERIMETER, cell_kind,
};
use sheshbesh::{
    BOARD_DIM, CellKind, DiceRoll, DiceSource, Game, GameState, Heuristic, MoonField, Move,
    PerimeterIdx, Position, RandomDice, Side, apply, checker_cell, legal_turns, margin_coord,
};

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

fn advance_ai(game: &mut Game, dice: StoredValue<RandomDice>, human: Side) {
    let mut guard = 0;
    while game.winner().is_none() && game.state.to_move != human && guard < 2000 {
        dice.update_value(|d| {
            game.play_turn(d, &mut Heuristic);
        });
        guard += 1;
    }
}

fn fresh(dice: StoredValue<RandomDice>, human: Side) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(vec![Side::A, Side::A.opposite()], d)));
    let mut game = game.expect("game started");
    advance_ai(&mut game, dice, human);
    game
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
    let nudge = |p: (f64, f64), amt: f64| {
        let d = (c - p.0, c - p.1);
        let l = (d.0 * d.0 + d.1 * d.1).sqrt().max(1e-3);
        (p.0 + d.0 / l * amt, p.1 + d.1 / l * amt)
    };
    let p0 = nudge(
        center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON))),
        0.8,
    );
    let p2 = nudge(
        center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON_EXIT))),
        0.8,
    );
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

struct PrisonGeom {
    coord: (usize, usize),
    cage: (f64, f64),
}

/// Тюрьмы всех сторон: клетка-маркер на периметре и «каземат» внутри доски.
fn prison_geoms() -> Vec<PrisonGeom> {
    let c = BOARD_DIM as f64 / 2.0;
    let mut out = Vec::new();
    for side in Side::ALL {
        for local in [LOCAL_PRISON_NEAR, LOCAL_PRISON_FAR] {
            let coord = margin_coord(side.local_to_perimeter(local));
            let p = center_pt(coord);
            let d = (c - p.0, c - p.1); // внутрь, к центру
            let l = (d.0 * d.0 + d.1 * d.1).sqrt().max(1e-3);
            out.push(PrisonGeom {
                coord,
                cage: (p.0 + d.0 / l * 1.8, p.1 + d.1 / l * 1.8),
            });
        }
    }
    out
}

fn prison_cage(coord: (usize, usize)) -> Option<(f64, f64)> {
    prison_geoms()
        .into_iter()
        .find(|p| p.coord == coord)
        .map(|p| p.cage)
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
        nodes.push(
            view! { <rect x=cx - 0.7 y=cy - 0.5 width=1.4 height=1.0 rx=0.1 class="cage" /> }
                .into_any(),
        );
        for k in 0..3 {
            let bx = cx - 0.45 + f64::from(k) * 0.45;
            nodes.push(
                view! { <line x1=bx y1=cy - 0.45 x2=bx y2=cy + 0.45 class="cage-bar" /> }
                    .into_any(),
            );
        }
    }

    nodes
}

#[component]
fn App() -> impl IntoView {
    let human = Side::A;
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    let game = RwSignal::new(fresh(dice, human));
    let roll = RwSignal::new(None::<DiceRoll>);
    let turns = RwSignal::new(Vec::<Vec<Move>>::new());
    let prefix = RwSignal::new(Vec::<Move>::new());
    let sel = RwSignal::new(None::<Sel>);

    // Авто-бросок в начале хода человека.
    Effect::new(move |_| {
        let g = game.get();
        if g.winner().is_none() && g.state.to_move == human && roll.get_untracked().is_none() {
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
        game.update(|g| {
            g.commit_turn(r, seq, |_, _, _| 0);
            advance_ai(g, dice, human);
        });
        roll.set(None);
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
    };

    let click = move |target: Sel| {
        let g = game.get_untracked();
        if g.winner().is_some() || g.state.to_move != human || roll.get_untracked().is_none() {
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
        game.set(fresh(dice, human));
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
                {move || roll.get().map(|r| {
                    let [a, b] = r.values();
                    view! { <span class="dice">{format!("кости: {a} + {b}")}</span> }
                })}
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

            <div class="board-area">
            <svg class="board" viewBox=format!("0 0 {d} {d}", d = BOARD_DIM)>
                {move || static_board(&game.get().state)}

                <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                    <circle
                        r=0.36
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

            <div class="trays">
                {move || {
                    let g = game.get();
                    let pre = prefix.get();
                    let ps = after_prefix(&g, &pre);
                    let active = roll.get().is_some()
                        && g.winner().is_none()
                        && g.state.to_move == human;
                    let cands = if active { step_opts(&turns.get(), &pre) } else { Vec::new() };
                    let cur = sel.get();
                    g.state.active.clone().into_iter().map(|side| {
                        let chip = |kind: Sel, label: &'static str, n: usize| {
                            let is_human = side == human;
                            let is_src = is_human && cands.iter().any(|&m| move_source(&ps, m) == kind);
                            let is_dst = is_human && matches!(cur, Some(s) if cands.iter().any(|&m| move_source(&ps, m) == s && move_dest(&ps, m) == kind));
                            let is_sel = cur == Some(kind);
                            let cls = if is_sel { "chip sel" }
                                else if is_dst { "chip dst" }
                                else if is_src { "chip src" }
                                else { "chip" };
                            let dots = (0..n).map(|_| view! {
                                <span class="dot" style=format!("background:{}", side_color(side)) />
                            }).collect_view();
                            let clickable = is_src || is_dst || is_sel;
                            view! {
                                <span class=cls on:click=move |_| if clickable { click(kind) }>
                                    {label} " " {dots} " " {n.to_string()}
                                </span>
                            }
                        };
                        view! {
                            <div class="tray-row">
                                <b style=format!("color:{}", side_color(side))>{side.letter().to_string()}</b>
                                {chip(Sel::Reserve, "резерв", count_pos(&ps, side, true))}
                                {chip(Sel::Captured, "плен", count_pos(&ps, side, false))}
                            </div>
                        }
                    }).collect_view()
                }}
            </div>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
