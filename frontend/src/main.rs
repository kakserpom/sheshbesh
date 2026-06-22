//! Leptos-фронтенд Шеш-Беш: вся игровая логика — движок `sheshbesh` в WASM.
//! Доска — векторная (SVG); фишки рисуются отдельным слоем keyed-кружков, что даёт
//! плавную анимацию перемещения (CSS-переход `cx`/`cy`). Перед партией — экран
//! настроек (2/3/4 игрока, тип каждой стороны: человек/компьютер). Ход человека
//! собирается кликами (фишка → клетка) по одной кости; компьютерные стороны ходят
//! сами (эвристика).

mod app;
mod demo;
mod geom;
mod lessons;
mod model;
mod moves_ui;
mod util;
mod view;

use app::App;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
