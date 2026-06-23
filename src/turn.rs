//! Туровый цикл: очередь ходов, переброс при дубле, выкуп пленных, победитель.
//!
//! Ход одного игрока за один бросок: кинуть две кости → выбрать легальный полный
//! ход (с учётом правила максимального хода) → применить. Дубль даёт тому же игроку
//! **ещё один полный ход** (переброс); иначе очередь переходит к следующей активной
//! стороне против часовой стрелки.
//!
//! Выкуп пленной фишки (`MoveKind::Ransom`) тратит «6» и возвращает её в резерв;
//! сразу после этого захватчик обязан сделать ответный ход на 6 клеток (если такой
//! ход есть). Решения принимает `Agent`: основной выбор хода и выбор вынужденного
//! ответного хода.

use crate::board::Side;
use crate::dice::{DiceRoll, Die};
use crate::moves::{Move, MoveKind, apply, forced_six_moves, legal_turns, move_legal};
use crate::state::{GameState, Position};

/// Источник бросков костей — абстракция ради тестируемости.
pub trait DiceSource {
    /// Кинуть две кости.
    fn roll(&mut self) -> DiceRoll;
}

/// Заранее заданная последовательность бросков (тесты, реплеи).
pub struct ScriptedDice {
    rolls: Vec<DiceRoll>,
    next: usize,
}

impl ScriptedDice {
    /// Создаёт источник из готового списка бросков.
    pub fn new(rolls: Vec<DiceRoll>) -> ScriptedDice {
        ScriptedDice { rolls, next: 0 }
    }
}

impl DiceSource for ScriptedDice {
    /// # Panics
    /// Если запрошено больше бросков, чем задано.
    fn roll(&mut self) -> DiceRoll {
        let r = self.rolls[self.next];
        self.next += 1;
        r
    }
}

/// Встроенный генератор бросков на основе `SplitMix64` — без внешних зависимостей.
/// Смещение от `% 6` пренебрежимо мало для игральной кости.
pub struct RandomDice {
    state: u64,
}

impl RandomDice {
    /// Детерминированный источник с заданным зерном (удобно для воспроизводимости).
    pub fn from_seed(seed: u64) -> RandomDice {
        RandomDice { state: seed }
    }

    /// Источник, засеянный системным временем.
    pub fn from_entropy() -> RandomDice {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        RandomDice::from_seed(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn die(&mut self) -> Die {
        Die::new((self.next_u64() % 6) as u8 + 1).expect("значение 1..=6")
    }
}

impl DiceSource for RandomDice {
    fn roll(&mut self) -> DiceRoll {
        DiceRoll::new(self.die(), self.die())
    }
}

/// Принимающий решения участник: выбирает ход из легальных вариантов.
pub trait Agent {
    /// Выбрать полный ход — индекс в списке легальных последовательностей
    /// (каждая уже удовлетворяет правилу максимального хода). Список непуст.
    fn choose_turn(&mut self, state: &GameState, turns: &[Vec<Move>]) -> usize;

    /// Выбрать вынужденный ответный ход на «6» при выкупе пленной фишки —
    /// индекс в списке `moves` ходов захватчика `captor`. По умолчанию — первый.
    fn choose_forced(&mut self, state: &GameState, captor: Side, moves: &[Move]) -> usize {
        let _ = (state, captor, moves);
        0
    }
}

/// Простейший агент: всегда берёт первый вариант. Удобен для демо и тестов.
pub struct FirstChoice;

impl Agent for FirstChoice {
    fn choose_turn(&mut self, _: &GameState, _: &[Vec<Move>]) -> usize {
        0
    }
}

/// Разыгрывает право первого хода: активные стороны по очереди (против часовой
/// стрелки, начиная с `active[0]`) кидают кости; первая, у кого на кости выпала
/// **6**, получает право начать.
///
/// # Panics
/// Если `active` пуст. С `RandomDice` завершается почти наверное; со `ScriptedDice`
/// требует, чтобы в сценарии в конце концов выпала шестёрка.
pub fn determine_first<D: DiceSource>(dice: &mut D, active: &[Side]) -> Side {
    assert!(!active.is_empty(), "нужна хотя бы одна сторона");
    let mut side = active[0];
    loop {
        if dice.roll().has_value(6) {
            return side;
        }
        side = next_active_side(active, side);
    }
}

/// Команда стороны при игре 2×2 (противоположные стороны — союзники): 0 = A/C, 1 = B/D.
pub fn team_of(side: Side) -> usize {
    side.index() % 2
}

/// Победители партии (стороны с `z = 1`), либо `None`, если партия не окончена.
/// Командный режим (`state.teams` при 4 сторонах): победа, когда **обе** стороны
/// команды завели фишки в Дом → возвращаются обе. Иначе (каждый сам за себя, вкл.
/// 2 игрока): первая финишировавшая сторона.
pub fn winners_of(state: &GameState) -> Option<Vec<Side>> {
    if state.teams && state.active.len() == 4 {
        for t in 0..2 {
            let members: Vec<Side> = state
                .active
                .iter()
                .copied()
                .filter(|&s| team_of(s) == t)
                .collect();
            if !members.is_empty() && members.iter().all(|&s| state.has_won(s)) {
                return Some(members);
            }
        }
        None
    } else {
        state
            .active
            .iter()
            .copied()
            .find(|&s| state.has_won(s))
            .map(|s| vec![s])
    }
}

/// Следующая активная сторона в порядке против часовой стрелки (A→B→C→D).
pub fn next_active_side(active: &[Side], current: Side) -> Side {
    for step in 1..=Side::ALL.len() {
        let cand = Side::from_index(current.index() + step);
        if active.contains(&cand) {
            return cand;
        }
    }
    current
}

/// Следующая активная сторона, ещё НЕ заведшая все фишки в Дом (финишировавшие
/// стороны пропускаются — их очередь не наступает). Если все финишировали — `current`.
pub fn next_unfinished_active(state: &GameState, current: Side) -> Side {
    let mut s = current;
    for _ in 0..Side::ALL.len() {
        s = next_active_side(&state.active, s);
        if !state.has_won(s) {
            return s;
        }
    }
    current
}

/// Итог одного броска в рамках хода игрока.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    /// Кто ходил.
    pub side: Side,
    /// Что выпало.
    pub roll: DiceRoll,
    /// Фактически применённые ходы игрока (без пропущенных из-за ответа захватчика).
    pub played: Vec<Move>,
    /// Вынужденные ответные ходы захватчиков, вызванные выкупами в этом ходе.
    pub forced: Vec<Move>,
    /// Все применённые ходы В ПОРЯДКЕ применения: ход игрока, а сразу за выкупом —
    /// обязательный ответ захватчика. Удобно для анимации.
    pub applied: Vec<Move>,
    /// Дубль: тот же игрок получает ещё один ход.
    pub again: bool,
}

/// Партия: состояние плюс логика очереди ходов.
#[derive(Clone, Debug)]
pub struct Game {
    /// Текущее состояние партии.
    pub state: GameState,
}

impl Game {
    /// Новая партия поверх готового состояния.
    pub fn new(state: GameState) -> Game {
        Game { state }
    }

    /// Новая партия с розыгрышем права первого хода: первым ходит тот, кто
    /// раньше всех выбросил «6» (см. `determine_first`).
    pub fn start<D: DiceSource>(active: Vec<Side>, dice: &mut D) -> Game {
        let first = determine_first(dice, &active);
        Game::new(GameState::new(active, first))
    }

    /// Победитель — сторона, все фишки которой в Доме (если есть).
    pub fn winner(&self) -> Option<Side> {
        self.state
            .active
            .iter()
            .copied()
            .find(|&s| self.state.has_won(s))
    }

    /// Победители партии с учётом режима (см. `winners_of`).
    pub fn winners(&self) -> Option<Vec<Side>> {
        winners_of(&self.state)
    }

    /// Применяет уже выбранную последовательность ходов и завершает ход.
    ///
    /// Каждый выкуп (`Ransom`) тут же влечёт обязательный ответный ход захватчика
    /// на «6»; конкретный ход выбирает `forced(state, captor, &options)` (индекс).
    /// При дубле ход остаётся за тем же игроком, иначе переходит к следующему.
    /// Это общий «движок завершения хода» для ИИ (`play_turn`) и интерактива.
    pub fn commit_turn<F>(
        &mut self,
        roll: DiceRoll,
        played: Vec<Move>,
        mut forced: F,
    ) -> TurnOutcome
    where
        F: FnMut(&GameState, Side, &[Move]) -> usize,
    {
        let side = self.state.to_move;
        let mut applied_player = Vec::new();
        let mut forced_moves = Vec::new();
        let mut applied = Vec::new();
        for mv in played {
            // Ответный ход захватчика на предыдущий выкуп мог сделать этот ход
            // нелегальным (например, съесть фишку, которой собирались ходить) —
            // тогда пропускаем его.
            if !move_legal(&self.state, mv) {
                continue;
            }
            // Захватчик известен до применения выкупа (после фишка — в резерве).
            let captor = if mv.kind == MoveKind::Ransom {
                match self.state.checkers[mv.checker].pos {
                    Position::Captured { captor } => Some(captor),
                    _ => None,
                }
            } else {
                None
            };
            self.state = apply(&self.state, mv);
            applied_player.push(mv);
            applied.push(mv);
            // Обязательный ответ захватчика — СРАЗУ после выкупа.
            if let Some(captor) = captor {
                let options = forced_six_moves(&self.state, captor);
                if !options.is_empty() {
                    let idx = forced(&self.state, captor, &options).min(options.len() - 1);
                    self.state = apply(&self.state, options[idx]);
                    forced_moves.push(options[idx]);
                    applied.push(options[idx]);
                }
            }
        }
        let played = applied_player;

        // Дубль даёт ещё ход — но только если сторона ещё не финишировала.
        let again = roll.is_double() && !self.state.has_won(side);
        if !again {
            // Очередь переходит к следующей НЕфинишировавшей стороне.
            self.state.to_move = next_unfinished_active(&self.state, side);
        }
        TurnOutcome {
            side,
            roll,
            played,
            applied,
            forced: forced_moves,
            again,
        }
    }

    /// Один бросок текущего игрока: кидает кости, даёт `agent` выбрать ход и
    /// применяет его (включая вынужденные ответы захватчиков при выкупе).
    pub fn play_turn<D, A>(&mut self, dice: &mut D, agent: &mut A) -> TurnOutcome
    where
        D: DiceSource,
        A: Agent,
    {
        let roll = dice.roll();
        let turns = legal_turns(&self.state, roll);
        let idx = agent.choose_turn(&self.state, &turns).min(turns.len() - 1);
        let played = turns[idx].clone();
        self.commit_turn(roll, played, |state, captor, options| {
            agent.choose_forced(state, captor, options)
        })
    }

    /// Гоняет ходы до появления победителя, но не более `max_turns` бросков
    /// (страховка от зацикливания). Возвращает победителя, если он определился.
    pub fn play_until_winner<D, A>(
        &mut self,
        dice: &mut D,
        agent: &mut A,
        max_turns: usize,
    ) -> Option<Side>
    where
        D: DiceSource,
        A: Agent,
    {
        for _ in 0..max_turns {
            if let Some(w) = self.winner() {
                return Some(w);
            }
            self.play_turn(dice, agent);
        }
        self.winner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Position;

    fn roll(a: u8, b: u8) -> DiceRoll {
        DiceRoll::new(Die::new(a).unwrap(), Die::new(b).unwrap())
    }

    /// Тестовый агент: выбирает последовательность с выкупом, иначе первую.
    struct RansomFirst;
    impl Agent for RansomFirst {
        fn choose_turn(&mut self, _: &GameState, turns: &[Vec<Move>]) -> usize {
            turns
                .iter()
                .position(|t| t.iter().any(|m| m.kind == MoveKind::Ransom))
                .unwrap_or(0)
        }
    }

    #[test]
    fn turn_order_skips_inactive_sides() {
        assert_eq!(next_active_side(&[Side::A, Side::C], Side::A), Side::C);
        assert_eq!(next_active_side(&[Side::A, Side::C], Side::C), Side::A);
        assert_eq!(next_active_side(&Side::ALL, Side::A), Side::B);
        assert_eq!(next_active_side(&Side::ALL, Side::D), Side::A);
    }

    #[test]
    fn first_move_roll_off() {
        // A кидает (2,3) — нет 6; C кидает (4,1) — нет 6; A кидает (6,2) — 6 → начинает A.
        let mut dice = ScriptedDice::new(vec![roll(2, 3), roll(4, 1), roll(6, 2)]);
        assert_eq!(determine_first(&mut dice, &[Side::A, Side::C]), Side::A);

        // Первый же бросок с шестёркой выигрывает право хода.
        let mut dice = ScriptedDice::new(vec![roll(1, 6)]);
        assert_eq!(determine_first(&mut dice, &[Side::A, Side::C]), Side::A);

        // Второй игрок выбрасывает 6 первым.
        let mut dice = ScriptedDice::new(vec![roll(1, 1), roll(5, 6)]);
        assert_eq!(determine_first(&mut dice, &[Side::A, Side::C]), Side::C);
    }

    #[test]
    fn non_double_passes_turn() {
        let mut game = Game::new(GameState::new(vec![Side::A, Side::C], Side::A));
        let mut dice = ScriptedDice::new(vec![roll(6, 3)]);
        let outcome = game.play_turn(&mut dice, &mut FirstChoice);
        assert_eq!(outcome.side, Side::A);
        assert!(!outcome.again);
        assert_eq!(game.state.to_move, Side::C);
    }

    #[test]
    fn double_grants_extra_turn_to_same_side() {
        let mut game = Game::new(GameState::new(vec![Side::A, Side::C], Side::A));
        // Дубль 6-6: можно ввести две фишки и сходить ещё раз.
        let mut dice = ScriptedDice::new(vec![roll(6, 6)]);
        let outcome = game.play_turn(&mut dice, &mut FirstChoice);
        assert!(outcome.again);
        assert_eq!(game.state.to_move, Side::A);
        assert_eq!(outcome.played.len(), 2);
        assert_eq!(outcome.played.iter().map(|m| m.pips).sum::<u8>(), 12);
        assert!(
            game.state
                .checkers
                .iter()
                .any(|c| c.owner == Side::A && !matches!(c.pos, Position::Reserve))
        );
    }

    #[test]
    fn detects_winner() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        for (depth, c) in state
            .checkers
            .iter_mut()
            .filter(|c| c.owner == Side::A)
            .enumerate()
        {
            c.pos = Position::Home { depth: depth as u8 };
        }
        let game = Game::new(state);
        assert_eq!(game.winner(), Some(Side::A));
    }

    #[test]
    fn ransom_frees_captive_and_forces_captor_six_move() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        // A: фишка 0 в плену у C; остальные A — в резерве.
        state.checkers[0].pos = Position::Captured { captor: Side::C };
        // C: фишка 4 на дорожке (единственный её 6-ход); 5..7 в Доме (ходов на 6 нет).
        state.checkers[4].pos = Position::OnTrack { progress: 0 };
        state.checkers[5].pos = Position::Home { depth: 0 };
        state.checkers[6].pos = Position::Home { depth: 1 };
        state.checkers[7].pos = Position::Home { depth: 2 };
        let mut game = Game::new(state);

        let mut dice = ScriptedDice::new(vec![roll(6, 6)]);
        let outcome = game.play_turn(&mut dice, &mut RansomFirst);

        // Пленная фишка больше не в плену (в резерве либо уже введена).
        assert!(!matches!(
            game.state.checkers[0].pos,
            Position::Captured { .. }
        ));
        // Захватчик сделал ровно один вынужденный ход на 6: фишка 4 продвинулась.
        assert_eq!(outcome.forced.len(), 1);
        assert_eq!(
            game.state.checkers[4].pos,
            Position::OnTrack { progress: 6 }
        );
    }

    #[test]
    fn ransom_skipped_when_captor_has_no_six_move() {
        let mut state = GameState::new(vec![Side::A, Side::C], Side::A);
        state.checkers[0].pos = Position::Captured { captor: Side::C };
        // У C нет ни одного хода на «6»: все его фишки в Доме.
        for (depth, c) in state
            .checkers
            .iter_mut()
            .filter(|c| c.owner == Side::C)
            .enumerate()
        {
            c.pos = Position::Home { depth: depth as u8 };
        }
        let mut game = Game::new(state);

        // Дубль 6-6, чтобы выкуп вошёл в максимальный по очкам ход.
        let mut dice = ScriptedDice::new(vec![roll(6, 6)]);
        let outcome = game.play_turn(&mut dice, &mut RansomFirst);
        assert!(!matches!(
            game.state.checkers[0].pos,
            Position::Captured { .. }
        ));
        // Обязательный ход пропущен — у захватчика нет хода на 6.
        assert!(outcome.forced.is_empty());
    }

    #[test]
    fn random_dice_is_deterministic_for_seed() {
        let mut a = RandomDice::from_seed(42);
        let mut b = RandomDice::from_seed(42);
        for _ in 0..10 {
            let ra = a.roll();
            let rb = b.roll();
            assert_eq!(ra.values(), rb.values());
            for v in ra.values() {
                assert!((1..=6).contains(&v));
            }
        }
    }

    #[test]
    fn random_game_runs_without_panicking_and_keeps_invariants() {
        // Дымовой тест: гоняем много случайных ходов и проверяем инварианты.
        let mut game = Game::new(GameState::new(vec![Side::A, Side::C], Side::A));
        let mut dice = RandomDice::from_seed(7);
        for _ in 0..5_000 {
            if game.winner().is_some() {
                break;
            }
            let before = game.state.checkers.len();
            let outcome = game.play_turn(&mut dice, &mut FirstChoice);
            assert_eq!(game.state.checkers.len(), before);
            assert!(game.state.active.contains(&outcome.side));
            assert!(game.state.active.contains(&game.state.to_move));
        }
        assert!(
            game.state
                .checkers
                .iter()
                .any(|c| !matches!(c.pos, Position::Reserve))
        );
    }
}
