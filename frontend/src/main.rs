//! Leptos-фронтенд Шеш-Беш: вся игровая логика — движок `sheshbesh` в WASM.
//! Человек играет стороной A против эвристики; ходы выбираются мышкой.

use leptos::prelude::*;
use sheshbesh::{
    BOARD_DIM, DiceRoll, DiceSource, Game, GameState, Glyph, Heuristic, Move, MoveKind, RandomDice,
    Side, board_glyphs, legal_turns,
};

/// Зерно ГПСЧ из времени браузера (на wasm `SystemTime` недоступен).
fn seed() -> u64 {
    (js_sys::Date::now() as u64) ^ 0x9E37_79B9_7F4A_7C15
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

/// Клетка-подложка (квадрат со скруглением) заданного CSS-класса.
fn cell_bg(x: f64, y: f64, class: &'static str) -> impl IntoView {
    view! {
        <rect x=x + 0.08 y=y + 0.08 width=0.84 height=0.84 rx=0.14 class=class />
    }
}

/// SVG-узел одной клетки сетки `board_glyphs` (или `None` для пустой).
fn svg_cell(r: usize, c: usize, g: Glyph) -> Option<AnyView> {
    let (x, y) = (c as f64, r as f64);
    let (cx, cy) = (x + 0.5, y + 0.5);
    let node = match g {
        Glyph::Empty => return None,
        Glyph::Landmark('+') => cell_bg(x, y, "corner").into_any(),
        Glyph::Landmark('h') => cell_bg(x, y, "home-gate").into_any(),
        Glyph::Landmark('o') => view! {
            <circle cx=cx cy=cy r=0.3 class="home-slot" />
        }
        .into_any(),
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
        Glyph::Overflow => view! {
            <text x=cx y=cy class="overflow">"+"</text>
        }
        .into_any(),
    };
    Some(node)
}

fn side_color(s: Side) -> &'static str {
    match s {
        Side::A => "#22d3ee",
        Side::B => "#4ade80",
        Side::C => "#e879f9",
        Side::D => "#facc15",
    }
}

fn kind_label(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::Enter => "ввод",
        MoveKind::Step => "ход",
        MoveKind::EnterMoon => "на Луну",
        MoveKind::MoonAdvance => "Луна+",
        MoveKind::MoonExit => "с Луны",
        MoveKind::EnterPrison => "в Тюрьму",
        MoveKind::PrisonRelease => "из Тюрьмы",
        MoveKind::EnterHome => "в Дом",
        MoveKind::Ransom => "выкуп",
    }
}

/// Человекочитаемое описание варианта хода.
fn describe(seq: &[Move]) -> String {
    if seq.is_empty() {
        return "нет ходов — пропустить".into();
    }
    let pips: u32 = seq.iter().map(|m| u32::from(m.die)).sum();
    let parts: Vec<String> = seq
        .iter()
        .map(|m| format!("{}·{}", m.die, kind_label(m.kind)))
        .collect();
    format!("{pips} очк.: {}", parts.join(", "))
}

/// Уникальные ходы (фишки одного игрока в одной позиции неразличимы).
fn unique_turns(state: &GameState, turns: Vec<Vec<Move>>) -> Vec<Vec<Move>> {
    let mut seen: Vec<(Side, _, u8)> = Vec::new();
    let mut out = Vec::new();
    for t in turns {
        let key = t.first().map(|m| {
            let c = &state.checkers[m.checker];
            (c.owner, c.pos, m.die)
        });
        if key.is_none() || !seen.iter().any(|k| Some(*k) == key) {
            if let Some(k) = key {
                seen.push(k);
            }
            out.push(t);
        }
    }
    out
}

#[component]
fn App() -> impl IntoView {
    let human = Side::A;
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    let game = RwSignal::new(fresh(dice, human));
    let roll = RwSignal::new(None::<DiceRoll>);
    let turns = RwSignal::new(Vec::<Vec<Move>>::new());

    // Бросок костей человеком.
    let on_roll = move |_| {
        let mut r = None;
        dice.update_value(|d| r = Some(d.roll()));
        let r = r.expect("roll");
        let st = game.get_untracked();
        roll.set(Some(r));
        turns.set(unique_turns(&st.state, legal_turns(&st.state, r)));
    };

    // Применение выбранного варианта хода и авто-ответ ИИ.
    let apply = move |seq: Vec<Move>| {
        let Some(r) = roll.get_untracked() else {
            return;
        };
        game.update(|g| {
            g.commit_turn(r, seq, |_, _, _| 0);
            advance_ai(g, dice, human);
        });
        roll.set(None);
        turns.set(Vec::new());
    };

    let on_new = move |_| {
        dice.update_value(|d| *d = RandomDice::from_seed(seed()));
        game.set(fresh(dice, human));
        roll.set(None);
        turns.set(Vec::new());
    };

    view! {
        <div class="wrap">
            <h1>"Шеш-Беш"</h1>
            <div class="status">
                {move || {
                    let g = game.get();
                    if let Some(w) = g.winner() {
                        format!("Победила сторона {}", w.letter())
                    } else {
                        format!("Ход стороны {}", g.state.to_move.letter())
                    }
                }}
            </div>
            <div class="controls">
                <button on:click=on_new>"Новая игра"</button>
                {move || {
                    let g = game.get();
                    let can_roll =
                        g.winner().is_none() && g.state.to_move == human && roll.get().is_none();
                    can_roll.then(|| view! { <button on:click=on_roll>"Бросить кости"</button> })
                }}
                {move || roll.get().map(|r| {
                    let [a, b] = r.values();
                    view! { <span class="dice">{format!("кости: {a} + {b}")}</span> }
                })}
            </div>
            <div class="moves">
                {move || {
                    turns.get().into_iter().map(move |seq| {
                        let label = describe(&seq);
                        view! {
                            <button class="move" on:click=move |_| apply(seq.clone())>
                                {label}
                            </button>
                        }
                    }).collect_view()
                }}
            </div>
            <svg class="board" viewBox=format!("0 0 {d} {d}", d = BOARD_DIM)>
                {move || {
                    board_glyphs(&game.get().state)
                        .into_iter()
                        .enumerate()
                        .flat_map(|(r, row)| {
                            row.into_iter()
                                .enumerate()
                                .filter_map(move |(c, g)| svg_cell(r, c, g))
                        })
                        .collect_view()
                }}
            </svg>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
