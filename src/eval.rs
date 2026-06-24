//! Сравнение силы агентов: матчи на 2 игрока с чередованием мест и подсчётом
//! побед. Это мерило прогресса при обучении — winrate против эвристики или прошлых
//! версий обучаемого ИИ. Чистый движок, без внешних зависимостей.

use crate::ai::Heuristic;
use crate::board::Side;
use crate::moves::{Move, legal_turns};
use crate::state::GameState;
use crate::turn::{Agent, DiceSource, Game, RandomDice, team_of};
use crate::value::{Value, best_forced, best_turn};

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// Партия двух агентов (2 игрока): сторона `side_a` ходит агентом `a`,
/// противоположная — `b`. Вынужденный ответ при выкупе делает агент **захватчика**.
/// Возвращает победителя (или `None`, если не уложились в `max_turns`).
pub fn play_game_2p<A, B, D>(
    side_a: Side,
    a: &mut A,
    b: &mut B,
    dice: &mut D,
    max_turns: usize,
) -> Option<Side>
where
    A: Agent,
    B: Agent,
    D: DiceSource,
{
    let mut game = Game::new(GameState::new(vec![side_a, side_a.opposite()], side_a));
    for _ in 0..max_turns {
        if let Some(w) = game.winner() {
            return Some(w);
        }
        let mover = game.state.to_move;
        let roll = dice.roll();
        let turns = legal_turns(&game.state, roll);
        let raw = if mover == side_a {
            a.choose_turn(&game.state, &turns)
        } else {
            b.choose_turn(&game.state, &turns)
        };
        let played = turns[raw.min(turns.len() - 1)].clone();
        game.commit_turn(roll, played, |s, captor, opts| {
            if captor == side_a {
                a.choose_forced(s, captor, opts)
            } else {
                b.choose_forced(s, captor, opts)
            }
        });
    }
    game.winner()
}

/// Матч из `games` партий между `a` и `b`: места чередуются (чтобы преимущество
/// первого хода усреднилось), у каждой партии своё зерно костей. Возвращает
/// `(победы a, победы b)`; незавершённые в пределах `max_turns` в счёт не идут.
pub fn match_winrate<A, B>(
    a: &mut A,
    b: &mut B,
    games: usize,
    seed: u64,
    max_turns: usize,
) -> (usize, usize)
where
    A: Agent,
    B: Agent,
{
    let (mut aw, mut bw) = (0, 0);
    for g in 0..games {
        let mut dice = RandomDice::from_seed(
            seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(g as u64),
        );
        // Чётные партии — `a` за сторону A, нечётные — за C (другой первый ход).
        let side_a = if g % 2 == 0 { Side::A } else { Side::C };
        match play_game_2p(side_a, a, b, &mut dice, max_turns) {
            Some(w) if w == side_a => aw += 1,
            Some(_) => bw += 1,
            None => {}
        }
    }
    (aw, bw)
}

/// Партия произвольного числа игроков (с режимом команд): ход стороны `mover`
/// выбирает `choose`, вынужденный ответ-выкуп захватчика — `forced`. Возвращает
/// победителей (`z = 1`; см. `winners_of`).
pub fn play_game<C, F>(
    active: &[Side],
    teams: bool,
    dice: &mut impl DiceSource,
    max_turns: usize,
    mut choose: C,
    mut forced: F,
) -> Vec<Side>
where
    C: FnMut(Side, &GameState, &[Vec<Move>]) -> usize,
    F: FnMut(Side, &GameState, &[Move]) -> usize,
{
    let mut state = GameState::new(active.to_vec(), active[0]);
    state.teams = teams && active.len() == 4;
    let mut game = Game::new(state);
    for _ in 0..max_turns {
        if let Some(w) = game.winners() {
            return w;
        }
        let mover = game.state.to_move;
        let roll = dice.roll();
        let turns = legal_turns(&game.state, roll);
        let idx = choose(mover, &game.state, &turns).min(turns.len() - 1);
        let played = turns[idx].clone();
        game.commit_turn(roll, played, |s, captor, opts| forced(captor, s, opts));
    }
    game.winners().unwrap_or_default()
}

/// Доля побед обученной модели в режиме «каждый сам за себя»: модель играет ОДНОЙ
/// стороной (место чередуется по партиям ради честности), остальные — `Heuristic`.
/// Случайная игра дала бы `1/n`; превышение означает, что модель сильнее эвристики.
pub fn winrate_ffa<V: Value>(
    model: &V,
    active: &[Side],
    games: usize,
    seed: u64,
    max_turns: usize,
) -> (usize, usize) {
    winrate_ffa_vs(model, &Heuristic, active, games, seed, max_turns)
}

/// Как `winrate_ffa`, но противники на остальных местах — произвольная ценность `opp`
/// (а не обязательно эвристика). Нужно, чтобы стравливать две обучаемые модели.
pub fn winrate_ffa_vs<V: Value, O: Value>(
    model: &V,
    opp: &O,
    active: &[Side],
    games: usize,
    seed: u64,
    max_turns: usize,
) -> (usize, usize) {
    let mut wins = 0;
    for g in 0..games {
        let seat = active[g % active.len()];
        let mut dice = RandomDice::from_seed(seed.wrapping_mul(GOLDEN).wrapping_add(g as u64));
        let choose = |side: Side, st: &GameState, turns: &[Vec<Move>]| {
            if side == seat {
                best_turn(model, st, turns)
            } else {
                best_turn(opp, st, turns)
            }
        };
        let forced = |side: Side, st: &GameState, moves: &[Move]| {
            if side == seat {
                best_forced(model, st, side, moves)
            } else {
                best_forced(opp, st, side, moves)
            }
        };
        if play_game(active, false, &mut dice, max_turns, choose, forced).contains(&seat) {
            wins += 1;
        }
    }
    (wins, games)
}

/// Доля побед модели в командах 2×2: модель играет одну команду (чередуется),
/// `Heuristic` — другую. Случайная игра — 50%.
pub fn winrate_teams<V: Value>(
    model: &V,
    games: usize,
    seed: u64,
    max_turns: usize,
) -> (usize, usize) {
    winrate_teams_vs(model, &Heuristic, games, seed, max_turns)
}

/// Как `winrate_teams`, но вражеская команда играет произвольной ценностью `opp`
/// (а не эвристикой). Нужно, чтобы стравливать две обучаемые модели.
pub fn winrate_teams_vs<V: Value, O: Value>(
    model: &V,
    opp: &O,
    games: usize,
    seed: u64,
    max_turns: usize,
) -> (usize, usize) {
    let active = Side::ALL.to_vec();
    let mut wins = 0;
    for g in 0..games {
        let model_team = g % 2; // 0 = {A,C}, 1 = {B,D}
        let is_model = move |s: Side| team_of(s) == model_team;
        let mut dice = RandomDice::from_seed(seed.wrapping_mul(GOLDEN).wrapping_add(g as u64));
        let choose = |side: Side, st: &GameState, turns: &[Vec<Move>]| {
            if is_model(side) {
                best_turn(model, st, turns)
            } else {
                best_turn(opp, st, turns)
            }
        };
        let forced = |side: Side, st: &GameState, moves: &[Move]| {
            if is_model(side) {
                best_forced(model, st, side, moves)
            } else {
                best_forced(opp, st, side, moves)
            }
        };
        if play_game(&active, true, &mut dice, max_turns, choose, forced)
            .iter()
            .any(|&s| is_model(s))
        {
            wins += 1;
        }
    }
    (wins, games)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Heuristic;
    use crate::turn::FirstChoice;
    use crate::value::ValueAgent;

    // Нарды — игра с высокой дисперсией (исход во многом решают кости), поэтому для
    // надёжного сигнала о силе нужно МНОГО партий: на ~40 играх перевес тонет в шуме
    // (бывает и 50/50), а на 200 эвристика стабильно берёт ~65–70%.
    #[test]
    fn heuristic_beats_first_choice() {
        let (h, f) = match_winrate(&mut Heuristic, &mut FirstChoice, 200, 12345, 40_000);
        assert_eq!(h + f, 200, "все партии должны завершиться");
        assert!(h > f, "эвристика должна обыгрывать FirstChoice: {h} к {f}");
    }

    #[test]
    fn value_agent_wraps_heuristic() {
        // ValueAgent(Heuristic) играет той же политикой (та же оценка афтерстейтов),
        // поэтому так же обыгрывает тривиального агента.
        let (v, f) = match_winrate(
            &mut ValueAgent(Heuristic),
            &mut FirstChoice,
            200,
            777,
            40_000,
        );
        assert!(
            v > f,
            "ValueAgent(Heuristic) должен обыгрывать FirstChoice: {v} к {f}"
        );
    }
}
