use serde::{Deserialize, Serialize};
use sheshbesh::{DiceRoll, GameState, Move};

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
    #[serde(rename = "chat")]
    Chat { text: String },
    #[serde(rename = "reconnect")]
    Reconnect { sid: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Serialize, Deserialize)]
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
    #[serde(rename = "opponent_rolled")]
    OpponentRolled { side: String, roll: DiceRoll },
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
    #[serde(rename = "reconnected")]
    Reconnected {
        state: GameState,
        side: String,
        nicknames: Vec<(String, String)>,
        lobby_code: String,
        roll: Option<DiceRoll>,
        to_move: String,
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
