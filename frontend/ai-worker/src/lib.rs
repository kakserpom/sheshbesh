use serde::{Deserialize, Serialize};
use sheshbesh::{
    Checker, DEFAULT_WIDTH, GameState, Heuristic, LinearValue, MlpValue, MoonField, PerimeterIdx,
    Position, Side, Value, best_forced, best_turn, best_turn_2ply, legal_turns_remaining,
};
use wasm_bindgen::prelude::*;

static MODEL_2P: &[u8] = include_bytes!("../../src/model.bin");
static MODEL_3P: &[u8] = include_bytes!("../../src/model_3p.bin");
static MODEL_4P: &[u8] = include_bytes!("../../src/model_4p.bin");
static MODEL_4P_TEAMS: &[u8] = include_bytes!("../../src/model_4p_teams.bin");
static LIN_2P: &[u8] = include_bytes!("../../src/model_lin.bin");
static LIN_3P: &[u8] = include_bytes!("../../src/model_lin_3p.bin");
static LIN_4P: &[u8] = include_bytes!("../../src/model_lin_4p.bin");
static LIN_4P_TEAMS: &[u8] = include_bytes!("../../src/model_lin_4p_teams.bin");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Algo {
    Mlp,
    Search,
    Linear,
    Heuristic,
}

impl Algo {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Algo::Mlp,
            1 => Algo::Search,
            2 => Algo::Linear,
            _ => Algo::Heuristic,
        }
    }
}

enum Ai {
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

fn ai_model_for(algo: Algo, active_len: usize, teams: bool) -> Ai {
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

#[derive(Deserialize)]
struct Request {
    id: u64,
    mode: String,
    state: RawState,
    roll: [u8; 2],
    rem: Option<Vec<u8>>,
    algo: u8,
    captor: Option<u8>,
}

#[derive(Deserialize)]
struct RawState {
    a: Vec<u8>,
    m: u8,
    t: bool,
    c: Vec<Vec<u8>>,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    idx: Option<usize>,
}

fn to_side(v: u8) -> Side {
    const SIDES: [Side; 4] = [Side::A, Side::B, Side::C, Side::D];
    SIDES[v as usize]
}

fn reconstruct_state(raw: RawState) -> GameState {
    let active: Vec<Side> = raw.a.iter().map(|&v| to_side(v)).collect();
    let mut state = GameState::new(active.clone(), to_side(raw.m));
    state.clear_checkers();
    for c in &raw.c {
        let owner = to_side(c[0]);
        let pos = match c[1] {
            0 => Position::Reserve,
            1 => Position::OnTrack {
                progress: c[2] as u16,
            },
            2 => {
                let field = match c[3] {
                    0 => MoonField::One,
                    1 => MoonField::Three,
                    _ => MoonField::Six,
                };
                Position::Moon {
                    side: to_side(c[2]),
                    field,
                }
            }
            3 => Position::Prison {
                cell: PerimeterIdx::new(c[2] as usize),
            },
            4 => Position::Home { depth: c[2] },
            5 => Position::Captured {
                captor: to_side(c[2]),
            },
            _ => Position::Reserve,
        };
        state.push_checker(Checker { owner, pos });
    }
    state.teams = raw.t;
    state
}

#[wasm_bindgen]
pub fn compute(json: &str) -> String {
    console_error_panic_hook::set_once();
    let req: Request = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(_) => {
            return serde_json::to_string(&Response {
                id: 0,
                idx: None,
            })
            .unwrap();
        }
    };
    let state = reconstruct_state(req.state);
    let algo = Algo::from_u8(req.algo);
    let model = ai_model_for(algo, state.active.len(), state.teams);
    let result = match req.mode.as_str() {
        "turn" => {
            let rem = req.rem.unwrap_or_else(|| req.roll.to_vec());
            let turns = legal_turns_remaining(&state, &rem);
            if turns.iter().all(Vec::is_empty) {
                None
            } else {
                let idx = if algo == Algo::Search {
                    best_turn_2ply::<Ai>(&model, DEFAULT_WIDTH, &state, &turns)
                } else {
                    best_turn::<Ai>(&model, &state, &turns)
                };
                Some(idx.min(turns.len().saturating_sub(1)))
            }
        }
        "forced" => {
            let captor = req.captor.and_then(|v| {
                let s = to_side(v);
                if state.active.contains(&s) {
                    Some(s)
                } else {
                    None
                }
            });
            match captor {
                Some(captor) => {
                    let moves = sheshbesh::moves::forced_six_moves(&state, captor);
                    if moves.is_empty() {
                        None
                    } else {
                        let idx = best_forced::<Ai>(&model, &state, captor, &moves);
                        Some(idx.min(moves.len().saturating_sub(1)))
                    }
                }
                None => None,
            }
        }
        _ => None,
    };
    serde_json::to_string(&Response {
        id: req.id,
        idx: result,
    })
    .unwrap()
}
