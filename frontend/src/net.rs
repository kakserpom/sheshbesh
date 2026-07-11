use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use sheshbesh::{DiceRoll, GameState, Move};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub enum NetState {
    Disconnected,
    Connecting,
    Connected { lobby_code: String, side: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    #[serde(rename = "create_lobby")]
    CreateLobby {
        players: u8,
        network_count: u8,
        nickname: String,
    },
    #[serde(rename = "join_lobby")]
    JoinLobby { code: String, nickname: String },
    #[serde(rename = "play_turn")]
    PlayTurn { moves: Vec<Move> },
    #[serde(rename = "pick_forced")]
    PickForced { idx: usize },
    #[serde(rename = "reconnect")]
    Reconnect { sid: String },
    #[serde(rename = "chat")]
    Chat { text: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum ServerMsg {
    #[serde(rename = "lobby_created")]
    LobbyCreated { code: String, sid: String },
    #[serde(rename = "player_joined")]
    PlayerJoined { side: String, nickname: String },
    #[serde(rename = "joined")]
    Joined { sid: String },
    #[serde(rename = "game_start")]
    GameStart {
        side: String,
        state: GameState,
        nicknames: Vec<(String, String)>,
        sid: String,
    },
    #[serde(rename = "reconnected")]
    Reconnected {
        state: GameState,
        side: String,
        nicknames: Vec<(String, String)>,
        lobby_code: String,
        roll: Option<DiceRoll>,
        to_move: String,
    },
    #[serde(rename = "your_turn")]
    YourTurn { roll: DiceRoll, state: GameState },
    #[serde(rename = "wait_turn")]
    WaitTurn,
    #[serde(rename = "moves_applied")]
    MovesApplied {
        side: String,
        applied: Vec<Move>,
        state: GameState,
        roll: DiceRoll,
        again: bool,
    },
    #[serde(rename = "forced_pick")]
    ForcedPick {
        captor: String,
        options: Vec<Move>,
        from_state: GameState,
        roll: DiceRoll,
        remaining: Vec<u8>,
    },
    #[serde(rename = "chat")]
    ChatMsg {
        side: String,
        nickname: String,
        text: String,
    },
    #[serde(rename = "game_over")]
    GameOver {
        winners: Vec<String>,
        result_msg: String,
    },
    #[serde(rename = "disconnected")]
    Disconnected { side: String },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "error")]
    Error { text: String },
}

/// WebSocket client stored in StoredValue. Pass signals from app.rs
/// for reactive integration: the closures capture RwSignals directly.
pub struct Net {
    pub state: RwSignal<NetState>,
    pub my_side: RwSignal<Option<String>>,
    pub chat_msgs: RwSignal<Vec<(String, String, String)>>,
    pub incoming_msgs: RwSignal<Vec<ServerMsg>>,
    pub error: RwSignal<Option<String>>,
    ws: StoredValue<Option<web_sys::WebSocket>>,
}

impl Net {
    pub fn new(
        state: RwSignal<NetState>,
        my_side: RwSignal<Option<String>>,
        chat_msgs: RwSignal<Vec<(String, String, String)>>,
        incoming_msgs: RwSignal<Vec<ServerMsg>>,
        error: RwSignal<Option<String>>,
    ) -> Self {
        Net {
            state,
            my_side,
            chat_msgs,
            incoming_msgs,
            error,
            ws: StoredValue::new(None),
        }
    }

    pub fn connect(&self, url: &str, msg: Option<&ClientMsg>) {
        if matches!(self.state.get_untracked(), NetState::Connected { .. }) {
            return;
        }
        self.state.set(NetState::Connecting);
        self.error.set(None);

        let ws = web_sys::WebSocket::new(url).unwrap();
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let ws_clone = ws.clone(); // for onopen closure
        let first_json = msg.map(|m| serde_json::to_string(&m).unwrap());

        let state = self.state;
        let error = self.error;
        let incoming_msgs = self.incoming_msgs;
        let my_side = self.my_side;
        let chat_msgs = self.chat_msgs;

        let onopen = Closure::<dyn Fn()>::new(move || {
            state.set(NetState::Connected {
                lobby_code: String::new(),
                side: String::new(),
            });
            if let Some(ref json) = first_json {
                let _ = ws_clone.send_with_str(json);
            }
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        onopen.forget();

        let onclose = Closure::<dyn Fn()>::new(move || {
            state.set(NetState::Disconnected);
        });
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();

        let onerror = Closure::<dyn Fn()>::new(move || {
            error.set(Some("WebSocket error".into()));
            state.set(NetState::Disconnected);
        });
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();

        let onmessage =
            Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| {
                if let Ok(text) = ev.data().dyn_into::<js_sys::JsString>() {
                    let text: String = text.into();
                    if let Ok(smsg) = serde_json::from_str::<ServerMsg>(&text) {
                        let set_url_sid = |sid: &str| {
                            if let Some(w) = web_sys::window() {
                                let _ = w.location().set_hash(&format!("sid={sid}"));
                            }
                        };
                        match &smsg {
                            ServerMsg::LobbyCreated { code, sid } => {
                                set_url_sid(sid);
                                let old = state.get_untracked();
                                if let NetState::Connected { .. } = old {
                                    state.set(NetState::Connected {
                                        lobby_code: code.clone(),
                                        side: "A".into(),
                                    });
                                    my_side.set(Some("A".into()));
                                }
                            }
                            ServerMsg::Joined { sid } => {
                                set_url_sid(sid);
                            }
                            ServerMsg::PlayerJoined { side, nickname } => {
                                chat_msgs.update(|msgs| {
                                    msgs.push((
                                        "".into(),
                                        "".into(),
                                        format!("Player {} ({}) joined", side, nickname),
                                    ));
                                });
                            }
                            ServerMsg::ChatMsg {
                                side,
                                nickname,
                                text,
                            } => {
                                chat_msgs.update(|msgs| {
                                    msgs.push((side.clone(), nickname.clone(), text.clone()));
                                });
                            }
                            ServerMsg::GameStart { side, sid, .. } => {
                                set_url_sid(sid);
                                my_side.set(Some(side.clone()));
                                if let NetState::Connected { lobby_code, .. } =
                                    state.get_untracked()
                                {
                                    state.set(NetState::Connected {
                                        lobby_code,
                                        side: side.clone(),
                                    });
                                }
                            }
                            ServerMsg::Reconnected {
                                state: _,
                                side,
                                nicknames: _,
                                lobby_code,
                                roll: _,
                                to_move: _,
                            } => {
                                my_side.set(Some(side.clone()));
                                state.set(NetState::Connected {
                                    lobby_code: lobby_code.clone(),
                                    side: side.clone(),
                                });
                            }
                            _ => {}
                        }
                        incoming_msgs.update(|v| v.push(smsg));
                    }
                }
            });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();

        self.ws.set_value(Some(ws));
    }

    pub fn send(&self, msg: &ClientMsg) {
        let ws_opt = self.ws.with_value(|w| w.clone());
        if let Some(ref ws) = ws_opt {
            let json = serde_json::to_string(msg).unwrap();
            let _ = ws.send_with_str(&json);
        }
    }

    pub fn disconnect(&self) {
        let ws_opt = self.ws.with_value(|w| w.clone());
        if let Some(ref ws) = ws_opt {
            let _ = ws.close();
        }
        self.ws.set_value(None);
        self.state.set(NetState::Disconnected);
        // Clear sid from URL
        let _ = web_sys::window().map(|w| w.location().set_hash(""));
    }
}
