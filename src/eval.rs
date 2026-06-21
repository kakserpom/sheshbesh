//! Сравнение силы агентов: матчи на 2 игрока с чередованием мест и подсчётом
//! побед. Это мерило прогресса при обучении — winrate против эвристики или прошлых
//! версий обучаемого ИИ. Чистый движок, без внешних зависимостей.

use crate::board::Side;
use crate::moves::legal_turns;
use crate::state::GameState;
use crate::turn::{Agent, DiceSource, Game, RandomDice};

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
