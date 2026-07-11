//! Кодирование позиции в вектор признаков для обучаемой функции ценности.
//!
//! **Канонизация под ходящего.** Доска 4-кратно симметрична (у каждой стороны одна
//! и та же геометрия: Луна на локальном 2, Тюрьмы на 4/14, Дом на 9), поэтому
//! позицию кодируем *относительно перспективы* `p`:
//! - стороны нумеруются относительно `p`: `rel_owner = (owner − p) mod 4` (0 — сам);
//! - клетки периметра — относительно входа `p`: `rel_cell = (abs − entry(p)) mod 72`.
//!
//! Тогда поворот всей партии на `r` мест (см. `rotate_state`) не меняет вектор,
//! если сменить перспективу на `p+r` — одна и та же сеть обслуживает все четыре
//! места, а повороты дают бесплатную аугментацию данных (×4).
//!
//! Признаки на каждую относительную сторону (`PER_OWNER`):
//! - `pip` — нормированная сумма «пройденного пути к Дому» (гонка), линейный сигнал;
//! - `reserve`/`captured`/`prison` — доли фишек в резерве/плену/Тюрьме;
//! - `home[4]` — занятость клеток Дома по глубине;
//! - `moon[3]` — фишки на полях Луны 1/3/6;
//! - `track[72]` — занятость клеток периметра в системе отсчёта `p` (контакт/блок):
//!   сюда попадают фишки на дорожке и в Тюрьме (всё, что физически на периметре);
//! - `mobile[6]` — можно ли сыграть костью 1..6 (гибкость: «эффективно использовать
//!   ЛЮБОЙ бросок»; для соперников этот же признак — сигнал «снижать их подвижность»);
//! - `corner` — доля фишек на углах (безопасный анкер: на углу не едят, и им удобно
//!   поджидать обошедшие фишки соперника);
//! - `blots` — доля своих **блотов**: одиночных фишек на бьющихся клетках (обычная /
//!   вход в Дом — там едят; угол/Луна/Тюрьма безопасны). Прямой сигнал уязвимости;
//! - `contact` — доля блотов **под боем**: позади которых (в 1..6 по ходу) стоит
//!   вражеская (не союзная) фишка, способная съесть их прямым броском. Дистиллированный
//!   сигнал контакта/непосредственной угрозы (точную «стрельбу» сеть добирает из `track`).

use crate::board::{
    CellKind, HOME_DEPTH, LOCAL_MOON_EXIT, PERIMETER, PerimeterIdx, Side, cell_kind,
};
use crate::moves::can_play;
use crate::state::{Checker, GameState, MoonField, Position};

// Смещения внутри блока одной относительной стороны.
const OFF_PIP: usize = 0;
const OFF_RESERVE: usize = 1;
const OFF_CAPTURED: usize = 2;
const OFF_PRISON: usize = 3;
const OFF_HOME: usize = 4; // 4 клетки Дома
const OFF_MOON: usize = OFF_HOME + HOME_DEPTH; // 3 поля Луны
const OFF_TRACK: usize = OFF_MOON + 3; // 72 клетки периметра
const OFF_MOBILE: usize = OFF_TRACK + PERIMETER; // 6 — играбельность костей 1..6
const OFF_CORNER: usize = OFF_MOBILE + 6; // 1 — доля фишек на углах
const OFF_BLOTS: usize = OFF_CORNER + 1; // 1 — доля своих блотов (уязвимых одиночек)
const OFF_CONTACT: usize = OFF_BLOTS + 1; // 1 — доля блотов под прямым боем врага
/// Длина блока признаков одной относительной стороны.
const PER_OWNER: usize = OFF_CONTACT + 1;
/// Полная длина вектора признаков (4 стороны, включая отсутствующие — нулями).
pub const FEATURES: usize = PER_OWNER * 4;

/// Нормировка `pip`: максимум суммы пути по 4 фишкам (Дом самой глубокой клетки).
const PIP_MAX: f32 = (4 * (PERIMETER + HOME_DEPTH)) as f32;

/// Относительный индекс стороны: 0 — сам ходящий, далее против часовой стрелки.
fn rel_owner(p: Side, owner: Side) -> usize {
    (owner.index() + 4 - p.index()) % 4
}

/// Клетка периметра в системе отсчёта перспективы `p` (0 — у входа `p`).
fn rel_cell(p: Side, abs: PerimeterIdx) -> usize {
    (abs.get() + PERIMETER - p.entry().get()) % PERIMETER
}

/// «Путь к Дому» фишки с точки зрения её владельца (для линейного признака гонки).
fn progress_to_home(owner: Side, pos: Position) -> usize {
    match pos {
        Position::Reserve | Position::Captured { .. } => 0,
        Position::OnTrack { progress } => progress as usize,
        Position::Moon { side, .. } => {
            owner.progress_of(side.local_to_perimeter(LOCAL_MOON_EXIT)) as usize
        }
        Position::Prison { cell } => owner.progress_of(cell) as usize,
        Position::Home { depth } => PERIMETER + depth as usize,
    }
}

fn moon_field_index(field: MoonField) -> usize {
    match field {
        MoonField::One => 0,
        MoonField::Three => 1,
        MoonField::Six => 2,
    }
}

/// Кодирует позицию с точки зрения стороны `p` в `out` (длиной `FEATURES`).
///
/// # Panics
/// Если `out.len() != FEATURES`.
pub fn encode_into(state: &GameState, p: Side, out: &mut [f32]) {
    assert_eq!(out.len(), FEATURES, "буфер признаков неверной длины");
    out.fill(0.0);
    for ch in state.checkers() {
        let owner = ch.owner;
        let base = rel_owner(p, owner) * PER_OWNER;
        out[base + OFF_PIP] += progress_to_home(owner, ch.pos) as f32 / PIP_MAX;
        match ch.pos {
            Position::Reserve => out[base + OFF_RESERVE] += 0.25,
            Position::Captured { .. } => out[base + OFF_CAPTURED] += 0.25,
            Position::Prison { .. } => out[base + OFF_PRISON] += 0.25,
            Position::Home { depth } => out[base + OFF_HOME + depth as usize] += 1.0,
            Position::Moon { field, .. } => out[base + OFF_MOON + moon_field_index(field)] += 0.25,
            Position::OnTrack { .. } => {}
        }
        // Контакт/блок: всё, что физически на периметре (дорожка и Тюрьма).
        if let Some(abs) = ch.pos.perimeter_cell(owner) {
            out[base + OFF_TRACK + rel_cell(p, abs)] += 1.0;
            if cell_kind(abs) == CellKind::Corner {
                out[base + OFF_CORNER] += 0.25; // безопасный анкер на углу
            }
        }
    }
    // Подвижность: можно ли сыграть каждой костью 1..6 (для каждой присутствующей
    // стороны — своя; модель учится максимизировать свою и минимизировать чужую).
    for &side in &state.active {
        let base = rel_owner(p, side) * PER_OWNER;
        for d in 1..=6u8 {
            if can_play(state, side, d) {
                out[base + OFF_MOBILE + (d - 1) as usize] = 1.0;
            }
        }
    }
    // Контакт и блоты. «Блот» — фишка на бьющейся клетке (обычная / вход в Дом; на углу,
    // Луне и в Тюрьме не едят). На таких клетках двух своих не бывает, так что каждая —
    // одиночка-блот. «Под боем» — позади блота (в 1..6 клеток по ходу) стоит фишка не-
    // союзного соперника: прямой бросок может её съесть. Путь/точную кость не проверяем
    // (это `Heuristic::shots_to`) — дешёвый дистиллированный сигнал, остальное сеть
    // добирает из `track`. Сначала соберём абсолютные клетки всех фишек на периметре.
    let perim: Vec<(Side, usize)> = state
        .checkers()
        .iter()
        .filter_map(|c| {
            c.pos
                .perimeter_cell(c.owner)
                .map(|abs| (c.owner, abs.get()))
        })
        .collect();
    for c in state.checkers() {
        let owner = c.owner;
        let Some(abs) = c.pos.perimeter_cell(owner) else {
            continue;
        };
        if !matches!(cell_kind(abs), CellKind::Plain | CellKind::HomeEntrance) {
            continue; // угол/Луна/Тюрьма — не блот (там не едят)
        }
        let base = rel_owner(p, owner) * PER_OWNER;
        out[base + OFF_BLOTS] += 0.25;
        let t = abs.get();
        let under_fire = perim.iter().any(|&(eo, e)| {
            eo != owner && !state.are_allied(owner, eo) && {
                let d = (t + PERIMETER - e) % PERIMETER; // вперёд от врага к блоту
                (1..=6).contains(&d)
            }
        });
        if under_fire {
            out[base + OFF_CONTACT] += 0.25;
        }
    }
}

/// Кодирует позицию с точки зрения стороны `p` в новый вектор.
pub fn encode(state: &GameState, p: Side) -> Vec<f32> {
    let mut out = vec![0.0; FEATURES];
    encode_into(state, p, &mut out);
    out
}

/// Поворот всей партии на `r` мест против часовой стрелки: каждая сторона `s`
/// становится `s+r`, абсолютные поля (клетка Тюрьмы, сторона Луны, захватчик)
/// сдвигаются соответственно; владелец-относительные поля (`progress`/`depth`)
/// не меняются. Используется для аугментации данных и проверки инвариантности.
pub fn rotate_state(state: &GameState, r: usize) -> GameState {
    let rot = |s: Side| Side::from_index(s.index() + r);
    let mut out = state.clone();
    out.active = state.active.iter().map(|&s| rot(s)).collect();
    out.to_move = rot(state.to_move);
    out.clear_checkers();
    for ch in state.checkers() {
        let owner = rot(ch.owner);
        let pos = match ch.pos {
            Position::Moon { side, field } => Position::Moon {
                side: rot(side),
                field,
            },
            Position::Prison { cell } => Position::Prison {
                cell: cell.advance(18 * r),
            },
            Position::Captured { captor } => Position::Captured {
                captor: rot(captor),
            },
            other => other,
        };
        out.push_checker(Checker { owner, pos });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Checker;

    /// Состояние со всеми четырьмя сторонами и разнообразными положениями фишек.
    fn varied_state() -> GameState {
        let mut s = GameState::new(Side::ALL.to_vec(), Side::A);
        s.clear_checkers();
        let mut push = |owner: Side, pos: Position| s.push_checker(Checker { owner, pos });
        // A: дорожка, Дом, Луна, плен.
        push(Side::A, Position::OnTrack { progress: 5 });
        push(Side::A, Position::Home { depth: 2 });
        push(
            Side::A,
            Position::Moon {
                side: Side::C,
                field: MoonField::Three,
            },
        );
        push(Side::A, Position::Captured { captor: Side::B });
        // B: резерв, дорожка, Тюрьма (на клетке своей дальней Тюрьмы).
        push(Side::B, Position::Reserve);
        push(Side::B, Position::OnTrack { progress: 30 });
        push(
            Side::B,
            Position::Prison {
                cell: Side::B.local_to_perimeter(crate::board::LOCAL_PRISON_FAR),
            },
        );
        push(Side::B, Position::OnTrack { progress: 71 });
        // C и D: по паре фишек на дорожке и в Доме.
        push(Side::C, Position::OnTrack { progress: 12 });
        push(Side::C, Position::Home { depth: 0 });
        push(Side::D, Position::OnTrack { progress: 40 });
        push(Side::D, Position::Reserve);
        s
    }

    #[test]
    fn length_matches() {
        assert_eq!(
            encode(&GameState::new(vec![Side::A, Side::C], Side::A), Side::A).len(),
            FEATURES
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // значения точны: 0.25×4 = 1.0, отсутствие = 0.0
    fn initial_state_puts_reserve_for_self_at_slot_zero() {
        // Старт: у каждого 4 фишки в резерве. С перспективы A её резерв — слот 0.
        let s = GameState::new(vec![Side::A, Side::C], Side::A);
        let v = encode(&s, Side::A);
        assert_eq!(v[OFF_RESERVE], 1.0); // 4 × 0.25
        // Соперник C — относительная сторона 2.
        assert_eq!(v[2 * PER_OWNER + OFF_RESERVE], 1.0);
        // Сторон 1 и 3 нет — нули.
        assert_eq!(v[PER_OWNER + OFF_RESERVE], 0.0);
    }

    #[test]
    fn encoding_is_rotation_invariant() {
        // Кодирование позиции с перспективы A совпадает с кодированием повёрнутой
        // на r позиции с перспективы A+r — это и есть симметрия, на которой стоит
        // обучение одной сетью для всех мест.
        let s = varied_state();
        let base = encode(&s, Side::A);
        for r in 1..4 {
            let sr = rotate_state(&s, r);
            let rot = encode(&sr, Side::from_index(Side::A.index() + r));
            assert_eq!(base, rot, "вектор должен быть инвариантен к повороту r={r}");
        }
    }

    #[test]
    #[allow(clippy::float_cmp)] // значения кратны 0.25 — точны
    fn blots_and_contact() {
        // A на обычной клетке (progress 10 = abs 19, обычная) — это блот.
        let mut s = GameState::new(vec![Side::A, Side::C], Side::A);
        s.clear_checkers();
        s.push_checker(Checker {
            owner: Side::A,
            pos: Position::OnTrack { progress: 10 },
        });
        let v = encode(&s, Side::A);
        assert_eq!(
            v[OFF_BLOTS], 0.25,
            "одна своя фишка на обычной клетке — блот"
        );
        assert_eq!(v[OFF_CONTACT], 0.0, "врагов рядом нет — не под боем");

        // Враг C в 3 клетках ПОЗАДИ блота A (по ходу) — прямой бой.
        let a_abs = Side::A.entry().advance(10); // клетка блота
        let behind = a_abs.advance(PERIMETER - 3); // на 3 позади (по ходу к блоту)
        s.push_checker(Checker {
            owner: Side::C,
            pos: Position::OnTrack {
                progress: Side::C.progress_of(behind),
            },
        });
        let v = encode(&s, Side::A);
        assert_eq!(
            v[OFF_CONTACT], 0.25,
            "блот A под прямым боем врага в 3 клетках"
        );
    }

    #[test]
    fn perspective_matters() {
        // С разных перспектив (без поворота) вектор отличается — иначе перспектива
        // была бы бесполезна.
        let s = varied_state();
        assert_ne!(encode(&s, Side::A), encode(&s, Side::B));
    }

    #[test]
    fn rotate_preserves_checker_count_and_completeness() {
        let s = varied_state();
        let sr = rotate_state(&s, 1);
        assert_eq!(s.checkers_len(), sr.checkers_len());
        // Сумма всех признаков не нулевая (позиция закодирована).
        assert!(encode(&s, Side::A).iter().sum::<f32>() > 0.0);
    }
}
