//! Обучает линейную ценность и сохраняет веса (little-endian `f32`) для встраивания
//! в GUI. Запуск: `cargo run --release --example export_model [путь]`
use sheshbesh::{TdConfig, train};
use std::io::Write;

fn main() {
    let model = train(&TdConfig::default());
    let floats = model.to_floats();
    let mut bytes = Vec::with_capacity(floats.len() * 4);
    for f in &floats {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "frontend/src/model.bin".to_string());
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&bytes))
        .expect("write model.bin");
    eprintln!(
        "wrote {} floats ({} bytes) -> {path}",
        floats.len(),
        bytes.len()
    );
}
