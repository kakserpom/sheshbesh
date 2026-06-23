use sheshbesh::{GameState, Heuristic, LinearValue, MlpValue, Side, Value};

// Веса обученных TD(λ)-ценностей (LE f32) под каждый режим — компьютер ходит ими.
// MLP — `examples/export_model.rs`; линейные — `examples/export_linear.rs`.
pub(crate) static MODEL_2P: &[u8] = include_bytes!("model.bin");
pub(crate) static MODEL_3P: &[u8] = include_bytes!("model_3p.bin");
pub(crate) static MODEL_4P: &[u8] = include_bytes!("model_4p.bin");
pub(crate) static MODEL_4P_TEAMS: &[u8] = include_bytes!("model_4p_teams.bin");
pub(crate) static LIN_2P: &[u8] = include_bytes!("model_lin.bin");
pub(crate) static LIN_3P: &[u8] = include_bytes!("model_lin_3p.bin");
pub(crate) static LIN_4P: &[u8] = include_bytes!("model_lin_4p.bin");
pub(crate) static LIN_4P_TEAMS: &[u8] = include_bytes!("model_lin_4p_teams.bin");

/// Алгоритм компьютера для одной стороны — выбирается в настройках.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Algo {
    /// Однослойный перцептрон (по умолчанию, сильнейший).
    Mlp,
    /// Линейная TD(λ)-ценность (быстрее, чуть слабее).
    Linear,
    /// Жадная эвристика (без обучения).
    Heuristic,
}

impl Algo {
    pub(crate) const ALL: [Algo; 3] = [Algo::Mlp, Algo::Linear, Algo::Heuristic];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Algo::Mlp => "MLP",
            Algo::Linear => "Linear",
            Algo::Heuristic => "Эвристика",
        }
    }
}

/// Готовая функция ценности под выбранный алгоритм (ход выбирается `best_turn`).
pub(crate) enum Ai {
    Mlp(MlpValue),
    Linear(LinearValue),
    Heuristic(Heuristic),
}

impl Value for Ai {
    fn value(&self, state: &GameState, side: Side) -> f32 {
        match self {
            Ai::Mlp(m) => m.value(state, side),
            Ai::Linear(m) => m.value(state, side),
            Ai::Heuristic(h) => h.value(state, side),
        }
    }
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Агент-ценность под алгоритм, число сторон и режим (команды 2×2). Для MLP число
/// скрытых нейронов выводится из длины весов (`MlpValue::from_floats`).
pub(crate) fn ai_model_for(algo: Algo, active_len: usize, teams: bool) -> Ai {
    let idx = match (active_len, teams) {
        (4, true) => 3,
        (4, false) => 2,
        (3, _) => 1,
        _ => 0,
    };
    match algo {
        Algo::Heuristic => Ai::Heuristic(Heuristic),
        Algo::Mlp => {
            let bytes = [MODEL_2P, MODEL_3P, MODEL_4P, MODEL_4P_TEAMS][idx];
            Ai::Mlp(MlpValue::from_floats(&floats(bytes)))
        }
        Algo::Linear => {
            let bytes = [LIN_2P, LIN_3P, LIN_4P, LIN_4P_TEAMS][idx];
            Ai::Linear(LinearValue::from_floats(&floats(bytes)))
        }
    }
}
