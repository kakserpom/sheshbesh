//! Линейная функция ценности и её обучение методом **TD(λ)** в self-play
//! (подход TD-Gammon), на чистом Rust — без внешних зависимостей.
//!
//! Ценность `V(s, p) = sigmoid(b + w·φ(s, p)) ∈ (0, 1)` — оценка вероятности, что
//! сторона `p` выиграет из позиции `s` (признаки `φ` — см. `encode`). Агент ходит
//! жадно по ценности **афтерстейта** (`ValueAgent`).
//!
//! Обучение (2 игрока, zero-sum). Self-play текущими весами даёт траекторию
//! афтерстейтов с пометкой, кто их создал. Для каждой стороны `p` берём её
//! подпоследовательность афтерстейтов и применяем TD(λ) (backward-view, γ = 1):
//! бутстрап к ценности **своего следующего** афтерстейта, а в конце — терминал
//! `z = 1` победителю и `0` проигравшему. Так значение учится быть вероятностью
//! победы. Все афтерстейты — «ход только что сделан, очередь соперника», поэтому
//! очередь хода в признаки кодировать не нужно.

use crate::board::Side;
use crate::encode::{FEATURES, encode_into};
use crate::moves::legal_turns;
use crate::state::GameState;
use crate::turn::{DiceSource, Game, RandomDice};
use crate::value::{Value, best_forced, best_turn};

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Линейная функция ценности над признаками `encode` (сигмоида на выходе).
#[derive(Clone)]
pub struct LinearValue {
    /// Веса по признакам (длина `FEATURES`).
    pub w: Vec<f32>,
    /// Смещение.
    pub bias: f32,
}

impl Default for LinearValue {
    fn default() -> Self {
        Self::zeros()
    }
}

impl LinearValue {
    /// Нулевая модель (старт обучения): ценность любой позиции — 0.5.
    pub fn zeros() -> LinearValue {
        LinearValue {
            w: vec![0.0; FEATURES],
            bias: 0.0,
        }
    }

    /// Из плоского среза (`FEATURES` весов + смещение последним) — загрузка.
    ///
    /// # Panics
    /// Если длина среза не `FEATURES + 1`.
    pub fn from_floats(f: &[f32]) -> LinearValue {
        assert_eq!(f.len(), FEATURES + 1, "ожидается FEATURES+1 чисел");
        LinearValue {
            w: f[..FEATURES].to_vec(),
            bias: f[FEATURES],
        }
    }

    /// В плоский вектор (`FEATURES` весов + смещение) — сохранение/экспорт в GUI.
    pub fn to_floats(&self) -> Vec<f32> {
        let mut v = self.w.clone();
        v.push(self.bias);
        v
    }

    /// Линейная комбинация `b + w·φ` (до сигмоиды).
    fn raw(&self, phi: &[f32]) -> f32 {
        let mut s = self.bias;
        for (wi, xi) in self.w.iter().zip(phi) {
            s += wi * xi;
        }
        s
    }

    /// TD(λ)-обновление по траектории одной партии (афтерстейты + их авторы).
    fn td_update(&mut self, traj: &[(GameState, Side)], winner: Side, alpha: f32, lambda: f32) {
        for p in [Side::A, Side::C] {
            let states: Vec<&GameState> = traj
                .iter()
                .filter(|(_, m)| *m == p)
                .map(|(s, _)| s)
                .collect();
            if states.is_empty() {
                continue;
            }
            let z = f32::from(winner == p);
            let mut e = vec![0.0f32; FEATURES];
            let mut e_bias = 0.0f32;
            let mut buf = [0.0f32; FEATURES];
            for t in 0..states.len() {
                encode_into(states[t], p, &mut buf);
                let v = sigmoid(self.raw(&buf));
                let v_next = if t + 1 < states.len() {
                    self.value(states[t + 1], p)
                } else {
                    z
                };
                let delta = v_next - v;
                let grad = v * (1.0 - v); // dV/d(raw)
                for j in 0..FEATURES {
                    e[j] = lambda * e[j] + grad * buf[j];
                    self.w[j] += alpha * delta * e[j];
                }
                e_bias = lambda * e_bias + grad;
                self.bias += alpha * delta * e_bias;
            }
        }
    }
}

impl Value for LinearValue {
    fn value(&self, state: &GameState, side: Side) -> f32 {
        let mut buf = [0.0f32; FEATURES];
        encode_into(state, side, &mut buf);
        sigmoid(self.raw(&buf))
    }
}

/// Гиперпараметры обучения TD(λ).
pub struct TdConfig {
    /// Сколько self-play партий сыграть.
    pub games: usize,
    /// Начальный шаг обучения.
    pub alpha: f32,
    /// Затухание шага: на партии `g` действует `alpha / (1 + alpha_decay·g)`.
    /// Гасит дрожание/расхождение TD по мере дрейфа self-play-политики.
    pub alpha_decay: f32,
    /// Параметр следов приемлемости λ (0 — чистый TD(0), 1 — Монте-Карло).
    pub lambda: f32,
    /// Предел бросков на партию (страховка от зацикливания).
    pub max_turns: usize,
    /// Зерно ГПСЧ костей.
    pub seed: u64,
}

impl Default for TdConfig {
    fn default() -> Self {
        // Подобрано перебором: устойчиво даёт ~58–61% против Heuristic; больше
        // партий улучшает медленно. ~13с на партию обучения (release).
        TdConfig {
            games: 10_000,
            alpha: 0.1,
            alpha_decay: 0.001,
            lambda: 0.7,
            max_turns: 20_000,
            seed: 1,
        }
    }
}

/// Self-play партия текущей моделью (2 игрока, A против C, жадно по ценности).
/// Возвращает траекторию афтерстейтов (позиция, кто ходил) и победителя.
fn play_and_record(
    model: &LinearValue,
    dice: &mut impl DiceSource,
    max_turns: usize,
) -> (Vec<(GameState, Side)>, Option<Side>) {
    let mut game = Game::new(GameState::new(vec![Side::A, Side::C], Side::A));
    let mut traj = Vec::new();
    for _ in 0..max_turns {
        if let Some(w) = game.winner() {
            return (traj, Some(w));
        }
        let mover = game.state.to_move;
        let roll = dice.roll();
        let turns = legal_turns(&game.state, roll);
        let idx = best_turn(model, &game.state, &turns).min(turns.len() - 1);
        let played = turns[idx].clone();
        game.commit_turn(roll, played, |s, captor, opts| {
            best_forced(model, s, captor, opts)
        });
        traj.push((game.state.clone(), mover));
    }
    (traj, game.winner())
}

/// Обучает линейную ценность TD(λ) в self-play и возвращает модель.
pub fn train(cfg: &TdConfig) -> LinearValue {
    let mut model = LinearValue::zeros();
    let mut dice = RandomDice::from_seed(cfg.seed);
    for g in 0..cfg.games {
        let (traj, winner) = play_and_record(&model, &mut dice, cfg.max_turns);
        if let Some(w) = winner {
            let alpha = cfg.alpha / (1.0 + cfg.alpha_decay * g as f32);
            model.td_update(&traj, w, alpha, cfg.lambda);
        }
    }
    model
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Heuristic;
    use crate::eval::match_winrate;
    use crate::turn::FirstChoice;
    use crate::value::ValueAgent;

    #[test]
    fn untrained_value_is_half() {
        let m = LinearValue::zeros();
        let s = GameState::new(vec![Side::A, Side::C], Side::A);
        assert!((m.value(&s, Side::A) - 0.5).abs() < 1e-6);
    }

    #[test]
    #[allow(clippy::float_cmp)] // round-trip — значения должны совпасть точно
    fn floats_round_trip() {
        let mut m = LinearValue::zeros();
        m.w[0] = 1.5;
        m.w[7] = -2.0;
        m.bias = 0.25;
        let back = LinearValue::from_floats(&m.to_floats());
        assert_eq!(back.w, m.w);
        assert_eq!(back.bias, m.bias);
    }

    // Обучение через self-play медленное — запускать вручную (лучше с --release):
    //   cargo test --release -p sheshbesh trained_beats -- --ignored --nocapture
    #[test]
    #[ignore = "медленно: self-play обучение"]
    fn trained_beats_first_choice() {
        let model = train(&TdConfig {
            games: 5000,
            ..Default::default()
        });
        let (v, f) = match_winrate(&mut ValueAgent(&model), &mut FirstChoice, 200, 99, 40_000);
        assert!(
            v > f,
            "обученная модель должна обыгрывать FirstChoice: {v} к {f}"
        );
        // Главная цель фазы 2: обученная ценность сильнее ручной эвристики (~58–61%).
        let (vh, h) = match_winrate(&mut ValueAgent(&model), &mut Heuristic, 400, 4242, 40_000);
        println!(
            "vs Heuristic: {vh}:{h} ({:.1}%)",
            100.0 * f64::from(vh as u32) / f64::from((vh + h).max(1) as u32)
        );
        assert!(
            vh > h,
            "обученная модель должна обыгрывать Heuristic: {vh} к {h}"
        );
    }
}
