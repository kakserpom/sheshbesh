importScripts('./worker/sheshbesh_ai_worker.js');

let ready = false;
let pending = [];

wasm_bindgen({ module_or_path: './worker/sheshbesh_ai_worker_bg.wasm' }).then(function () {
    ready = true;
    for (var i = 0; i < pending.length; i++) {
        postMessage(wasm_bindgen.compute(pending[i]));
    }
    pending = [];
});

onmessage = function (e) {
    if (ready) {
        postMessage(wasm_bindgen.compute(e.data));
    } else {
        pending.push(e.data);
    }
};
