//! Leptos-фронтенд Шеш-Беш: вся игровая логика — движок `sheshbesh` в WASM.
//! Человек играет стороной A против эвристики; ход собирается **кликами по доске**
//! (фишка → клетка назначения) по одной кости, плюс лотки резерва и плена.

use leptos::prelude::*;
use sheshbesh::{
    BOARD_DIM, DiceRoll, DiceSource, Game, GameState, Glyph, Heuristic, Move, Position, RandomDice,
    Side, apply, board_glyphs, checker_cell, legal_turns,
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

/// Тип подсветки клетки.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hl {
    None,
    Source,
    Dest,
    Selected,
}

fn side_color(s: Side) -> &'static str {
    match s {
        Side::A => "#22d3ee",
        Side::B => "#4ade80",
        Side::C => "#e879f9",
        Side::D => "#facc15",
    }
}

/// Доводит партию до хода человека (за неактивные стороны играет эвристика).
fn advance_ai(game: &mut Game, dice: StoredValue<RandomDice>, human: Side) {
    let mut guard = 0;
    while game.winner().is_none() && game.state.to_move != human && guard < 2000 {
        dice.update_value(|d| {
            game.play_turn(d, &mut Heuristic);
        });
        guard += 1;
    }
}

/// Новая партия: розыгрыш первого хода и авто-ходы ИИ до хода человека.
fn fresh(dice: StoredValue<RandomDice>, human: Side) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(vec![Side::A, Side::A.opposite()], d)));
    let mut game = game.expect("game started");
    advance_ai(&mut game, dice, human);
    game
}

/// Состояние после применения уже выбранных ходов `prefix` (превью хода).
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

/// Источник хода (где сейчас фишка) — клетка/резерв/плен в состоянии `ps`.
fn move_source(ps: &GameState, m: Move) -> Sel {
    sel_of(ps.checkers[m.checker].owner, ps.checkers[m.checker].pos)
}

/// Цель хода — куда фишка попадёт после применения `m` к `ps`.
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

/// Клетка-подложка (квадрат со скруглением) заданного CSS-класса.
fn cell_bg(x: f64, y: f64, class: &'static str) -> impl IntoView {
    view! { <rect x=x + 0.08 y=y + 0.08 width=0.84 height=0.84 rx=0.14 class=class /> }
}

/// Кольцо-подсветка поверх клетки.
fn hl_ring(x: f64, y: f64, class: &'static str) -> impl IntoView {
    view! { <rect x=x + 0.03 y=y + 0.03 width=0.94 height=0.94 rx=0.18 fill="none" class=class /> }
}

/// SVG-узел одной клетки сетки `board_glyphs` (визуал, без обработки кликов).
fn svg_cell(r: usize, c: usize, g: Glyph, hl: Hl) -> Option<AnyView> {
    let (x, y) = (c as f64, r as f64);
    let (cx, cy) = (x + 0.5, y + 0.5);
    let base: AnyView = match g {
        Glyph::Empty if hl == Hl::None => return None,
        Glyph::Empty => ().into_any(),
        Glyph::Landmark('+') => cell_bg(x, y, "corner").into_any(),
        Glyph::Landmark('h') => cell_bg(x, y, "home-gate").into_any(),
        Glyph::Landmark('o') => view! { <circle cx=cx cy=cy r=0.3 class="home-slot" /> }.into_any(),
        Glyph::Landmark(_) => cell_bg(x, y, "track").into_any(),
        Glyph::Prison(_) => cell_bg(x, y, "prison").into_any(),
        Glyph::Moon(ch) if ch.is_ascii_digit() => view! {
            <circle cx=cx cy=cy r=0.32 class="moon-field" />
            <text x=cx y=cy class="moon-num">{ch.to_string()}</text>
        }
        .into_any(),
        Glyph::Moon(_) => cell_bg(x, y, "moon").into_any(),
        Glyph::Checker(s) => view! {
            {cell_bg(x, y, "track")}
            <circle cx=cx cy=cy r=0.36 class="checker" fill=side_color(s) />
        }
        .into_any(),
        Glyph::Captive(s) => view! {
            {cell_bg(x, y, "prison")}
            <circle cx=cx cy=cy r=0.34 class="checker" fill=side_color(s) />
            <circle cx=cx cy=cy r=0.42 class="captive-ring" />
        }
        .into_any(),
        Glyph::Overflow => view! { <text x=cx y=cy class="overflow">"+"</text> }.into_any(),
    };
    let ring = match hl {
        Hl::None => None,
        Hl::Source => Some(hl_ring(x, y, "hl-src")),
        Hl::Dest => Some(hl_ring(x, y, "hl-dst")),
        Hl::Selected => Some(hl_ring(x, y, "hl-sel")),
    };
    Some(view! { {base} {ring} }.into_any())
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

    // Авто-бросок в начале хода человека (в т.ч. после дубля/ответа ИИ).
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

    // Завершить ход и пустить ИИ; сбросить состояние выбора.
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

    // Клик по источнику/цели (клетка или лоток).
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

            <svg class="board" viewBox=format!("0 0 {d} {d}", d = BOARD_DIM)>
                {move || {
                    let g = game.get();
                    let pre = prefix.get();
                    let ps = after_prefix(&g, &pre);
                    let active = roll.get().is_some()
                        && g.winner().is_none()
                        && g.state.to_move == human;
                    let cands = if active { step_opts(&turns.get(), &pre) } else { Vec::new() };
                    let cur = sel.get();

                    // Клетки-источники и клетки-цели для подсветки.
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

                    let mut nodes: Vec<AnyView> = Vec::new();
                    for (r, row) in board_glyphs(&ps).into_iter().enumerate() {
                        for (c, g) in row.into_iter().enumerate() {
                            let hl = if sel_cell == Some((r, c)) {
                                Hl::Selected
                            } else if dsts.contains(&(r, c)) {
                                Hl::Dest
                            } else if cur.is_none() && srcs.contains(&(r, c)) {
                                Hl::Source
                            } else {
                                Hl::None
                            };
                            if let Some(v) = svg_cell(r, c, g, hl) {
                                nodes.push(v);
                            }
                        }
                    }
                    // Прозрачные кликабельные прямоугольники над интерактивными клетками.
                    let mut hits: Vec<(usize, usize)> = Vec::new();
                    hits.extend(srcs.iter().copied());
                    hits.extend(dsts.iter().copied());
                    if let Some(rc) = sel_cell { hits.push(rc); }
                    for (r, c) in hits {
                        nodes.push(view! {
                            <rect x=c as f64 y=r as f64 width=1 height=1 class="hit"
                                on:click=move |_| click(Sel::Cell(r, c)) />
                        }.into_any());
                    }
                    nodes
                }}
            </svg>

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
