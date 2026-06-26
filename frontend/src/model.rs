use crate::i18n::*;
use leptos_i18n::I18nContext;
use sheshbesh::{GameState, Heuristic, LinearValue, MlpValue, Side, Value};

pub(crate) static MODEL_2P: &[u8] = include_bytes!("model.bin");
pub(crate) static MODEL_3P: &[u8] = include_bytes!("model_3p.bin");
pub(crate) static MODEL_4P: &[u8] = include_bytes!("model_4p.bin");
pub(crate) static MODEL_4P_TEAMS: &[u8] = include_bytes!("model_4p_teams.bin");
pub(crate) static LIN_2P: &[u8] = include_bytes!("model_lin.bin");
pub(crate) static LIN_3P: &[u8] = include_bytes!("model_lin_3p.bin");
pub(crate) static LIN_4P: &[u8] = include_bytes!("model_lin_4p.bin");
pub(crate) static LIN_4P_TEAMS: &[u8] = include_bytes!("model_lin_4p_teams.bin");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Algo {
    Mlp,
    Search,
    Linear,
    Heuristic,
}

impl Algo {
    pub(crate) const ALL: [Algo; 4] = [Algo::Mlp, Algo::Search, Algo::Linear, Algo::Heuristic];

    pub(crate) fn label(self, i18n: I18nContext<Locale>) -> String {
        match self {
            Algo::Mlp => t_string!(i18n, algo_mlp).to_string(),
            Algo::Search => t_string!(i18n, algo_search).to_string(),
            Algo::Linear => t_string!(i18n, algo_linear).to_string(),
            Algo::Heuristic => t_string!(i18n, algo_heuristic).to_string(),
        }
    }

    pub(crate) fn is_search(self) -> bool {
        self == Algo::Search
    }
}

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

pub(crate) fn ai_model_for(algo: Algo, active_len: usize, teams: bool) -> Ai {
    let idx = match (active_len, teams) {
        (4, true) => 3,
        (4, false) => 2,
        (3, _) => 1,
        _ => 0,
    };
    match algo {
        Algo::Heuristic => Ai::Heuristic(Heuristic),
        Algo::Mlp | Algo::Search => {
            let bytes = [MODEL_2P, MODEL_3P, MODEL_4P, MODEL_4P_TEAMS][idx];
            Ai::Mlp(MlpValue::from_floats(&floats(bytes)))
        }
        Algo::Linear => {
            let bytes = [LIN_2P, LIN_3P, LIN_4P, LIN_4P_TEAMS][idx];
            Ai::Linear(LinearValue::from_floats(&floats(bytes)))
        }
    }
}
