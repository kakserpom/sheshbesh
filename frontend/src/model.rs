use sheshbesh::MlpValue;

// Веса обученных TD(λ)-ценностей-MLP (LE f32) под каждый режим — компьютер ходит ими.
// См. `examples/export_model.rs`.
pub(crate) static MODEL_2P: &[u8] = include_bytes!("model.bin");
pub(crate) static MODEL_3P: &[u8] = include_bytes!("model_3p.bin");
pub(crate) static MODEL_4P: &[u8] = include_bytes!("model_4p.bin");
pub(crate) static MODEL_4P_TEAMS: &[u8] = include_bytes!("model_4p_teams.bin");

/// MLP-ценность из встроенных весов под число сторон и режим (команды 2×2). Число
/// скрытых нейронов выводится из длины весов (`MlpValue::from_floats`).
pub(crate) fn ai_model_for(active_len: usize, teams: bool) -> MlpValue {
    let bytes = match (active_len, teams) {
        (4, true) => MODEL_4P_TEAMS,
        (4, false) => MODEL_4P,
        (3, _) => MODEL_3P,
        _ => MODEL_2P,
    };
    let floats: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    MlpValue::from_floats(&floats)
}
