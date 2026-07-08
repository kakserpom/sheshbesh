use js_sys::JSON;
use leptos::prelude::*;
use sheshbesh::{GameState, MoonField, Position};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, Worker};

thread_local! {
    static PENDING: OnceCell<RefCell<HashMap<u64, RwSignal<Option<Option<usize>>>>>> = const { OnceCell::new() };
    static NEXT_ID: OnceCell<Cell<u64>> = const { OnceCell::new() };
    static WORKER: OnceCell<Worker> = const { OnceCell::new() };
}

fn with_pending<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<u64, RwSignal<Option<Option<usize>>>>) -> R,
{
    PENDING.with(|p| {
        let cell = p.get_or_init(move || RefCell::new(HashMap::new()));
        let mut map = cell.borrow_mut();
        f(&mut map)
    })
}

pub fn init_worker() {
    let worker = Worker::new("worker.js").expect("failed to create AI worker");
    let cb = Closure::<dyn Fn(MessageEvent)>::new(move |e: MessageEvent| {
        let text = e.data().as_string().unwrap_or_default();
        if let Ok(val) = JSON::parse(&text) {
            use js_sys::Reflect;
            if let Ok(id_val) = Reflect::get(&val, &"id".into()) {
                let id = id_val.as_f64().unwrap_or(0.0) as u64;
                with_pending(|map| {
                    if let Some(signal) = map.remove(&id) {
                        let idx = Reflect::get(&val, &"idx".into())
                            .ok()
                            .and_then(|v| v.as_f64().map(|f| f as usize));
                        signal.set(Some(idx));
                    }
                });
            }
        }
    });
    worker.set_onmessage(Some(cb.as_ref().unchecked_ref()));
    cb.forget();
    WORKER.with(|w| {
        w.set(worker).ok();
    });
}

pub fn next_id() -> u64 {
    NEXT_ID.with(|n| {
        let cell = n.get_or_init(move || Cell::new(1));
        let id = cell.get();
        cell.set(id + 1);
        id
    })
}

/// Sends a full JSON message to the AI worker.
/// The message must include `"id"` field.
/// Returns a signal that resolves with the response.
pub fn send_raw(msg: &str, id: u64) -> RwSignal<Option<Option<usize>>> {
    let signal = RwSignal::new(None::<Option<usize>>);
    with_pending(|map| {
        map.insert(id, signal);
    });
    WORKER.with(|w| {
        let w = w.get().expect("AI worker not initialized — call init_worker first");
        w.post_message(&JsValue::from_str(msg))
            .expect("AI worker postMessage failed");
    });
    signal
}

/// Serialises GameState to the compact JSON format used by the AI worker.
///
/// Format: `{"a":[side,…],"m":side,"t":bool,"c":[[owner,kind,a,b],…]}`
/// kind: 0=Reserve, 1=OnTrack, 2=Moon, 3=Prison, 4=Home, 5=Captured
pub fn state_json(state: &GameState) -> String {
    let active: String = state
        .active
        .iter()
        .map(|s| s.index().to_string())
        .collect::<Vec<_>>()
        .join(",");
    let checkers: String = state
        .checkers()
        .iter()
        .map(|c| {
            let owner = c.owner.index();
            let (kind, a, b): (u8, u32, u8) = match c.pos {
                Position::Reserve => (0, 0, 0),
                Position::OnTrack { progress } => (1, progress as u32, 0),
                Position::Moon { side, field } => {
                    let f = match field {
                        MoonField::One => 0,
                        MoonField::Three => 1,
                        MoonField::Six => 2,
                    };
                    (2, side.index() as u32, f)
                }
                Position::Prison { cell } => (3, cell.get() as u32, 0),
                Position::Home { depth } => (4, depth as u32, 0),
                Position::Captured { captor } => (5, captor.index() as u32, 0),
            };
            format!("[{owner},{kind},{a},{b}]")
        })
        .collect::<Vec<_>>()
        .join(",");
    let teams = if state.teams { "true" } else { "false" };
    format!(
        r#"{{"a":[{}],"m":{},"t":{},"c":[{}]}}"#,
        active,
        state.to_move.index(),
        teams,
        checkers
    )
}
