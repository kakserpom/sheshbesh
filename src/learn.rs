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

/// Афтерстейты стороны `p` (позиции, которые она создала своим ходом) по порядку.
fn afterstates_of(traj: &[(GameState, Side)], p: Side) -> Vec<&GameState> {
    traj.iter()
        .filter(|(_, m)| *m == p)
        .map(|(s, _)| s)
        .collect()
}

/// Обучаемая методом TD(λ) функция ценности.
pub trait TdModel: Value {
    /// TD(λ)-обновление по траектории одной партии (афтерстейты + их авторы) и
    /// победителю: для каждой стороны бутстрап к её следующему афтерстейту, в конце —
    /// терминал `z = 1` победителю / `0` проигравшему (γ = 1).
    fn td_update(&mut self, traj: &[(GameState, Side)], winner: Side, alpha: f32, lambda: f32);
}

/// Мини-ГПСЧ `SplitMix64` для инициализации весов (детерминирован от зерна).
struct SplitMix(u64);
impl SplitMix {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Случайное число в `[-1, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
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
}

impl Value for LinearValue {
    fn value(&self, state: &GameState, side: Side) -> f32 {
        let mut buf = [0.0f32; FEATURES];
        encode_into(state, side, &mut buf);
        sigmoid(self.raw(&buf))
    }
}

impl TdModel for LinearValue {
    #[allow(clippy::needless_range_loop)] // индексация по признакам нагляднее
    fn td_update(&mut self, traj: &[(GameState, Side)], winner: Side, alpha: f32, lambda: f32) {
        for p in [Side::A, Side::C] {
            let states = afterstates_of(traj, p);
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

/// Однослойный перцептрон (MLP) как функция ценности: `sigmoid(b2 + w2·h)`, где
/// `h = sigmoid(b1 + w1·φ)`. В отличие от линейной модели улавливает нелинейные
/// взаимодействия признаков (контакт/угрозы — произведения занятостей клеток).
#[derive(Clone)]
pub struct MlpValue {
    hidden: usize,
    w1: Vec<f32>, // hidden × FEATURES, построчно: [j*FEATURES + k]
    b1: Vec<f32>, // hidden
    w2: Vec<f32>, // hidden
    b2: f32,
}

impl MlpValue {
    /// Случайная инициализация `hidden` скрытых нейронов (масштаб ~`1/sqrt(fan_in)`).
    pub fn new(hidden: usize, seed: u64) -> MlpValue {
        let mut rng = SplitMix(seed ^ 0x1234_5678_9ABC_DEF0);
        let r1 = 1.0 / (FEATURES as f32).sqrt();
        let r2 = 1.0 / (hidden as f32).sqrt();
        MlpValue {
            hidden,
            w1: (0..hidden * FEATURES).map(|_| rng.unit() * r1).collect(),
            b1: vec![0.0; hidden],
            w2: (0..hidden).map(|_| rng.unit() * r2).collect(),
            b2: 0.0,
        }
    }

    /// Прямой проход: заполняет активации `h` (длина `hidden`) и возвращает ценность.
    #[allow(clippy::needless_range_loop)]
    fn forward(&self, phi: &[f32], h: &mut [f32]) -> f32 {
        for j in 0..self.hidden {
            let row = &self.w1[j * FEATURES..(j + 1) * FEATURES];
            let mut z = self.b1[j];
            for (wjk, xk) in row.iter().zip(phi) {
                z += wjk * xk;
            }
            h[j] = sigmoid(z);
        }
        let mut z2 = self.b2;
        for j in 0..self.hidden {
            z2 += self.w2[j] * h[j];
        }
        sigmoid(z2)
    }

    /// Плоский вектор всех весов (w1, b1, w2, b2) — для сохранения/экспорта в GUI.
    pub fn to_floats(&self) -> Vec<f32> {
        let mut v = Vec::with_capacity(self.w1.len() + 2 * self.hidden + 1);
        v.extend_from_slice(&self.w1);
        v.extend_from_slice(&self.b1);
        v.extend_from_slice(&self.w2);
        v.push(self.b2);
        v
    }
}

impl Value for MlpValue {
    fn value(&self, state: &GameState, side: Side) -> f32 {
        let mut buf = [0.0f32; FEATURES];
        encode_into(state, side, &mut buf);
        let mut h = vec![0.0f32; self.hidden];
        self.forward(&buf, &mut h)
    }
}

impl TdModel for MlpValue {
    #[allow(clippy::needless_range_loop)] // индексация по нейронам/признакам нагляднее
    fn td_update(&mut self, traj: &[(GameState, Side)], winner: Side, alpha: f32, lambda: f32) {
        for p in [Side::A, Side::C] {
            let states = afterstates_of(traj, p);
            if states.is_empty() {
                continue;
            }
            let z = f32::from(winner == p);
            // Следы приемлемости на каждый параметр.
            let mut e_w1 = vec![0.0f32; self.w1.len()];
            let mut e_b1 = vec![0.0f32; self.hidden];
            let mut e_w2 = vec![0.0f32; self.hidden];
            let mut e_b2 = 0.0f32;
            let mut phi = [0.0f32; FEATURES];
            let mut h = vec![0.0f32; self.hidden];
            for t in 0..states.len() {
                encode_into(states[t], p, &mut phi);
                let v = self.forward(&phi, &mut h);
                let v_next = if t + 1 < states.len() {
                    self.value(states[t + 1], p)
                } else {
                    z
                };
                let delta = v_next - v;
                let d = v * (1.0 - v); // dV/d(z2)
                // Скрытый слой и веса выхода.
                for j in 0..self.hidden {
                    let hd = d * self.w2[j] * h[j] * (1.0 - h[j]); // dV/d(z1_j)
                    e_w2[j] = lambda * e_w2[j] + d * h[j];
                    self.w2[j] += alpha * delta * e_w2[j];
                    e_b1[j] = lambda * e_b1[j] + hd;
                    self.b1[j] += alpha * delta * e_b1[j];
                    let base = j * FEATURES;
                    for k in 0..FEATURES {
                        let idx = base + k;
                        e_w1[idx] = lambda * e_w1[idx] + hd * phi[k];
                        self.w1[idx] += alpha * delta * e_w1[idx];
                    }
                }
                e_b2 = lambda * e_b2 + d;
                self.b2 += alpha * delta * e_b2;
            }
        }
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
fn play_and_record<V: Value>(
    model: &V,
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

/// Обучает произвольную `TdModel` методом TD(λ) в self-play и возвращает её.
pub fn train_with<M: TdModel>(mut model: M, cfg: &TdConfig) -> M {
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

/// Обучает линейную ценность TD(λ) в self-play.
pub fn train(cfg: &TdConfig) -> LinearValue {
    train_with(LinearValue::zeros(), cfg)
}

/// Обучает MLP-ценность с `hidden` скрытыми нейронами TD(λ) в self-play.
pub fn train_mlp(hidden: usize, cfg: &TdConfig) -> MlpValue {
    train_with(MlpValue::new(hidden, cfg.seed), cfg)
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

    #[test]
    fn untrained_mlp_value_is_finite_and_centered() {
        // Случайная инициализация → ценность конечна и около 0.5 (без NaN/насыщения).
        let m = MlpValue::new(40, 1);
        let s = GameState::new(vec![Side::A, Side::C], Side::A);
        let v = m.value(&s, Side::A);
        assert!(v.is_finite() && v > 0.2 && v < 0.8, "v={v}");
    }

    // Запускать вручную (лучше с --release):
    //   cargo test --release -p sheshbesh trained_mlp -- --ignored --nocapture
    #[test]
    #[ignore = "медленно: self-play обучение MLP"]
    fn trained_mlp_beats_heuristic() {
        // h=20, α=0.05 — лучшая из найденных конфигураций MLP (~58–60% vs Heuristic).
        let model = train_mlp(
            20,
            &TdConfig {
                games: 5000,
                alpha: 0.05,
                alpha_decay: 0.0005,
                ..Default::default()
            },
        );
        let (vh, h) = match_winrate(&mut ValueAgent(&model), &mut Heuristic, 400, 4242, 40_000);
        println!(
            "MLP vs Heuristic: {vh}:{h} ({:.1}%)",
            100.0 * f64::from(vh as u32) / f64::from((vh + h).max(1) as u32)
        );
        assert!(
            vh > h,
            "обученный MLP должен обыгрывать Heuristic: {vh} к {h}"
        );
    }
}
