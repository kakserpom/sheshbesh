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
    Agent, BOARD_DIM, BOARD_MARGIN, CellKind, Checker, DiceRoll, DiceSource, Die, Game, GameState,
    Heuristic, LinearValue, MoonField, Move, MoveKind, PerimeterIdx, Position, RandomDice, Side,
    apply, best_forced, best_turn, checker_cell, legal_turns, margin_coord,
};
use wasm_bindgen_futures::spawn_local;

// Веса обученных TD(λ)-ценностей (LE f32) под каждый режим — компьютер ходит ими.
// См. `examples/export_model.rs`.
static MODEL_2P: &[u8] = include_bytes!("model.bin");
static MODEL_3P: &[u8] = include_bytes!("model_3p.bin");
static MODEL_4P: &[u8] = include_bytes!("model_4p.bin");
static MODEL_4P_TEAMS: &[u8] = include_bytes!("model_4p_teams.bin");

/// Линейная ценность из встроенных весов под число сторон и режим (команды 2×2).
fn ai_model_for(active_len: usize, teams: bool) -> LinearValue {
    let bytes = match (active_len, teams) {
        (4, true) => MODEL_4P_TEAMS,
        (4, false) => MODEL_4P,
        (3, _) => MODEL_3P,
        _ => MODEL_2P,
    };
    let floats: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    LinearValue::from_floats(&floats)
}

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

/// Имя стороны для комментатора: «Игрок L» (человек) или «ИИ L» (компьютер),
/// окрашенное в цвет стороны. Статус рисуется как HTML (`inner_html`), поэтому
/// возвращаем `<b>`-спан с цветом; текст наш — инъекций не бывает.
fn side_name(side: Side, humans: &[Side]) -> String {
    let who = if humans.contains(&side) {
        "Игрок"
    } else {
        "ИИ"
    };
    format!(
        "<b style=\"color:{}\">{who} {}</b>",
        side_color(side),
        side.letter()
    )
}

/// Реплика об одном применённом ходе (съедание/Дом/Луна/Тюрьма/выкуп). Для
/// обычного шага по периметру реплики нет (`None`) — чтобы не засорять ленту.
fn move_note(before: &GameState, after: &GameState, mv: Move, humans: &[Side]) -> Option<String> {
    let owner = before.checkers[mv.checker].owner;
    let name = side_name(owner, humans);
    // Финиш: этим ходом сторона завела ВСЕ фишки в Дом.
    if !before.has_won(owner) && after.has_won(owner) {
        return Some(format!("{name}: все фишки в Доме — финиш!"));
    }
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
        // Проход сквозь Тюрьму отдельной репликой не сопровождаем (лишний шум).
        MoveKind::PrisonPass | MoveKind::Step | MoveKind::Ransom => return None,
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

/// Команда стороны при игре 2×2 (противоположные стороны — союзники): 0 = A/C, 1 = B/D.
fn team_of(s: Side) -> usize {
    s.index() % 2
}

/// Окончена ли партия с учётом режима (считается напрямую из состояния): 2×2 — когда
/// обе стороны команды финишировали; каждый-сам-за-себя — когда остался один не
/// финишировавший (FFA до `n-1` финишей).
fn game_over(state: &GameState, teams: bool) -> bool {
    if teams && state.active.len() == 4 {
        (0..2).any(|t| {
            let mut members = state.active.iter().filter(|&&s| team_of(s) == t).peekable();
            members.peek().is_some() && members.all(|&s| state.has_won(s))
        })
    } else {
        state.active.iter().filter(|&&s| state.has_won(s)).count()
            >= state.active.len().saturating_sub(1)
    }
}

/// Итоговая реплика партии (если окончена): победившая команда либо места по порядку
/// финиша (`finished` — порядок финиша). До конца партии — `None`.
fn result_msg(
    state: &GameState,
    finished: &[Side],
    teams: bool,
    humans: &[Side],
) -> Option<String> {
    if !game_over(state, teams) {
        return None;
    }
    if teams && state.active.len() == 4 {
        let t = (0..2).find(|&t| {
            state
                .active
                .iter()
                .filter(|&&s| team_of(s) == t)
                .all(|&s| state.has_won(s))
        })?;
        let ms: Vec<String> = state
            .active
            .iter()
            .filter(|&&s| team_of(s) == t)
            .map(|s| s.letter().to_string())
            .collect();
        Some(format!("Команда {} победила! Игра окончена.", ms.join("+")))
    } else {
        let mut order = finished.to_vec();
        for &s in &state.active {
            if !order.contains(&s) {
                order.push(s);
            }
        }
        let places: Vec<String> = order
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}) {}", i + 1, side_name(*s, humans)))
            .collect();
        Some(format!(
            "Победил {}! Места: {}.",
            side_name(order[0], humans),
            places.join(", ")
        ))
    }
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
    // Промежуточные клетки пути (без конечной — её даёт `apply`), чтобы фишка
    // «шагала», а не перепрыгивала.
    let owner = state.checkers[mv.checker].owner;
    let perim = PERIMETER as u16;
    let mids: Vec<Position> = match state.checkers[mv.checker].pos {
        Position::OnTrack { progress } => {
            let target = progress + u16::from(mv.die);
            if target < perim {
                // Обычный шаг по периметру (промежуточные клетки без конечной).
                let mut v: Vec<Position> = (progress + 1..target)
                    .map(|i| Position::OnTrack { progress: i })
                    .collect();
                // Заход на Луну / в Тюрьму: фишка сперва ВСТАЁТ на саму клетку
                // (ворота), а затем улетает на дугу Луны / в каземат. Иначе конечный
                // кадр (Луна/Тюрьма рисуются вне клетки) «проглатывал» эту клетку.
                if matches!(mv.kind, MoveKind::EnterMoon | MoveKind::EnterPrison) {
                    v.push(Position::OnTrack { progress: target });
                }
                v
            } else {
                // Заход в Дом: периметр → клетка ВХОДА в Дом (ворота) → клетки Дома
                // вглубь. Без «второго круга» по периметру.
                let depth = (target - perim) as u8;
                let mut v: Vec<Position> = (progress + 1..perim)
                    .map(|i| Position::OnTrack { progress: i })
                    .collect();
                v.push(Position::OnTrack { progress: 0 }); // ворота = клетка входа
                v.extend((0..depth).map(|d| Position::Home { depth: d }));
                v
            }
        }
        // Продвижение вглубь Дома — по клеткам Дома.
        Position::Home { depth } if mv.kind == MoveKind::HomeAdvance => (depth + 1..depth + mv.die)
            .map(|d| Position::Home { depth: d })
            .collect(),
        // Проход сквозь Тюрьму — от клетки Тюрьмы по периметру.
        Position::Prison { cell } if mv.kind == MoveKind::PrisonPass => {
            let sp = owner.progress_of(cell);
            (1..mv.die)
                .map(|s| Position::OnTrack {
                    progress: sp + u16::from(s),
                })
                .collect()
        }
        _ => Vec::new(),
    };
    for pos in mids {
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

fn fresh(dice: StoredValue<RandomDice>, active: Vec<Side>, teams: bool) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(active.clone(), d)));
    let mut game = game.expect("game started");
    game.state.teams = teams && game.state.active.len() == 4;
    game
}

/// Активные стороны для `n` игроков: 2 — противоположные, 3 — A/B/C, 4 — все.
fn active_for(n: usize) -> Vec<Side> {
    match n {
        2 => vec![Side::A, Side::A.opposite()],
        3 => vec![Side::A, Side::B, Side::C],
        _ => Side::ALL.to_vec(),
    }
}

// --- Режим разработчика: демонстрационные состояния доски ---

/// Сценарий-демо: предельные/наглядные расстановки для проверки отрисовки.
#[derive(Clone, Copy, PartialEq)]
enum Demo {
    Reserve,
    Moon(MoonField),
    Cell,
    Prison,
    Captured,
    Homes,
}

/// Список сценариев и их подписи для панели разработчика.
const DEMOS: &[(Demo, &str)] = &[
    (Demo::Reserve, "Резерв (старт)"),
    (Demo::Moon(MoonField::One), "16 фишек на Луне — поле 1"),
    (Demo::Moon(MoonField::Three), "16 фишек на Луне — поле 3"),
    (Demo::Moon(MoonField::Six), "16 фишек на Луне — поле 6"),
    (Demo::Cell, "16 фишек на углу"),
    (Demo::Prison, "16 фишек в одной Тюрьме"),
    (Demo::Captured, "12 фишек в плену (макс)"),
    (Demo::Homes, "Дома заполнены"),
];

/// Состояние со всеми четырьмя сторонами; позиция каждой фишки задаётся
/// `place(side, k)`, где `k` — её индекс (0..4) среди фишек своей стороны.
fn demo_state(place: impl Fn(Side, usize) -> Position) -> GameState {
    let mut s = GameState::new(Side::ALL.to_vec(), Side::A);
    let mut seen: Vec<Side> = Vec::new();
    for ch in &mut s.checkers {
        let k = seen.iter().filter(|&&o| o == ch.owner).count();
        seen.push(ch.owner);
        ch.pos = place(ch.owner, k);
    }
    s
}

/// Строит состояние для выбранного сценария-демо.
fn demo_game(demo: Demo) -> GameState {
    match demo {
        Demo::Reserve => demo_state(|_, _| Position::Reserve),
        Demo::Moon(field) => demo_state(move |_, _| Position::Moon {
            side: Side::A,
            field,
        }),
        Demo::Cell => {
            // Все 16 на одном углу (углы и Тюрьма — единственные клетки с разными
            // фишками): у каждой стороны свой прогресс до этой абсолютной клетки.
            let corner = Side::A.start_corner();
            demo_state(move |side, _| Position::OnTrack {
                progress: side.progress_of(corner),
            })
        }
        Demo::Prison => {
            let cell = Side::A.local_to_perimeter(LOCAL_PRISON_NEAR);
            demo_state(move |_, _| Position::Prison { cell })
        }
        // Свою фишку в плен не берут: A держит максимум — по 4 от B/C/D (12), а её
        // собственные 4 фишки остаются в резерве.
        Demo::Captured => demo_state(|side, _| {
            if side == Side::A {
                Position::Reserve
            } else {
                Position::Captured { captor: Side::A }
            }
        }),
        Demo::Homes => demo_state(|_, k| Position::Home { depth: k as u8 }),
    }
}

/// Партия-заготовка для демо-анимации: фишка A на дорожке съедает фишку C впереди
/// (остальные — в Домах, чтобы не мешать). Бросок «3 и 1» делает ход однозначным.
fn demo_capture() -> Game {
    let target = Side::A.entry().advance(3);
    let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
    s.checkers.clear();
    s.checkers.push(Checker {
        owner: Side::A,
        pos: Position::OnTrack { progress: 0 },
    });
    for d in 0..3 {
        s.checkers.push(Checker {
            owner: Side::A,
            pos: Position::Home { depth: d },
        });
    }
    s.checkers.push(Checker {
        owner: Side::C,
        pos: Position::OnTrack {
            progress: Side::C.progress_of(target),
        },
    });
    for d in 0..3 {
        s.checkers.push(Checker {
            owner: Side::C,
            pos: Position::Home { depth: d },
        });
    }
    Game::new(s)
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

/// Шаг разноса разноцветных фишек на угловой клетке и на поле Луны (наружу).
const CORNER_GAP: f64 = 0.6;
const MOON_GAP: f64 = 0.5;

/// Точка `k`-го слота: от базовой точки `base` наружу от центра доски с шагом `gap`
/// (`k == 0` — на самой точке). Одноцветные совпадают (рисуются со счётчиком).
fn outward_slot(base: (f64, f64), k: usize, gap: f64) -> (f64, f64) {
    let c = BOARD_DIM as f64 / 2.0;
    let d = (base.0 - c, base.1 - c);
    let len = (d.0 * d.0 + d.1 * d.1).sqrt().max(1e-3);
    let off = k as f64 * gap;
    (base.0 + d.0 / len * off, base.1 + d.1 / len * off)
}

/// Абсолютная клетка-угол, на которой стоит фишка (`None`, если не на углу).
/// Угол — единственная (кроме Тюрьмы) клетка, где уживаются разные фишки.
fn checker_corner(owner: Side, pos: Position) -> Option<PerimeterIdx> {
    if let Position::OnTrack { progress } = pos {
        let abs = owner.entry().advance(progress as usize);
        if cell_kind(abs) == CellKind::Corner {
            return Some(abs);
        }
    }
    None
}

/// Активные стороны, у которых есть фишка на угловой клетке `abs` (в их порядке).
fn corner_sides(state: &GameState, abs: PerimeterIdx) -> Vec<Side> {
    state
        .active
        .iter()
        .copied()
        .filter(|&s| {
            state
                .checkers
                .iter()
                .any(|c| c.owner == s && checker_corner(c.owner, c.pos) == Some(abs))
        })
        .collect()
}

/// Точка слота `k` фишки на угловой клетке (как в Тюрьме, но разнос — наружу).
fn corner_slot_point(coord: (usize, usize), k: usize) -> (f64, f64) {
    outward_slot(center_pt(coord), k, CORNER_GAP)
}

/// Активные стороны с фишкой на поле `field` Луны стороны `side` (в их порядке).
fn moon_sides(state: &GameState, side: Side, field: MoonField) -> Vec<Side> {
    state
        .active
        .iter()
        .copied()
        .filter(|&s| {
            state
                .checkers
                .iter()
                .any(|c| c.owner == s && c.pos == Position::Moon { side, field })
        })
        .collect()
}

/// Точка слота `k` фишки на поле Луны: разнос как на углу. Поля `1`/`6` (у концов
/// дуги) разносятся наружу от центра, а поле `3` (на вершине дуги, у центра) — к
/// центру: иначе стопка налезала бы на сторону Дома.
fn moon_slot_point(side: Side, field: MoonField, k: usize) -> (f64, f64) {
    let gap = if field == MoonField::Three {
        -MOON_GAP
    } else {
        MOON_GAP
    };
    outward_slot(moon_field_point(side, field), k, gap)
}

/// Все три поля Луны (по порядку прохождения).
const MOON_FIELDS: [MoonField; 3] = [MoonField::One, MoonField::Three, MoonField::Six];

/// Бейдж-счётчик одноцветной стопки: стабильный ключ (индекс первой фишки группы —
/// чтобы цифра анимировалась вместе с кружком), точка слота и число.
#[derive(Clone, Copy)]
struct Badge {
    key: usize,
    pt: (f64, f64),
    count: usize,
}

/// Первый индекс и число фишек, удовлетворяющих предикату.
fn first_and_count(state: &GameState, pred: impl Fn(&Checker) -> bool) -> (Option<usize>, usize) {
    let mut first = None;
    let mut count = 0;
    for (i, c) in state.checkers.iter().enumerate() {
        if pred(c) {
            count += 1;
            first.get_or_insert(i);
        }
    }
    (first, count)
}

/// Бейджи-счётчики одноцветных стопок (Тюрьма, углы, Луна): одноцветные фишки
/// накладываются в один кружок, поверх которого рисуется цифра (>1). Ключ бейджа —
/// индекс первой фишки группы, чтобы при анимации цифра «ехала» вместе с кружком,
/// а не телепортировалась (keyed `<For>` + CSS-переход `transform`).
fn stack_counts(state: &GameState) -> Vec<Badge> {
    let mut out = Vec::new();
    let mut push = |first: Option<usize>, pt, count| {
        if count > 1 {
            out.push(Badge {
                key: first.expect("группа непуста"),
                pt,
                count,
            });
        }
    };
    // Тюрьмы: одноцветные пленники в каземате.
    for pg in prison_geoms() {
        for &side in &state.active {
            let (first, count) = first_and_count(state, |c| {
                c.owner == side
                    && matches!(c.pos, Position::Prison { .. })
                    && checker_cell(c.owner, c.pos) == Some(pg.coord)
            });
            let (k, n) = prison_slot(state, side);
            push(first, prison_slot_point(&pg, k, n), count);
        }
    }
    // Углы: одноцветные фишки на одной угловой клетке.
    for &abs in &[0usize, 18, 36, 54] {
        let abs = PerimeterIdx::new(abs);
        let coord = margin_coord(abs);
        for (k, &side) in corner_sides(state, abs).iter().enumerate() {
            let (first, count) = first_and_count(state, |c| {
                c.owner == side && checker_corner(c.owner, c.pos) == Some(abs)
            });
            push(first, corner_slot_point(coord, k), count);
        }
    }
    // Поля Луны: одноцветные фишки на одном поле.
    for side in Side::ALL {
        for field in MOON_FIELDS {
            for (k, &owner) in moon_sides(state, side, field).iter().enumerate() {
                let (first, count) = first_and_count(state, |c| {
                    c.owner == owner && c.pos == Position::Moon { side, field }
                });
                push(first, moon_slot_point(side, field, k), count);
            }
        }
    }
    out
}

/// Реактивный бейдж-счётчик стопки с ключом `key`. Позиция и число читаются из
/// **текущего** состояния (а не из снимка `<For>`), поэтому при движении фишек
/// цифра едет вместе с кружком (CSS-переход `transform`), а не остаётся на месте.
fn badge_view(game: RwSignal<Game>, key: usize) -> impl IntoView {
    let cur = move || {
        stack_counts(&game.get().state)
            .into_iter()
            .find(|b| b.key == key)
    };
    view! {
        <text class="cage-count"
            style=move || cur().map_or(String::new(), |b| {
                format!("transform:translate({:.3}px,{:.3}px)", b.pt.0, b.pt.1)
            })>
            {move || cur().map_or(String::new(), |b| b.count.to_string())}
        </text>
    }
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

/// «Гнездо» фишки — для группировки наложенных на обычной клетке (союзные фишки):
/// угол/Луна/Тюрьма имеют собственную раскладку, сюда не попадают.
#[derive(PartialEq, Eq)]
enum Slot {
    Cell((usize, usize)),
    Off,
}

fn slot_key(pos: Position, owner: Side) -> Slot {
    match pos {
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
    // Угол — единственная (кроме Тюрьмы) клетка периметра с разными фишками: рисуем
    // как в Тюрьме (одноцветные — в один кружок со счётчиком), но цвета — НАРУЖУ.
    if let Some(abs) = checker_corner(ch.owner, ch.pos) {
        let coord = checker_cell(ch.owner, ch.pos).expect("corner cell");
        let sides = corner_sides(state, abs);
        let k = sides.iter().position(|&s| s == ch.owner).unwrap_or(0);
        let (x, y) = corner_slot_point(coord, k);
        return (x, y, true);
    }
    // Поле Луны — тоже стопка из разных фишек: так же, как на углу.
    if let Position::Moon { side, field } = ch.pos {
        let sides = moon_sides(state, side, field);
        let k = sides.iter().position(|&s| s == ch.owner).unwrap_or(0);
        let (x, y) = moon_slot_point(side, field, k);
        return (x, y, true);
    }
    // На клетке (в т.ч. при проходе сквозь клетку Тюрьмы) — по центру клетки.
    // В каземат попадают только настоящие пленники (обработаны выше).
    let coord = checker_cell(ch.owner, ch.pos).expect("on-board cell");
    let (base, visible) = (center_pt(coord), true);
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
            CellKind::HomeEntrance => match home_side(state, coord) {
                // Клетка входа в Дом активной стороны — в её цвете.
                Some(side) => {
                    let (r, c) = coord;
                    nodes.push(
                        view! {
                            <rect x=c as f64 + 0.08 y=r as f64 + 0.08 width=0.84 height=0.84 rx=0.14
                                class="home-gate" stroke=side_color(side) />
                        }
                        .into_any(),
                    );
                }
                // Дом неактивной стороны — обычная клетка (без золотой рамки).
                None => nodes.push(cell_rect(coord, "track").into_any()),
            },
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

// --- Обучение (туториал) ---

/// Один шаг туториала = граница хода: позиция `before` в начале шага и заранее
/// заданный (без случайности) бросок `roll` для хода из этой позиции в следующую.
/// Ход вычисляет сам движок (`legal_turns`), поэтому он гарантированно легален и
/// соблюдает правило максимального хода. `None` в `roll` — шаг-пояснение без хода.
struct Lesson {
    title: &'static str,
    text: &'static str,
    /// Позиция в начале хода игрока (сторона A).
    before: GameState,
    /// Бросок этого хода (`None` — шаг-пояснение без хода).
    roll: Option<DiceRoll>,
    /// Ход игрока (под-ходы по костям), посчитанный движком.
    moves: Vec<Move>,
    /// Ход соперника (C), который автоматически проигрывается после хода игрока,
    /// прежде чем дойти до следующего шага — чтобы стороны ходили по очереди.
    opp: Option<OppTurn>,
}

/// Авто-ход соперника между ходами игрока.
struct OppTurn {
    roll: DiceRoll,
    moves: Vec<Move>,
}

/// Пара значений → бросок двух костей.
fn dr(a: u8, b: u8) -> DiceRoll {
    DiceRoll::new(Die::new(a).expect("1..6"), Die::new(b).expect("1..6"))
}

/// Туториал как **непрерывная партия** A против C по заранее заданным броскам: одна
/// фишка A проходит путь (ввод→Тюрьма→Луна→съедание), а между её ходами соперник C
/// двигает свою фишку-«мувера» (`checkers[5]`), не мешая пути; фишка-мишень C
/// (`checkers[4]`) стоит до съедания. Все ходы считает движок (`legal_turns`),
/// поэтому они легальны и соблюдают правило максимального хода.
fn lessons() -> Vec<Lesson> {
    use Side::{A, C};
    // Стартовая позиция непрерывной партии.
    let mut s = GameState::new(vec![A, C], A);
    s.checkers.clear();
    s.checkers.push(Checker { owner: A, pos: Position::Reserve }); // A[0] — демо-фишка
    for d in 1..4u8 {
        s.checkers.push(Checker { owner: A, pos: Position::Home { depth: d } }); // A[1..3] инертны
    }
    s.checkers.push(Checker { owner: C, pos: Position::OnTrack { progress: 64 } }); // C[4] мишень
    s.checkers.push(Checker { owner: C, pos: Position::OnTrack { progress: 5 } }); // C[5] мувер
    for d in 1..3u8 {
        s.checkers.push(Checker { owner: C, pos: Position::Home { depth: d } }); // C[6,7]
    }

    // Ход игрока (A) — единственный легальный (движок форсирует путь).
    let a_turn = |st: &GameState, r: DiceRoll| legal_turns(st, r).into_iter().next().unwrap_or_default();
    // Ход соперника (C) — максимальный, двигающий ТОЛЬКО мувера C[5], иначе любой.
    let c_turn = |st: &GameState, r: DiceRoll| {
        let turns = legal_turns(st, r);
        turns
            .iter()
            .find(|t| !t.is_empty() && t.iter().all(|m| m.checker == 5))
            .or_else(|| turns.first())
            .cloned()
            .unwrap_or_default()
    };

    // Сценарий: (бросок A, бросок C после, заголовок, текст). Последний шаг — без C.
    let script: [(DiceRoll, Option<DiceRoll>, &str, &str); 6] = [
        (dr(6, 5), Some(dr(3, 2)), "Цель и ввод",
         "У каждого 4 фишки в резерве; цель — обойти доску против часовой и собрать все в своём Доме. \
          Выпало 6 и 5: по 6 фишка входит на точку входа, затем оставшейся 5 доходит до Тюрьмы. \
          Нажмите фишку, затем клетку (по одной кости), или ▶."),
        (dr(4, 6), Some(dr(4, 1)), "Тюрьма и Луна",
         "Тюрьма держит фишку: выйти можно только выбросив 4 — она выпала. Затем оставшейся 6 фишка \
          идёт дальше и попадает на Луну. Сделайте оба под-хода."),
        (dr(1, 2), Some(dr(3, 1)), "Луна",
         "Луна — короткий безопасный путь по полям 1·3·6 (съесть нельзя). Выпало 1 — переход с поля «1» \
          на «3». Вторая кость (2) не играется: на Луне нужно точное значение. Сделайте ход."),
        (dr(3, 2), Some(dr(2, 1)), "Дорожка Луны",
         "Выпало 3 — переход с поля «3» на «6». Осталось выбросить 6, чтобы сойти с Луны."),
        (dr(6, 1), Some(dr(4, 2)), "Сход с Луны",
         "По 6 фишка сходит с Луны на доску, заметно ближе к Дому. Впереди — фишка соперника."),
        (dr(2, 1), None, "Съедание",
         "Встав на клетку с фишкой соперника, фишка съедает её — пленная уходит к нашему Дому \
          (с красной обводкой). На углах, Луне и в Тюрьме не едят."),
    ];

    let mut out = Vec::new();
    let mut g = Game::new(s);
    for (a_roll, c_roll, title, text) in script {
        let before = g.state.clone();
        let moves = a_turn(&g.state, a_roll);
        g.commit_turn(a_roll, moves.clone(), |_, _, _| 0);
        let opp = c_roll.map(|cr| {
            let cm = c_turn(&g.state, cr);
            g.commit_turn(cr, cm.clone(), |_, _, _| 0);
            OppTurn { roll: cr, moves: cm }
        });
        out.push(Lesson { title, text, before, roll: Some(a_roll), moves, opp });
    }

    // Шаги-пояснения без хода.
    out.push(Lesson {
        title: "Плен и выкуп",
        text: "Съеденная фишка соперника — в плену у нашего Дома. Чтобы вернуть свою пленную, её \
               владелец выбрасывает 6 (выкуп) и обязывает соперника сходить на 6, затем ещё 6 — для \
               повторного ввода. Дальше →.",
        before: g.state.clone(),
        roll: None,
        moves: Vec::new(),
        opp: None,
    });
    let mut home = g.state.clone();
    home.checkers[0].pos = Position::Home { depth: 0 }; // финал: демо-фишка в Доме
    out.push(Lesson {
        title: "Дом и победа",
        text: "Дойдя до Дома, фишка заходит ровно по броску — по одной в клетку; перепрыгивать занятые \
               клетки Дома нельзя (но можно идти глубже). Чужие фишки в Дом не пускают. Заведите все 4 \
               фишки в Дом — и вы выиграли!",
        before: home,
        roll: None,
        moves: Vec::new(),
        opp: None,
    });
    out
}

/// Кадры броска кости БЕЗ движения фишки: кубики кувыркаются (3D), затем застывают.
/// Бросок происходит ДО хода игрока (как в реальной игре).
fn roll_only_frames(state: &GameState, roll: DiceRoll) -> Vec<Frame> {
    vec![
        Frame {
            state: state.clone(),
            roll: Some(roll),
            hold: ROLL_ANIM_MS,
            rolling: true,
            note: None,
        },
        Frame {
            state: state.clone(),
            roll: Some(roll),
            hold: HOLD_ROLL_MS,
            rolling: false,
            note: None,
        },
    ]
}

#[component]
fn App() -> impl IntoView {
    let dice = StoredValue::new(RandomDice::from_seed(seed()));
    // Поколение партии: растёт при старте/остановке игры. Любая запущенная анимация
    // (spawn_local) запоминает своё поколение и прекращается, когда оно устарело, —
    // иначе старая партия продолжала бы крутить кадры поверх новой.
    let epoch = StoredValue::new(0u64);
    // Настройки: число игроков, типы сторон и (для 4) командный режим 2×2.
    let players = RwSignal::new(2usize);
    let humans = RwSignal::new(vec![Side::A]);
    let teams = RwSignal::new(false);
    let started = RwSignal::new(false);
    // Режим разработчика: экран демонстрации состояний и анимаций (вне партии).
    let dev = RwSignal::new(false);
    // Справка по правилам и пошаговое обучение (туториал) — экраны из меню настроек.
    let rules = RwSignal::new(false);
    let tutorial = RwSignal::new(false);
    let lesson_idx = RwSignal::new(0usize);
    let lessons_sv = StoredValue::new(lessons());
    // Выбрана ли демонстрационная фишка (первый клик хода в туториале).
    let tut_sel = RwSignal::new(false);
    // Сколько под-ходов текущего шага сыграно (ход = последовательность по костям;
    // в туториале игрок проигрывает их по одному — как в реальной игре).
    let tut_played = RwSignal::new(0usize);
    // Стороны, финишировавшие (все фишки в Доме) — в порядке финиша.
    let finished = RwSignal::new(Vec::<Side>::new());
    // Пауза: блокирует автоход ИИ и авто-бросок; анимация замирает между кадрами.
    let paused = RwSignal::new(false);
    let game = RwSignal::new(fresh(dice, active_for(2), false));
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

    // Следим за финишами: запоминаем порядок финиша, а по окончании партии (с учётом
    // режима) выводим итоговую реплику.
    Effect::new(move |_| {
        let g = game.get();
        // В режиме разработчика и обучении доска несёт демо-состояния — не финиши.
        if dev.get_untracked() || tutorial.get_untracked() {
            return;
        }
        let mut newly = Vec::new();
        finished.with_untracked(|f| {
            for &s in &g.state.active {
                if g.state.has_won(s) && !f.contains(&s) {
                    newly.push(s);
                }
            }
        });
        if !newly.is_empty() {
            finished.update(|f| f.extend(newly));
        }
        if let Some(msg) = result_msg(
            &g.state,
            &finished.get_untracked(),
            teams.get_untracked(),
            &humans.get_untracked(),
        ) {
            herald.set(msg);
        }
    });

    // Проигрывает кадры с паузами, обновляя доску и кости; в конце снимает блокировку.
    // На паузе замирает между кадрами (опрос `paused`).
    let play = move |frames: Vec<Frame>| {
        if frames.is_empty() {
            return;
        }
        let era = epoch.get_value();
        animating.set(true);
        spawn_local(async move {
            for frame in frames {
                // Новая партия началась — бросаем эту анимацию (не трогая сигналы).
                if epoch.get_value() != era {
                    return;
                }
                while paused.get_untracked() {
                    if epoch.get_value() != era {
                        return;
                    }
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
            if epoch.get_value() != era {
                return;
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
            || game_over(&g.state, teams.get_untracked())
            || hs.contains(&g.state.to_move)
        {
            return;
        }
        let mut gg = g.clone();
        let mut roll_v = None;
        dice.update_value(|d| roll_v = Some(d.roll()));
        let r = roll_v.expect("roll");
        let t = legal_turns(&gg.state, r);
        // Модель под текущий режим (число сторон + команды).
        let m = ai_model_for(gg.state.active.len(), teams.get_untracked());
        let idx = best_turn(&m, &gg.state, &t).min(t.len() - 1);
        let played = t[idx].clone();
        let mut frames = Vec::new();
        commit_frames(&mut frames, &mut gg, r, played, &hs, true, |s, c, o| {
            best_forced(&m, s, c, o)
        });
        frames.push(Frame {
            state: gg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: None,
        });
        play(frames);
    });

    // Старт партии: держим паузу-заставку (видно «Игра началась»), затем снимаем
    // блокировку — дальше ход подхватят Effect'ы (авто-бросок человека / ход ИИ).
    let kickoff = move || {
        let era = epoch.get_value();
        animating.set(true);
        spawn_local(async move {
            TimeoutFuture::new(INTRO_MS).await;
            if epoch.get_value() != era {
                return;
            }
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
            && !game_over(&g.state, teams.get_untracked())
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
            let era = epoch.get_value();
            animating.set(true);
            spawn_local(async move {
                herald.set(format!("{name} бросает кости…"));
                roll.set(Some(r));
                rolling.set(true);
                TimeoutFuture::new(ROLL_ANIM_MS).await;
                // Новая партия началась за время броска — прекращаем.
                if epoch.get_value() != era {
                    return;
                }
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
            note: None,
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
            || game_over(&g.state, teams.get_untracked())
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
        // Новое поколение — глушит анимации предыдущей партии (иначе две идут разом).
        epoch.update_value(|e| *e += 1);
        let active = active_for(players.get_untracked());
        dice.update_value(|d| *d = RandomDice::from_seed(seed()));
        roll.set(None);
        turns.set(Vec::new());
        prefix.set(Vec::new());
        sel.set(None);
        finished.set(Vec::new());
        let g = fresh(dice, active, teams.get_untracked());
        let hs = humans.get_untracked();
        herald.set(format!(
            "Игра началась! Ходит {}.",
            side_name(g.state.to_move, &hs)
        ));
        game.set(g);
        started.set(true);
        kickoff();
    };
    // Назад к настройкам / «Закончить игру»: глушим анимации текущей партии и
    // сбрасываем переходные сигналы, чтобы новая партия стартовала с чистого листа.
    let to_settings = move |_| {
        epoch.update_value(|e| *e += 1);
        animating.set(false);
        rolling.set(false);
        roll.set(None);
        started.set(false);
    };

    // Демо-анимация (режим разработчика): пошаговый ход со съеданием через тот же
    // конвейер кадров, что и в партии (бросок, шаги по клеткам, реплики ведущего).
    let anim_demo = move |_| {
        if animating.get_untracked() {
            return;
        }
        epoch.update_value(|e| *e += 1);
        let mut gg = demo_capture();
        let r = DiceRoll::new(Die::new(3).expect("3"), Die::new(1).expect("1"));
        let t = legal_turns(&gg.state, r);
        if t.is_empty() {
            return;
        }
        let idx = Heuristic.choose_turn(&gg.state, &t).min(t.len() - 1);
        let played = t[idx].clone();
        game.set(Game::new(gg.state.clone()));
        roll.set(None);
        let mut frames = Vec::new();
        commit_frames(&mut frames, &mut gg, r, played, &[], true, |s, c, o| {
            Heuristic.choose_forced(s, c, o)
        });
        frames.push(Frame {
            state: gg.state.clone(),
            roll: None,
            hold: HOLD_STEP_MS,
            rolling: false,
            note: Some("Демо завершено.".to_string()),
        });
        play(frames);
    };

    // Открыть/закрыть экран разработчика, показав состояние-заглушку.
    let open_dev = move |_| {
        epoch.update_value(|e| *e += 1);
        animating.set(false);
        game.set(Game::new(demo_game(Demo::Reserve)));
        roll.set(None);
        herald.set("Режим разработчика: выберите сценарий.".to_string());
        dev.set(true);
    };

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
            {move || (!started.get() && !dev.get() && !rules.get() && !tutorial.get()).then(|| view! {
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
                    // Режим вчетвером: команды 2×2 (A+C vs B+D) или каждый сам за себя.
                    {move || (players.get() == 4).then(|| view! {
                        <div class="set-row">
                            <span>"Режим:"</span>
                            <button class:on=move || teams.get() on:click=move |_| teams.set(true)>
                                "Команды 2×2"
                            </button>
                            <button class:on=move || !teams.get() on:click=move |_| teams.set(false)>
                                "Каждый сам"
                            </button>
                        </div>
                    })}
                    <button class="primary" on:click=start_game>"Начать игру"</button>
                    <div class="set-row">
                        <button on:click=move |_| {
                            epoch.update_value(|e| *e += 1);
                            animating.set(false);
                            rolling.set(false);
                            lesson_idx.set(0);
                            tut_sel.set(false);
                            tut_played.set(0);
                            lessons_sv.with_value(|ls| {
                                game.set(Game::new(ls[0].before.clone()));
                                // Первый бросок — до первого хода.
                                match ls[0].roll {
                                    Some(r) => play(roll_only_frames(&ls[0].before, r)),
                                    None => roll.set(None),
                                }
                            });
                            tutorial.set(true);
                        }>"🎓 Обучение"</button>
                        <button on:click=move |_| rules.set(true)>"📖 Правила"</button>
                        <button class="icon-btn" title="Режим разработчика" on:click=open_dev>"🛠"</button>
                    </div>
                </div>
            })}

            // Экран правил: структурированный справочник (самобытные правила).
            {move || rules.get().then(|| view! {
                <div class="controls">
                    <button on:click=move |_| rules.set(false)>"← Назад"</button>
                </div>
                <div class="rules">
                    <h2>"Шеш-Беш — правила"</h2>
                    <p class="lead">"Самобытный вариант нард. Играют вдвоём или вчетвером; каждый владеет одной стороной квадрата и её Домом."</p>

                    <h3>"Цель"</h3>
                    <p>"Провести все 4 свои фишки полный круг по доске (против часовой стрелки) и завести их в свой Дом. Чужие фишки в Дом попасть не могут. Кто первым собрал Дом — победил. Вчетвером бывает командная игра 2×2 (победа, когда обе стороны команды собрали Дом) или каждый сам за себя."</p>

                    <h3>"Доска"</h3>
                    <ul>
                        <li>"Квадрат; у каждой из 4 сторон — 19 клеток (углы общие для двух сторон)."</li>
                        <li>"Посередине каждой стороны — "<b>"Дом"</b>" из 4 клеток, идущих внутрь квадрата."</li>
                        <li>"Все фишки стартуют в резерве у своего Дома и вводятся в игру."</li>
                    </ul>

                    <h3>"Ход"</h3>
                    <ul>
                        <li>"За ход бросают две кости. Значения можно отнести к двум разным фишкам (по значению на каждую) или объединить на одну."</li>
                        <li>"«Выбросить число» всегда значит значение на "<b>"одной"</b>" кости, а не сумму."</li>
                        <li>"Ввод фишки в игру — по "<b>"6"</b>". На точку входа нельзя поставить вторую свою фишку; чужую там можно съесть."</li>
                        <li>"Дубль даёт право на ещё один полный ход."</li>
                        <li>"Правило максимального хода: из вариантов выбирают тот, что использует больше очков."</li>
                        <li>"Нельзя перепрыгивать занятые клетки — исключение только углы (через занятый угол проходить можно)."</li>
                    </ul>

                    <h3>"Особые клетки"</h3>
                    <ul>
                        <li><b style="color:#93c5fd">"Луна"</b>" (2-я клетка от угла) — короткий путь. Фишка улетает на внутреннюю дорожку полей 1·3·6: с «1» двигает выброс 1, с «3» — 3, с «6» — 6, после чего возвращается на доску ближе к Дому. Луна полностью безопасна. Чужие Луны срезают путь вперёд, а вот своя Луна (она у самого Дома) выводит фишку в начало круга — это второй круг."</li>
                        <li><b>"Тюрьма"</b>" (4-я клетка от угла) — изолирует фишку: выйти можно только выбросив 4 (фишка остаётся на месте). Удобно блокировать чужие фишки. Если фишка попала на Тюрьму этим же ходом и есть вторая кость — можно пройти мимо."</li>
                        <li><b>"Углы"</b>" — общие клетки; на них фишки не едят и через них можно проходить."</li>
                    </ul>

                    <h3>"Съедание, плен и выкуп"</h3>
                    <ul>
                        <li>"Встав на клетку с чужой фишкой, вы её съедаете — она попадает к вам в плен."</li>
                        <li>"Съедания нет на углах, Луне и в Тюрьме."</li>
                        <li>"Чтобы вернуть пленную фишку, нужно выбросить 6 (выкуп), и тогда соперник обязан сделать ответный ход на 6 клеток. Затем нужна ещё одна 6 для повторного ввода."</li>
                    </ul>

                    <h3>"Дом"</h3>
                    <p>"Заход в Дом — ровно по броску, по одной фишке в клетку; перепрыгивать занятые клетки Дома нельзя (но фишка в Доме может идти глубже, освобождая место). Перебор за пределы Дома нелегален. При этом второй круг возможен через Луну: своя Луна стоит у самого Дома, и фишка с неё выходит в начало круга — приходится идти ещё раз."</p>
                </div>
            })}

            // Экран обучения: пошаговые уроки с анимацией. Доска и кубик — те же, что
            // в игре (сигнал `game` + `play` + keyed-фишки со скольжением).
            {move || tutorial.get().then(|| {
                let total = lessons_sv.with_value(Vec::len);
                // Завершает кадры хода игрока: проигрывает авто-ход соперника (если есть)
                // — чтобы стороны ходили по очереди — затем бросает кости СЛЕДУЮЩЕГО
                // шага (бросок до хода) и переходит к нему.
                let finish_step = move |frames: &mut Vec<Frame>, after: &GameState, cur: usize| {
                    lessons_sv.with_value(|ls| {
                        // Ход соперника: его бросок + пошаговое движение его фишки.
                        // В этих кадрах `to_move = C`, чтобы кости рисовались у Дома C.
                        let end_state = match &ls[cur].opp {
                            Some(opp) => {
                                let mut st = after.clone();
                                st.to_move = Side::C;
                                frames.extend(roll_only_frames(&st, opp.roll));
                                for mv in &opp.moves {
                                    st = apply_with_frames(frames, st, *mv, opp.roll, &[]);
                                }
                                st.to_move = Side::A; // вернули очередь игроку
                                st
                            }
                            None => after.clone(),
                        };
                        if cur + 1 < ls.len() {
                            match ls[cur + 1].roll {
                                Some(nr) => frames.extend(roll_only_frames(&end_state, nr)),
                                None => frames.push(Frame { state: end_state, roll: None, hold: HOLD_STEP_MS, rolling: false, note: None }),
                            }
                            lesson_idx.set(cur + 1);
                        }
                    });
                    tut_played.set(0);
                };
                // Проиграть `count` под-ходов текущего шага начиная с уже сыгранных
                // (1 — по клику на клетку; usize::MAX — весь оставшийся ход по ▶).
                let play_submoves = move |count: usize| {
                    if animating.get_untracked() {
                        return;
                    }
                    let cur = lesson_idx.get_untracked();
                    lessons_sv.with_value(|ls| {
                        match ls[cur].roll {
                            Some(r) => {
                                let chosen = &ls[cur].moves;
                                let k = tut_played.get_untracked();
                                let end = k.saturating_add(count).min(chosen.len());
                                let mut state = game.get_untracked().state.clone();
                                let mut frames = Vec::new();
                                for mv in &chosen[k..end] {
                                    state = apply_with_frames(&mut frames, state, *mv, r, &[]);
                                }
                                tut_sel.set(false);
                                if end >= chosen.len() {
                                    finish_step(&mut frames, &state, cur);
                                } else {
                                    tut_played.set(end);
                                }
                                play(frames);
                            }
                            None => {
                                // Шаг-пояснение без хода — просто показываем следующий.
                                if cur + 1 < ls.len() {
                                    let nb = ls[cur + 1].before.clone();
                                    lesson_idx.set(cur + 1);
                                    tut_played.set(0);
                                    game.set(Game::new(nb.clone()));
                                    match ls[cur + 1].roll {
                                        Some(nr) => play(roll_only_frames(&nb, nr)),
                                        None => roll.set(None),
                                    }
                                }
                            }
                        }
                    });
                };
                view! {
                    <div class="status">
                        <span class="herald">{move || {
                            let i = lesson_idx.get();
                            lessons_sv.with_value(|ls| format!("Шаг {}/{total}: {}", i + 1, ls[i].title))
                        }}</span>
                    </div>
                    <div class="controls">
                        <button on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); rolling.set(false); tutorial.set(false); }>"← Настройки"</button>
                    </div>
                    <div class="board-area">
                    <div class="board-wrap">
                    <svg class="board" viewBox=format!(
                        "{o} {o} {s} {s}",
                        o = BOARD_MARGIN as f64 - RESERVE_PAD,
                        s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD,
                    )>
                        {move || static_board(&game.get().state)}
                        <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                            <circle
                                r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Reserve | Position::Captured { .. }) { 0.3 } else { 0.36 }
                                class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Captured { .. }) { "piece captive" } else { "piece" }
                                fill=move || side_color(game.get().state.checkers[i].owner)
                                cx=move || checker_xy(&game.get().state, i).0
                                cy=move || checker_xy(&game.get().state, i).1
                                opacity=move || if checker_xy(&game.get().state, i).2 { 1.0 } else { 0.0 }
                            />
                        </For>
                        <For each=move || stack_counts(&game.get().state) key=|b| b.key let:b>
                            {badge_view(game, b.key)}
                        </For>
                        // Интерактивный ход кликами ПО ОДНОМУ под-ходу (по каждой кости):
                        // фишка → клетка её следующего под-хода. Не во время движения.
                        {move || (!animating.get()).then(|| {
                            let cur = lesson_idx.get().min(total - 1);
                            lessons_sv.with_value(|ls| {
                                let mut nodes: Vec<AnyView> = Vec::new();
                                if ls[cur].roll.is_some() {
                                    let chosen = &ls[cur].moves;
                                    let k = tut_played.get();
                                    if k < chosen.len() {
                                        let st = game.get().state;
                                        let mv = chosen[k];
                                        let after = apply(&st, mv);
                                        let moved = mv.checker;
                                        let (sx, sy, _) = checker_xy(&st, moved);
                                        let sel = tut_sel.get();
                                        let src_cls = if sel { "hl-sel" } else { "hl-src" };
                                        nodes.push(view! { <circle cx=sx cy=sy r=0.47 fill="none" class=src_cls /> }.into_any());
                                        nodes.push(view! { <circle cx=sx cy=sy r=0.5 class="hit" on:click=move |_| tut_sel.set(true) /> }.into_any());
                                        if sel {
                                            // Цель: для входа на Луну — клетка-вход (на
                                            // периметре), для остального — куда реально
                                            // встанет фишка (поле дорожки/клетка/каземат).
                                            let (tx, ty) = match (mv.kind, after.checkers[moved].pos) {
                                                (MoveKind::EnterMoon, Position::Moon { side, .. }) => {
                                                    center_pt(margin_coord(side.local_to_perimeter(LOCAL_MOON)))
                                                }
                                                // Вход в Тюрьму — на саму клетку, а не в каземат.
                                                (MoveKind::EnterPrison, Position::Prison { cell }) => {
                                                    center_pt(margin_coord(cell))
                                                }
                                                _ => {
                                                    let (x, y, _) = checker_xy(&after, moved);
                                                    (x, y)
                                                }
                                            };
                                            nodes.push(view! { <circle cx=tx cy=ty r=0.47 fill="none" class="hl-dst" /> }.into_any());
                                            nodes.push(view! { <circle cx=tx cy=ty r=0.5 class="hit" on:click=move |_| play_submoves(1) /> }.into_any());
                                        }
                                    }
                                }
                                nodes
                            })
                        })}
                    </svg>
                    // Две кости у Дома ХОДЯЩЕЙ стороны (в кадрах хода соперника `to_move`
                    // = C) — как в реальной игре, через тот же сигнал `roll`.
                    {move || roll.get().map(|r| {
                        let [a, b] = r.values();
                        let side = game.get().state.to_move;
                        let (ax, ay) = outside_anchor(side, DICE_OUT);
                        let (out, _) = reserve_axes(side);
                        let vertical = out.0 != 0.0;
                        let vb_o = BOARD_MARGIN as f64 - RESERVE_PAD;
                        let vb_s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD;
                        let fx = (ax - vb_o) / vb_s * 100.0;
                        let fy = (ay - vb_o) / vb_s * 100.0;
                        let inner = if rolling.get() {
                            view! { <span class="dice rolling">{die3d(a)} {die3d(b)}</span> }.into_any()
                        } else {
                            view! { <span class="dice">{die_face(a)} {die_face(b)}</span> }.into_any()
                        };
                        view! { <div class="board-dice" class:vert=vertical style=format!("left:{fx:.2}%;top:{fy:.2}%")>{inner}</div> }
                    })}
                    // Навигация по шагам — стрелки по краям доски.
                    <button class="nav-arrow left" title="Назад"
                        on:click=move |_| {
                            // Назад — мгновенно: ставим позицию и показываем её кости.
                            epoch.update_value(|e| *e += 1);
                            animating.set(false);
                            rolling.set(false);
                            tut_sel.set(false);
                            tut_played.set(0);
                            let i = lesson_idx.get_untracked().saturating_sub(1);
                            lesson_idx.set(i);
                            lessons_sv.with_value(|ls| {
                                game.set(Game::new(ls[i].before.clone()));
                                roll.set(ls[i].roll);
                            });
                        }>"◀"</button>
                    <button class="nav-arrow right" title="Далее"
                        on:click=move |_| play_submoves(usize::MAX)>"▶"</button>
                    </div>
                    </div>
                    <p class="lesson-text">{move || {
                        lessons_sv.with_value(|ls| ls[lesson_idx.get().min(total - 1)].text.to_string())
                    }}</p>
                    {move || lessons_sv.with_value(|ls| {
                        let cur = lesson_idx.get().min(total - 1);
                        ls[cur].roll.is_some().then(|| view! {
                            <p class="lesson-hint">"👆 Сделайте ход сами: нажмите на фишку, затем на подсвеченную клетку. Или жмите ▶."</p>
                        })
                    })}
                }
            })}

            // Экран разработчика: панель сценариев + демо-анимация + read-only доска.
            {move || dev.get().then(|| view! {
                <div class="status">
                    <span class="herald" inner_html=move || herald.get()></span>
                </div>
                <div class="controls dev-controls">
                    <button on:click=move |_| { epoch.update_value(|e| *e += 1); animating.set(false); dev.set(false); }>"← Настройки"</button>
                    {DEMOS.iter().map(|&(d, label)| view! {
                        <button on:click=move |_| {
                            // Обрываем демо-анимацию, если идёт, и показываем сценарий.
                            epoch.update_value(|e| *e += 1);
                            animating.set(false);
                            game.set(Game::new(demo_game(d)));
                            roll.set(None);
                            herald.set(label.to_string());
                        }>{label}</button>
                    }).collect_view()}
                    <button on:click=anim_demo>"▶ Анимация: ход и съедание"</button>
                </div>
                <div class="board-area">
                <div class="board-wrap">
                <svg class="board" viewBox=format!(
                    "{o} {o} {s} {s}",
                    o = BOARD_MARGIN as f64 - RESERVE_PAD,
                    s = (BOARD_DIM - 2 * BOARD_MARGIN) as f64 + 2.0 * RESERVE_PAD,
                )>
                    {move || static_board(&game.get().state)}
                    <For each=move || 0..game.get().state.checkers.len() key=|i| *i let:i>
                        <circle
                            r=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Reserve | Position::Captured { .. }) { 0.3 } else { 0.36 }
                            class=move || if matches!(game.get().state.checkers[i].pos, Position::Prison { .. } | Position::Captured { .. }) { "piece captive" } else { "piece" }
                            fill=move || side_color(game.get().state.checkers[i].owner)
                            cx=move || checker_xy(&game.get().state, i).0
                            cy=move || checker_xy(&game.get().state, i).1
                            opacity=move || if checker_xy(&game.get().state, i).2 { 1.0 } else { 0.0 }
                        />
                    </For>
                    <For each=move || stack_counts(&game.get().state) key=|b| b.key let:b>
                        {badge_view(game, b.key)}
                    </For>
                </svg>
                </div>
                </div>
            })}

            // Игровой экран после старта.
            {move || started.get().then(|| view! {
            <div class="status">
                <span class="herald" inner_html=move || herald.get()></span>
            </div>
            <div class="controls">
                <button class="icon-btn" title="Закончить игру" on:click=to_settings>"⏹"</button>
                <button class="icon-btn" class:on=move || paused.get()
                    title=move || if paused.get() { "Продолжить" } else { "Пауза" }
                    on:click=move |_| paused.update(|p| *p = !*p)>
                    {move || if paused.get() { "▶" } else { "⏸" }}
                </button>
                {move || {
                    let g = game.get();
                    let no_moves = roll.get().is_some()
                        && !game_over(&g.state, teams.get())
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

                // Счётчики одноцветных стопок (Тюрьма, углы, Луна) — keyed, чтобы
                // цифра ехала вместе с кружком (CSS-переход `transform`), а не прыгала.
                <For each=move || stack_counts(&game.get().state) key=|b| b.key let:b>
                    {badge_view(game, b.key)}
                </For>

                {move || {
                    let g = game.get();
                    let pre = prefix.get();
                    let ps = g.state.clone(); // доска уже продвинута применёнными частями
                    let mover = g.state.to_move;
                    let active = !animating.get()
                        && roll.get().is_some()
                        && !game_over(&g.state, teams.get())
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
                        // Пока кости крутятся, дубль золотом НЕ подсвечиваем — только
                        // после остановки (иначе результат «виден» раньше времени).
                        view! { <span class="dice rolling">{die3d(a)} {die3d(b)}</span> }.into_any()
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
