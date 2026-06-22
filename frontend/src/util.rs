use crate::geom::{moon_arc_point, moon_field_t};
use leptos::prelude::*;
use sheshbesh::board::PERIMETER;
use sheshbesh::{
    DiceRoll, Game, GameState, Move, MoveKind, Position, RandomDice, Side, TurnOutcome, apply,
};

/// Технический лог одного хода для воспроизведения багов (печатается в консоль только
/// в отладочной сборке): сторона, бросок и ФАКТИЧЕСКИ применённые ходы (`applied` —
/// включая ответы захватчика при выкупе). Вместе с залогированным seed этого хватает,
/// чтобы воспроизвести партию.
pub(crate) fn dbg_log_turn(outcome: &TurnOutcome) {
    let [a, b] = outcome.roll.values();
    let moves: Vec<String> = outcome
        .applied
        .iter()
        .map(|m| format!("{:?}(c{},d{})", m.kind, m.checker, m.die))
        .collect();
    leptos::logging::log!("[GAMELOG] {:?} {a}-{b} | {}", outcome.side, moves.join(" "));
}

/// Пауза-заставка перед первым ходом партии (мс) — чтобы старт не мелькал.
pub(crate) const INTRO_MS: u32 = 1800;
/// Пауза кадра броска: 3D-кувырок длится до ~2.2с (см. `die3d`) + запас, мс.
pub(crate) const ROLL_ANIM_MS: u32 = 2400;
/// Пауза на кадр «результат броска» — кости остановились (мс).
pub(crate) const HOLD_ROLL_MS: u32 = 1100;
/// Пауза, когда ходов нет: дольше показываем бросок, прежде чем отдать ход.
pub(crate) const HOLD_NOMOVE_MS: u32 = 1800;
/// Пауза на один шаг фишки по клетке, мс.
pub(crate) const HOLD_STEP_MS: u32 = 760;

/// Зерно ГПСЧ из времени браузера (на wasm `SystemTime` недоступен).
pub(crate) fn seed() -> u64 {
    (js_sys::Date::now() as u64) ^ 0x9E37_79B9_7F4A_7C15
}

/// Что выбрано как источник/цель: клетка доски, лоток резерва или плена.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sel {
    Cell(usize, usize),
    Reserve,
    Captured,
}

pub(crate) fn side_color(s: Side) -> &'static str {
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
pub(crate) struct Frame {
    pub(crate) state: GameState,
    pub(crate) roll: Option<DiceRoll>,
    pub(crate) hold: u32,
    pub(crate) rolling: bool,
    pub(crate) note: Option<String>,
    /// Переопределение точки отрисовки фишки `(индекс, x, y)` — для движения по
    /// параболе Луны (иначе keyed-кружок шёл бы по прямой между полями).
    pub(crate) pts: Vec<(usize, f64, f64)>,
}

/// Имя стороны для комментатора: «Игрок L» (человек) или «ИИ L» (компьютер),
/// окрашенное в цвет стороны. Статус рисуется как HTML (`inner_html`), поэтому
/// возвращаем `<b>`-спан с цветом; текст наш — инъекций не бывает.
pub(crate) fn side_name(side: Side, humans: &[Side]) -> String {
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
pub(crate) fn move_note(
    before: &GameState,
    after: &GameState,
    mv: Move,
    humans: &[Side],
) -> Option<String> {
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
        // Проход сквозь Тюрьму, обычный шаг и выкуп репликой не сопровождаем (шум).
        MoveKind::PrisonPass | MoveKind::Step | MoveKind::Ransom => return None,
    };
    Some(format!("{name}: {event}"))
}

/// Реплика о броске: что выпало, дубль и отсутствие ходов.
pub(crate) fn roll_note(side: Side, humans: &[Side], roll: DiceRoll, no_move: bool) -> String {
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
pub(crate) fn team_of(s: Side) -> usize {
    s.index() % 2
}

/// Окончена ли партия с учётом режима (считается напрямую из состояния): 2×2 — когда
/// обе стороны команды финишировали; каждый-сам-за-себя — когда остался один не
/// финишировавший (FFA до `n-1` финишей).
pub(crate) fn game_over(state: &GameState, teams: bool) -> bool {
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
pub(crate) fn result_msg(
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
pub(crate) fn apply_with_frames(
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
            pts: Vec::new(),
        });
    }
    let after = apply(&state, mv);
    // Движение по параболе Луны: вход (→поле 1), переход между полями и сход —
    // фишку ведём по дуге через кадры с переопределённой точкой `pts`.
    let moon_seg = match (
        mv.kind,
        after.checkers[mv.checker].pos,
        state.checkers[mv.checker].pos,
    ) {
        (MoveKind::EnterMoon, Position::Moon { side, field }, _) => {
            Some((side, 0.0, moon_field_t(field)))
        }
        (
            MoveKind::MoonAdvance,
            Position::Moon { side, field },
            Position::Moon { field: from, .. },
        ) => Some((side, moon_field_t(from), moon_field_t(field))),
        (MoveKind::MoonExit, _, Position::Moon { side, field }) => {
            Some((side, moon_field_t(field), 1.0))
        }
        _ => None,
    };
    if let Some((side, t0, t1)) = moon_seg {
        let steps = 8;
        for k in 1..=steps {
            let t = t0 + (t1 - t0) * (f64::from(k) / f64::from(steps));
            let (x, y) = moon_arc_point(side, t);
            frames.push(Frame {
                state: after.clone(),
                roll: Some(roll),
                hold: HOLD_STEP_MS / 7,
                rolling: false,
                note: None,
                pts: vec![(mv.checker, x, y)],
            });
        }
    }
    let note = move_note(&state, &after, mv, humans);
    frames.push(Frame {
        state: after.clone(),
        roll: Some(roll),
        hold: HOLD_STEP_MS,
        rolling: false,
        note,
        pts: Vec::new(),
    });
    after
}

/// Разворачивает один ход (бросок + выбранная последовательность) в кадры: сперва
/// «кости брошены», затем по фишке за раз (с пошаговым проходом по клеткам), включая
/// вынужденные ответные ходы захватчиков при выкупе. Продвигает `game`.
pub(crate) fn commit_frames<F>(
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
            pts: Vec::new(),
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
            pts: Vec::new(),
        });
    }
    let pre = game.state.clone();
    let outcome = game.commit_turn(roll, played, forced);
    if cfg!(debug_assertions) {
        dbg_log_turn(&outcome);
    }
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
pub(crate) fn step_through_prison(
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
                pts: Vec::new(),
            });
        }
    }
    apply(&state, mv)
}

pub(crate) fn fresh(dice: StoredValue<RandomDice>, active: Vec<Side>, teams: bool) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(active.clone(), d)));
    let mut game = game.expect("game started");
    game.state.teams = teams && game.state.active.len() == 4;
    game
}

/// Активные стороны для `n` игроков: 2 — противоположные, 3 — A/B/C, 4 — все.
pub(crate) fn active_for(n: usize) -> Vec<Side> {
    match n {
        2 => vec![Side::A, Side::A.opposite()],
        3 => vec![Side::A, Side::B, Side::C],
        _ => Side::ALL.to_vec(),
    }
}
