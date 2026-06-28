use crate::geom::{moon_arc_point, moon_field_t};
use crate::i18n::*;
use crate::settings::SoundKind;
use leptos::prelude::*;
use leptos_i18n::I18nContext;
use sheshbesh::board::PERIMETER;
use sheshbesh::{
    DiceRoll, Game, GameState, Move, MoveKind, Position, RandomDice, Side, TurnOutcome, apply,
};

#[cfg(debug_assertions)]
thread_local! {
    static GAME_LOG: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

#[cfg(debug_assertions)]
pub(crate) fn dbg_log(line: &str) {
    leptos::logging::log!("{line}");
    GAME_LOG.with(|g| {
        let mut s = g.borrow_mut();
        s.push_str(line);
        s.push('\n');
    });
}
#[cfg(not(debug_assertions))]
pub(crate) fn dbg_log(_line: &str) {}

#[cfg(debug_assertions)]
pub(crate) fn dbg_log_reset() {
    GAME_LOG.with(|g| g.borrow_mut().clear());
}
#[cfg(not(debug_assertions))]
pub(crate) fn dbg_log_reset() {}

#[cfg(debug_assertions)]
pub(crate) fn dbg_log_dump() -> String {
    GAME_LOG.with(|g| g.borrow().clone())
}
#[cfg(not(debug_assertions))]
pub(crate) fn dbg_log_dump() -> String {
    String::new()
}

pub(crate) fn dbg_log_turn(outcome: &TurnOutcome) {
    dbg_log_moves(outcome.side, outcome.roll, &outcome.applied);
}

pub(crate) fn dbg_log_moves(side: Side, roll: DiceRoll, applied: &[Move]) {
    let [a, b] = roll.values();
    let moves: Vec<String> = applied
        .iter()
        .map(|m| format!("{:?}(c{},p{})", m.kind, m.checker, m.pips))
        .collect();
    dbg_log(&format!("[GAMELOG] {side:?} {a}-{b} | {}", moves.join(" ")));
}

pub(crate) const INTRO_MS: u32 = 1800;
pub(crate) const ROLL_ANIM_MS: u32 = 2400;
pub(crate) const HOLD_ROLL_MS: u32 = 1100;
pub(crate) const HOLD_NOMOVE_MS: u32 = 1800;
pub(crate) const HOLD_STEP_MS: u32 = 570;
pub(crate) const FADE_MS: u32 = 450;

pub(crate) fn seed() -> u64 {
    (js_sys::Date::now() as u64) ^ 0x9E37_79B9_7F4A_7C15
}

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

// --- Frame / herald helpers ---

#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) state: GameState,
    pub(crate) roll: Option<DiceRoll>,
    pub(crate) hold: u32,
    pub(crate) rolling: bool,
    pub(crate) note: Option<String>,
    pub(crate) sound: Option<SoundKind>,
    pub(crate) pts: Vec<(usize, f64, f64)>,
    pub(crate) fade: bool,
}

pub(crate) fn side_name(side: Side, humans: &[Side], i18n: I18nContext<Locale>) -> String {
    let who = if humans.contains(&side) {
        tu_string!(i18n, side_human)
    } else {
        tu_string!(i18n, side_ai)
    };
    format!(
        "<b style=\"color:{}\">{who} {}</b>",
        side_color(side),
        side.letter()
    )
}

pub(crate) fn move_note(
    before: &GameState,
    after: &GameState,
    mv: Move,
    humans: &[Side],
    i18n: I18nContext<Locale>,
) -> Option<String> {
    let owner = before.checkers[mv.checker].owner;
    let name = side_name(owner, humans, i18n);
    if !before.has_won(owner) && after.has_won(owner) {
        return Some(tu_string!(i18n, note_finish, who = name));
    }
    if mv.kind == MoveKind::Ransom {
        return Some(tu_string!(i18n, note_ransom, who = name));
    }
    let victim = after.checkers.iter().enumerate().find(|(j, c)| {
        matches!(c.pos, Position::Captured { .. })
            && !matches!(before.checkers[*j].pos, Position::Captured { .. })
    });
    if let Some((j, _)) = victim {
        let v = side_name(before.checkers[j].owner, humans, i18n);
        return Some(tu_string!(i18n, note_capture, who = name, victim = v));
    }
    let event = match mv.kind {
        MoveKind::Enter => Some(tu_string!(i18n, note_enter)),
        MoveKind::EnterMoon => Some(tu_string!(i18n, note_moon_enter)),
        MoveKind::MoonAdvance => Some(tu_string!(i18n, note_moon_advance)),
        MoveKind::MoonExit => Some(tu_string!(i18n, note_moon_exit)),
        MoveKind::EnterPrison => Some(tu_string!(i18n, note_prison_enter)),
        MoveKind::PrisonRelease => Some(tu_string!(i18n, note_prison_release)),
        MoveKind::EnterHome => Some(tu_string!(i18n, note_home_enter)),
        MoveKind::HomeAdvance => Some(tu_string!(i18n, note_home_advance)),
        MoveKind::Step | MoveKind::Ransom => None,
    };
    event.map(|e| format!("{name}: {e}"))
}

pub(crate) fn roll_note(side: Side, humans: &[Side], roll: DiceRoll, no_move: bool, i18n: I18nContext<Locale>) -> String {
    let name = side_name(side, humans, i18n);
    let [a, b] = roll.values();
    if no_move {
        return tu_string!(i18n, herald_no_move, who = name, a = a.to_string(), b = b.to_string());
    }
    if roll.is_double() {
        tu_string!(i18n, herald_double, who = name, a = a.to_string(), b = b.to_string())
    } else {
        tu_string!(i18n, herald_wait_move, who = name, a = a.to_string(), b = b.to_string())
    }
}

/// Определяет звук по виду хода (без проверки захвата — может быть перекрыт).  
/// Если произошло съедание, колл-центр выставляет `SoundKind::Capture` отдельно.
pub(crate) fn kind_sound(kind: MoveKind) -> Option<SoundKind> {
    match kind {
        MoveKind::Step => None,
        MoveKind::Enter => Some(SoundKind::Enter),
        MoveKind::EnterMoon | MoveKind::MoonAdvance | MoveKind::MoonExit => Some(SoundKind::Moon),
        MoveKind::EnterPrison => Some(SoundKind::PrisonIn),
        MoveKind::PrisonRelease => Some(SoundKind::PrisonOut),
        MoveKind::EnterHome => Some(SoundKind::Home),
        MoveKind::HomeAdvance => None,
        MoveKind::Ransom => Some(SoundKind::Ransom),
    }
}

pub(crate) fn team_of(s: Side) -> usize {
    s.index() % 2
}

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

pub(crate) fn result_msg(
    state: &GameState,
    finished: &[Side],
    teams: bool,
    humans: &[Side],
    i18n: I18nContext<Locale>,
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
        Some(tu_string!(i18n, result_team_wins, team = ms.join("+")))
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
            .map(|(i, s)| format!("{}) {}", i + 1, side_name(*s, humans, i18n)))
            .collect();
        Some(tu_string!(i18n, result_full,
            who = side_name(order[0], humans, i18n),
            places = places.join(", ")
        ))
    }
}

pub(crate) fn apply_with_frames(
    frames: &mut Vec<Frame>,
    state: GameState,
    mv: Move,
    roll: DiceRoll,
    humans: &[Side],
    i18n: I18nContext<Locale>,
) -> GameState {
    let perim = PERIMETER as u16;
    let mids: Vec<Position> = match state.checkers[mv.checker].pos {
        Position::OnTrack { progress } => {
            let target = progress + u16::from(mv.pips);
            if target <= perim {
                let mut v: Vec<Position> = (progress + 1..target)
                    .map(|i| Position::OnTrack { progress: i })
                    .collect();
                if matches!(mv.kind, MoveKind::EnterMoon | MoveKind::EnterPrison) {
                    v.push(Position::OnTrack { progress: target });
                }
                v
            } else {
                let depth = (target - perim - 1) as u8;
                let mut v: Vec<Position> = (progress + 1..perim)
                    .map(|i| Position::OnTrack { progress: i })
                    .collect();
                if progress < perim {
                    v.push(Position::OnTrack { progress: perim });
                }
                v.extend((0..depth).map(|d| Position::Home { depth: d }));
                v
            }
        }
        Position::Home { depth } if mv.kind == MoveKind::HomeAdvance => {
            (depth + 1..depth + mv.pips)
                .map(|d| Position::Home { depth: d })
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
            sound: None,
            pts: Vec::new(),
            fade: false,
        });
    }
    let after = apply(&state, mv);
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
                sound: None,
                pts: vec![(mv.checker, x, y)],
                fade: false,
            });
        }
    }
    let note = move_note(&state, &after, mv, humans, i18n);
    let capture_sound = state.checkers.iter().enumerate().any(|(j, _c)| {
        matches!(after.checkers[j].pos, Position::Captured { .. })
            && !matches!(state.checkers[j].pos, Position::Captured { .. })
    });
    let sound = if capture_sound {
        Some(SoundKind::Capture)
    } else {
        kind_sound(mv.kind)
    };
    frames.push(Frame {
        state: after.clone(),
        roll: Some(roll),
        hold: HOLD_STEP_MS,
        rolling: false,
        note,
        sound,
        pts: Vec::new(),
        fade: false,
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
    i18n: I18nContext<Locale>,
) where
    F: FnMut(&GameState, Side, &[Move]) -> usize,
{
    let side = game.state.to_move;
    let no_move = played.is_empty();
    if roll_anim {
        frames.push(Frame {
            state: game.state.clone(),
            roll: Some(roll),
            hold: ROLL_ANIM_MS,
            rolling: true,
            note: Some(tu_string!(i18n, herald_roll, who = side_name(side, humans, i18n))),
            sound: Some(SoundKind::Dice),
            pts: Vec::new(),
            fade: false,
        });
        frames.push(Frame {
            state: game.state.clone(),
            roll: Some(roll),
            hold: if no_move {
                HOLD_NOMOVE_MS
            } else {
                HOLD_ROLL_MS
            },
            rolling: false,
            note: Some(roll_note(side, humans, roll, no_move, i18n)),
            sound: None,
            pts: Vec::new(),
            fade: false,
        });
    }
    let pre = game.state.clone();
    let outcome = game.commit_turn(roll, played, forced);
    if cfg!(debug_assertions) {
        dbg_log_turn(&outcome);
    }
    let applied = &outcome.applied;
    let mut scratch = pre;
    for &mv in applied {
        let forced_resp = scratch.checkers[mv.checker].owner != side;
        scratch = apply_with_frames(frames, scratch, mv, roll, humans, i18n);
        if forced_resp && let Some(last) = frames.last_mut() {
            last.note = Some(tu_string!(i18n, forced_reply,
                who = side_name(scratch.checkers[mv.checker].owner, humans, i18n)
            ));
        }
    }
}

pub(crate) fn fresh(dice: StoredValue<RandomDice>, active: Vec<Side>, teams: bool) -> Game {
    let mut game = None;
    dice.update_value(|d| game = Some(Game::start(active.clone(), d)));
    let mut game = game.expect("game started");
    game.state.teams = teams && game.state.active.len() == 4;
    game
}

pub(crate) fn active_for(n: usize) -> Vec<Side> {
    match n {
        2 => vec![Side::A, Side::A.opposite()],
        3 => vec![Side::A, Side::B, Side::C],
        _ => Side::ALL.to_vec(),
    }
}
