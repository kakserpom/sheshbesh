mod persist;
mod protocol;

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use persist::dead_tx;
use protocol::{ClientMsg, ServerMsg};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sheshbesh::turn::Game;
use sheshbesh::{
    apply, forced_six_moves, legal_turns_remaining, move_legal, next_unfinished_active,
    remaining_after, DiceRoll, DiceSource, Die, Move, MoveKind, RandomDice, Side,
};
use tokio::sync::mpsc;

type LobbyCode = String;

#[derive(Clone)]
struct Player {
    side: Side,
    nickname: String,
    sid: String,
    txs: Vec<mpsc::UnboundedSender<String>>,
}

struct GameSession {
    game: Game,
    dice: RandomDice,
    current_roll: DiceRoll,
    finished: Vec<Side>,
    turn_nonce: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct PendingForced {
    captor: Side,
    options: Vec<Move>,
    roll: DiceRoll,
    remaining_pips: Vec<u8>,
    original_side: Side,
}

struct Lobby {
    players: Vec<Player>,
    max_players: usize,
    total_sides: usize,
    network_count: usize,
    creator_sid: String,
    session: Option<GameSession>,
    pending_forced: Option<PendingForced>,
    creator_is_computer: bool,
}

fn is_computer_side(side: Side, total: usize, network_count: usize, creator_is_computer: bool) -> bool {
    if creator_is_computer && side == Side::A {
        return true;
    }
    active_idx(side, total) >= 1 + network_count
}

fn validate_nickname(nick: &str) -> Result<(), &'static str> {
    let trimmed = nick.trim();
    if trimmed.is_empty() {
        return Err("Nickname cannot be empty");
    }
    if trimmed.len() > 20 {
        return Err("Nickname too long (max 20 chars)");
    }
    if trimmed.chars().any(|c| c.is_ascii_control()) {
        return Err("Nickname contains invalid characters");
    }
    Ok(())
}

fn is_nickname_used(players: &[Player], nick: &str) -> bool {
    let lower = nick.trim().to_lowercase();
    players.iter().any(|p| p.nickname.trim().to_lowercase() == lower)
}

struct AppState {
    lobbies: Mutex<HashMap<LobbyCode, Lobby>>,
    sid_index: Mutex<HashMap<String, (LobbyCode, Side)>>,
    redis: tokio::sync::Mutex<Option<persist::RedisStore>>,
}

use tokio::sync::Mutex;

fn make_code() -> LobbyCode {
    let mut rng = rand::thread_rng();
    (0..6).map(|_| rng.gen_range(b'A'..=b'Z') as char).collect()
}

fn make_sid() -> String {
    let mut rng = rand::thread_rng();
    format!("{:08x}", rng.r#gen::<u64>())
}

async fn send(tx: &mpsc::UnboundedSender<String>, msg: &ServerMsg) {
    let json = serde_json::to_string(msg).unwrap();
    let _ = tx.send(json);
}

fn broadcast(players: &[Player], msg: &ServerMsg) {
    let json = serde_json::to_string(msg).unwrap();
    for p in players {
        for tx in &p.txs {
            let _ = tx.send(json.clone());
        }
    }
}

fn lobby_to_persisted(code: &str, lobby: &Lobby) -> persist::PersistedLobby {
    let (game_state, current_roll, finished) = match &lobby.session {
        Some(s) => (Some(s.game.state.clone()), Some(s.current_roll), Some(s.finished.clone())),
        None => (None, None, None),
    };
    let (forced_captor, forced_options, forced_roll, forced_remaining, forced_original_side) =
        match &lobby.pending_forced {
            Some(pf) => (
                Some(pf.captor),
                Some(pf.options.clone()),
                Some(pf.roll),
                Some(pf.remaining_pips.clone()),
                Some(pf.original_side),
            ),
            None => (None, None, None, None, None),
        };
    persist::PersistedLobby {
        code: code.to_string(),
        players: lobby.players.iter().map(|p| (p.side, p.nickname.clone(), p.sid.clone())).collect(),
        max_players: lobby.max_players,
        total_sides: lobby.total_sides,
        network_count: lobby.network_count,
        creator_sid: lobby.creator_sid.clone(),
        creator_is_computer: lobby.creator_is_computer,
        game_state,
        current_roll,
        forced_captor,
        forced_options,
        forced_roll,
        forced_remaining,
        forced_original_side,
        finished: finished.unwrap_or_default(),
    }
}

/// Save a pre-built PersistedLobby to Redis (only acquires redis lock).
async fn persist_pl(pl: &persist::PersistedLobby, state: &AppState) {
    let mut redis = state.redis.lock().await;
    if let Some(ref mut store) = *redis {
        store.save_lobby(pl).await;
    }
}

fn server_game_over(state: &sheshbesh::GameState) -> bool {
    if state.teams && state.active.len() == 4 {
        (0..2).any(|t| {
            let members: Vec<Side> = state.active.iter().copied()
                .filter(|s| s.index() % 2 == t).collect();
            !members.is_empty() && members.iter().all(|&s| state.has_won(s))
        })
    } else {
        let done = state.active.iter().filter(|&&s| state.has_won(s)).count();
        done >= state.active.len().saturating_sub(1)
    }
}

fn side_from_index(i: usize, total: usize) -> Side {
    match total {
        2 => [Side::A, Side::A.opposite()][i],
        3 => [Side::A, Side::B, Side::C][i],
        _ => Side::ALL[i],
    }
}

fn active_idx(side: Side, total: usize) -> usize {
    (0..total)
        .find(|&i| side_from_index(i, total) == side)
        .expect("side not in active set")
}

#[tokio::main]
async fn main() {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let mut lobbies = HashMap::new();
    let mut sid_index = HashMap::new();
    let redis_store = match persist::RedisStore::connect(&redis_url).await {
        Ok(mut store) => {
            println!("persistence: connected to {redis_url}");
            for pl in store.load_all_lobbies().await {
                let code = pl.code.clone();
                // Reconstruct lobby with placeholder channels (players reconnect later)
                let player_tx = dead_tx();
                let session = pl.game_state.map(|gs| GameSession {
                    game: Game::new(gs),
                    dice: RandomDice::from_entropy(),
                    current_roll: pl.current_roll.unwrap_or(DiceRoll::new(Die::new(1).unwrap(), Die::new(1).unwrap())),
                    finished: pl.finished.clone(),
                    turn_nonce: 0,
                });
                let pending_forced = pl.forced_captor.map(|captor| PendingForced {
                    captor,
                    options: pl.forced_options.clone().unwrap_or_default(),
                    roll: pl.forced_roll.unwrap_or(DiceRoll::new(Die::new(1).unwrap(), Die::new(1).unwrap())),
                    remaining_pips: pl.forced_remaining.clone().unwrap_or_default(),
                    original_side: pl.forced_original_side.unwrap_or(Side::A),
                });
                let lobby = Lobby {
                    players: pl.players.iter().map(|(side, nick, sid)| Player {
                        side: *side,
                        nickname: nick.clone(),
                        sid: sid.clone(),
                        txs: vec![player_tx.clone()],
                    }).collect(),
                    max_players: pl.max_players,
                    total_sides: pl.total_sides,
                    network_count: pl.network_count,
                    creator_is_computer: pl.creator_is_computer,
                    creator_sid: pl.creator_sid.clone(),
                    session,
                    pending_forced,
                };
                for (side, _, sid) in &pl.players {
                    sid_index.insert(sid.clone(), (code.clone(), *side));
                }
                lobbies.insert(code, lobby);
            }
            println!("persistence: loaded {} lobby(s)", lobbies.len());
            Some(store)
        }
        Err(e) => {
            eprintln!("persistence: cannot connect to {redis_url}: {e} (no persistence)");
            None
        }
    };

    let state = Arc::new(AppState {
        lobbies: Mutex::new(lobbies),
        sid_index: Mutex::new(sid_index),
        redis: tokio::sync::Mutex::new(redis_store),
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "./dist".into());

    let serve_dir = tower_http::services::ServeDir::new(&static_dir)
        .fallback(tower_http::services::ServeFile::new(format!("{static_dir}/index.html")));

    let app = Router::new()
        .route("/ws", get(ws_upgrade))
        .fallback_service(serve_dir)
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    eprintln!("[MAIN] server starting on {addr}");
    axum::serve(listener, app).await.unwrap();
}

/// Send YourTurn/WaitTurn for a given side to the correct player (remote or creator).
async fn send_turn(
    players: &[Player],
    nonce: u64,
    network_count: usize,
    creator_is_computer: bool,
    state: &sheshbesh::GameState,
    side: Side,
    roll: sheshbesh::DiceRoll,
) {
    let side_str = side.letter().to_string();
    // Computer side → send to all creator connections (creator = first player, side A)
    if is_computer_side(side, state.active.len(), network_count, creator_is_computer) {
        if let Some(creator) = players.iter().find(|p| p.side == Side::A) {
            let your_turn = ServerMsg::YourTurn { roll, state: state.clone(), nonce };
            let json = serde_json::to_string(&your_turn).unwrap();
            for tx in &creator.txs {
                let _ = tx.send(json.clone());
            }
        }
        for p in players {
            if p.side != side {
                let opp = ServerMsg::OpponentRolled { side: side_str.clone(), roll };
                let json = serde_json::to_string(&opp).unwrap();
                for tx in &p.txs { let _ = tx.send(json.clone()); }
                let wait = ServerMsg::WaitTurn;
                let json = serde_json::to_string(&wait).unwrap();
                for tx in &p.txs { let _ = tx.send(json.clone()); }
            }
        }
    } else {
        // Remote human or creator's own side
        for p in players {
            if p.side == side {
                let your_turn = ServerMsg::YourTurn { roll, state: state.clone(), nonce };
                let json = serde_json::to_string(&your_turn).unwrap();
                for tx in &p.txs { let _ = tx.send(json.clone()); }
            } else {
                let opp = ServerMsg::OpponentRolled { side: side_str.clone(), roll };
                let json = serde_json::to_string(&opp).unwrap();
                for tx in &p.txs { let _ = tx.send(json.clone()); }
                let wait = ServerMsg::WaitTurn;
                let json = serde_json::to_string(&wait).unwrap();
                for tx in &p.txs { let _ = tx.send(json.clone()); }
            }
        }
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(ws: WebSocket, state: Arc<AppState>) {
    let (mut ws_sender, mut ws_receiver) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let mut lobby_code: Option<LobbyCode> = None;
    let mut my_side: Option<Side> = None;
    let mut is_creator = false;

    // --- first message: create or join lobby ---
    if let Some(Ok(Message::Text(text))) = ws_receiver.next().await {
        if let Ok(cmsg) = serde_json::from_str::<ClientMsg>(&text) {
            match cmsg {
                ClientMsg::CreateLobby {
                    players,
                    network_count,
                    nickname,
                    creator_is_computer,
                } => {
                    if let Err(e) = validate_nickname(&nickname) {
                        send(&tx, &ServerMsg::Error { text: e.into() }).await;
                        return;
                    }
                    if is_nickname_used(&[], &nickname) {
                        // no existing players in CreateLobby
                    }
                    let code = make_code();
                    let sid = make_sid();
                    let side = Side::A;
                    my_side = Some(side);
                    is_creator = true;
                    let total = players.max(2).min(4) as usize;
                    let net_cnt = (network_count as usize).min(total.saturating_sub(1));
                    let lobby = Lobby {
                        players: vec![Player {
                            side,
                            nickname,
                            sid: sid.clone(),
                            txs: vec![tx.clone()],
                        }],
                        max_players: 1 + net_cnt,
                        total_sides: total,
                        network_count: network_count as usize,
                        creator_sid: sid.clone(),
                        session: None,
                        pending_forced: None,
                        creator_is_computer,
                    };
                    let pl = lobby_to_persisted(&code, &lobby);
                    {
                        let mut lobbies = state.lobbies.lock().await;
                        lobbies.insert(code.clone(), lobby);
                    }
                    {
                        let mut idx = state.sid_index.lock().await;
                        idx.insert(sid.clone(), (code.clone(), side));
                    }
                    send(
                        &tx,
                        &ServerMsg::LobbyCreated {
                            code: code.clone(),
                            sid,
                        },
                    )
                    .await;
                    lobby_code = Some(code.clone());
                    persist_pl(&pl, &state).await;
                    // Если лобби сразу заполнено (network_count == 0), стартуем игру
                    let mut lobbies = state.lobbies.lock().await;
                    if let Some(lobby) = lobbies.get_mut(&code) {
                        if lobby.players.len() == lobby.max_players {
                            let total = lobby.total_sides;
                            let active: Vec<Side> =
                                (0..total).map(|i| side_from_index(i, total)).collect();
                            let mut dice = RandomDice::from_entropy();
                            let game = Game::start(active, &mut dice);
                            let first_roll = dice.roll();
                            let st = game.state.clone();
                            let session = GameSession {
                                game,
                                dice,
                                current_roll: first_roll,
                                finished: Vec::new(),
                                turn_nonce: 1,
                            };
                            let nicknames: Vec<(String, String)> = lobby
                                .players
                                .iter()
                                .filter(|p| !(p.side == Side::A && lobby.creator_is_computer))
                                .map(|p| (p.side.letter().to_string(), p.nickname.clone()))
                                .collect();
                            for p in &lobby.players {
                                let sid = state.sid_index.lock().await
                                    .iter()
                                    .find(|(_, (lc, sd))| lc == &code && *sd == p.side)
                                    .map(|(sid, _)| sid.clone())
                                    .unwrap_or_default();
                                let gs = ServerMsg::GameStart {
                                    side: p.side.letter().to_string(),
                                    state: st.clone(),
                                    nicknames: nicknames.clone(),
                                    sid,
                                };
                                let json = serde_json::to_string(&gs).unwrap();
                                for tx in &p.txs { let _ = tx.send(json.clone()); }
                            }
                            for comp_idx in (1 + lobby.network_count)..lobby.total_sides {
                                let comp_side = side_from_index(comp_idx, lobby.total_sides);
                                if let Some(creator) = lobby.players.iter().find(|p| p.side == Side::A) {
                                    let gs = ServerMsg::GameStart {
                                        side: comp_side.letter().to_string(),
                                        state: st.clone(),
                                        nicknames: nicknames.clone(),
                                        sid: String::new(),
                                    };
                                    let json = serde_json::to_string(&gs).unwrap();
                                    for tx in &creator.txs { let _ = tx.send(json.clone()); }
                                }
                            }
                            let next_side = session.game.state.to_move;
                            let new_roll = session.current_roll;
                            let nonce = session.turn_nonce;
                            lobby.session = Some(session);
                            let players = lobby.players.clone();
                            let net_cnt = lobby.network_count;
                            let cc = lobby.creator_is_computer;
                            let pl = lobby_to_persisted(&code, lobby);
                            let _ = lobby;
                            drop(lobbies);
                            persist_pl(&pl, &state).await;
                            send_turn(
                                &players, nonce, net_cnt, cc, &st, next_side, new_roll,
                            )
                            .await;
                        }
                    }
                }
                ClientMsg::JoinLobby { code, nickname } => {
                    if let Err(e) = validate_nickname(&nickname) {
                        send(&tx, &ServerMsg::Error { text: e.into() }).await;
                    } else if let Some(lobby) = state.lobbies.lock().await.get_mut(&code) {
                        if is_nickname_used(&lobby.players, &nickname) {
                            send(&tx, &ServerMsg::Error { text: "Nickname already taken".into() }).await;
                        } else if lobby.players.len() >= lobby.max_players {
                            send(
                                &tx,
                                &ServerMsg::Error {
                                    text: "Lobby is full".into(),
                                },
                            )
                            .await;
                        } else {
                            let sid = make_sid();
                            let side = side_from_index(lobby.players.len(), lobby.total_sides);
                            my_side = Some(side);
                            broadcast(
                                &lobby.players,
                                &ServerMsg::PlayerJoined {
                                    side: side.letter().to_string(),
                                    nickname: nickname.clone(),
                                },
                            );
                            lobby.players.push(Player {
                                side,
                                nickname: nickname.clone(),
                                sid: sid.clone(),
                                txs: vec![tx.clone()],
                            });
                            {
                                let mut idx = state.sid_index.lock().await;
                                idx.insert(sid.clone(), (code.clone(), side));
                            }
                            send(&tx, &ServerMsg::Joined { sid: sid.clone() }).await;
                            lobby_code = Some(code.clone());

                            // All joined → start game
                            if lobby.players.len() == lobby.max_players {
                                let total = lobby.total_sides;
                                let active: Vec<Side> =
                                    (0..total).map(|i| side_from_index(i, total)).collect();
                                let mut dice = RandomDice::from_entropy();
                                let game = Game::start(active, &mut dice);
                                let first_roll = dice.roll();
                                let st = game.state.clone();
                                let session = GameSession {
                                    game,
                                    dice,
                                    current_roll: first_roll,
                                    finished: Vec::new(),
                                    turn_nonce: 1,
                                };
                                let nicknames: Vec<(String, String)> = lobby
                                    .players
                                    .iter()
                                    .filter(|p| !(p.side == Side::A && lobby.creator_is_computer))
                                    .map(|p| (p.side.letter().to_string(), p.nickname.clone()))
                                    .collect();
                                // Build sid lookup for this lobby's players
                                let sid_idx = state.sid_index.lock().await;
                                let player_sid = |s: Side| -> String {
                                    sid_idx
                                        .iter()
                                        .find(|(_, (lc, sd))| lc == &code && *sd == s)
                                        .map(|(sid, _)| sid.clone())
                                        .unwrap_or_default()
                                };
                                // Send GameStart to each remote player
                                for p in &lobby.players {
                                    let gs = ServerMsg::GameStart {
                                        side: p.side.letter().to_string(),
                                        state: st.clone(),
                                        nicknames: nicknames.clone(),
                                        sid: player_sid(p.side),
                                    };
                                    let json = serde_json::to_string(&gs).unwrap();
                                    for tx in &p.txs { let _ = tx.send(json.clone()); }
                                }
                                drop(sid_idx);
                                // Send GameStart to creator for computer-controlled sides too
                                if let Some(creator) = lobby.players.iter().find(|p| p.side == Side::A) {
                                    for comp_idx in (1 + lobby.network_count)..lobby.total_sides {
                                        let comp_side = side_from_index(comp_idx, lobby.total_sides);
                                        let gs = ServerMsg::GameStart {
                                            side: comp_side.letter().to_string(),
                                            state: st.clone(),
                                            nicknames: nicknames.clone(),
                                            sid: String::new(),
                                        };
                                        let json = serde_json::to_string(&gs).unwrap();
                                        for tx in &creator.txs { let _ = tx.send(json.clone()); }
                                    }
                                }
                                let next_side = session.game.state.to_move;
                                let new_roll = session.current_roll;
                                let nonce = session.turn_nonce;
                                lobby.session = Some(session);
                                let players = lobby.players.clone();
                                let net_cnt = lobby.network_count;
                                let cc = lobby.creator_is_computer;
                                let pl = lobby_to_persisted(&code, lobby);
                                let _ = lobby;
                                persist_pl(&pl, &state).await;
                                send_turn(&players, nonce, net_cnt, cc, &st, next_side, new_roll)
                                    .await;
                            }
                        }
                    } else {
                        send(
                            &tx,
                            &ServerMsg::Error {
                                text: "Lobby not found".into(),
                            },
                        )
                        .await;
                    }
                }
                ClientMsg::Reconnect { sid } => {
                    let sid_idx = state.sid_index.lock().await;
                    if let Some((ref code, side)) = sid_idx.get(sid.as_str()).cloned() {
                        drop(sid_idx);
                        let mut lobbies = state.lobbies.lock().await;
                        if let Some(lobby) = lobbies.get_mut(code) {
                            let nickname = lobby
                                .players
                                .iter()
                                .find(|p| p.side == side)
                                .map(|p| p.nickname.clone())
                                .unwrap_or_default();
                            // Add new tx to player's list (keep existing connections alive)
                            if let Some(player) =
                                lobby.players.iter_mut().find(|p| p.side == side)
                            {
                                player.txs.push(tx.clone());
                            }
                            is_creator = lobby.creator_sid == sid;
                            my_side = Some(side);
                            lobby_code = Some(code.to_string());
                            let nicknames: Vec<(String, String)> = lobby
                                .players
                                .iter()
                                .filter(|p| !(p.side == Side::A && lobby.creator_is_computer))
                                .map(|p| {
                                    (p.side.letter().to_string(), p.nickname.clone())
                                })
                                .collect();
                            if let Some(ref session) = lobby.session {
                                let to_move = session.game.state.to_move;
                                let rec = ServerMsg::Reconnected {
                                    state: session.game.state.clone(),
                                    side: side.letter().to_string(),
                                    nicknames,
                                    lobby_code: code.to_string(),
                                    roll: Some(session.current_roll),
                                    to_move: to_move.letter().to_string(),
                                    creator_is_computer: lobby.creator_is_computer,
                                    nonce: session.turn_nonce,
                                };
                                send(&tx, &rec).await;
                            } else {
                                let rec = ServerMsg::Reconnected {
                                    state: sheshbesh::GameState::new(vec![side], side),
                                    side: side.letter().to_string(),
                                    nicknames,
                                    lobby_code: code.to_string(),
                                    roll: None,
                                    to_move: String::new(),
                                    creator_is_computer: lobby.creator_is_computer,
                                    nonce: 0,
                                };
                                send(&tx, &rec).await;
                            }
                            // Notify other players that this player reconnected
                            let joined = serde_json::to_string(
                                &ServerMsg::PlayerJoined {
                                    side: side.letter().to_string(),
                                    nickname,
                                },
                            )
                            .unwrap();
                            for p in &lobby.players {
                                if p.side != side {
                                    for t in &p.txs { let _ = t.send(joined.clone()); }
                                }
                            }
                        } else {
                            send(
                                &tx,
                                &ServerMsg::Error {
                                    text: "Lobby gone".into(),
                                },
                            )
                            .await;
                        }
                    } else {
                        send(
                            &tx,
                            &ServerMsg::Error {
                                text: "Session not found".into(),
                            },
                        )
                        .await;
                    }
                }
                _ => {}
            }
        }
    }

    // --- subsequent messages ---
    while let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(cmsg) = serde_json::from_str::<ClientMsg>(&text) {
                    match cmsg {
                        ClientMsg::Ping => {
                            send(&tx, &ServerMsg::Pong).await;
                        }
                        ClientMsg::Chat { text: chat_text } => {
                            if let (Some(code), Some(side)) = (&lobby_code, my_side) {
                                let lobbies = state.lobbies.lock().await;
                                if let Some(lobby) = lobbies.get(code) {
                                    let nick = lobby
                                        .players
                                        .iter()
                                        .find(|p| p.side == side)
                                        .map(|p| p.nickname.clone())
                                        .unwrap_or_default();
                                    broadcast(
                                        &lobby.players,
                                        &ServerMsg::ChatMsg {
                                            side: side.letter().to_string(),
                                            nickname: nick,
                                            text: chat_text.clone(),
                                        },
                                    );
                                }
                            }
                        }
                        ClientMsg::PlayTurn { moves, nonce } => {
                            if let (Some(code), Some(side)) = (&lobby_code, my_side) {
                                let mut lobbies = state.lobbies.lock().await;
                                if let Some(lobby) = lobbies.get_mut(code) {
                                    if let Some(ref mut session) = lobby.session {
                                        // Stale PlayTurn (another tab already processed this turn) → ignore silently
                                        if nonce != 0 && nonce != session.turn_nonce {
                                            continue;
                                        }
                                        // Allow: own turn, or creator playing a computer side
                                        let comp_side =
                                            is_computer_side(session.game.state.to_move, lobby.total_sides, lobby.network_count, lobby.creator_is_computer);
                                        if session.game.state.to_move != side && !(is_creator && comp_side) {
                                            send(
                                                &tx,
                                                &ServerMsg::Error {
                                                    text: "Not your turn".into(),
                                                },
                                            )
                                            .await;
                                            continue;
                                        }

                                        let roll = session.current_roll;
                                        let mut remaining_pips: Vec<u8> = roll.values().to_vec();
                                        let mut applied_local = Vec::new();

                                        // Process moves one by one; if a Ransom triggers a forced
                                        // reply for a network captor, defer and wait for PickForced.
                                        let playing_side = session.game.state.to_move;
                                        let mut deferred = false;

                                        for &mv in &moves {
                                            if !move_legal(&session.game.state, mv) {
                                                continue;
                                            }
                                            // Detect captor BEFORE applying (checker still has captor info).
                                            let captor = if mv.kind == MoveKind::Ransom {
                                                match session.game.state.checker(mv.checker).pos {
                                                    sheshbesh::Position::Captured { captor } => {
                                                        Some(captor)
                                                    }
                                                    _ => None,
                                                }
                                            } else {
                                                None
                                            };
                                            session.game.state =
                                                apply(&session.game.state, mv);
                                            applied_local.push(mv);
                                            remaining_pips =
                                                remaining_after(&remaining_pips, mv.pips);

                                            if let Some(captor) = captor {
                                                let options = forced_six_moves(
                                                    &session.game.state,
                                                    captor,
                                                );
                                                if options.is_empty() {
                                                    // No forced reply possible — skip
                                                } else if !is_computer_side(captor, lobby.total_sides, lobby.network_count, lobby.creator_is_computer)
                                                {
                                                    // Network captor must choose
                                                    let captor_s = captor.letter().to_string();
                                                    let msg = ServerMsg::ForcedPick {
                                                        captor: captor_s.clone(),
                                                        options: options.clone(),
                                                        from_state: session.game.state.clone(),
                                                        roll,
                                                        remaining: remaining_pips.clone(),
                                                    };
                                                    lobby.pending_forced = Some(PendingForced {
                                                        captor,
                                                        options,
                                                        roll,
                                                        remaining_pips: remaining_pips.clone(),
                                                        original_side: playing_side,
                                                    });
                                                    // Send ForcedPick to captor's txs
                                                    if let Some(captor_player) = lobby.players.iter().find(|p| p.side == captor) {
                                                        let json = serde_json::to_string(&msg).unwrap();
                                                        for t in &captor_player.txs { let _ = t.send(json.clone()); }
                                                    }
                                                    // Also send to creator if they control this side
                                                    if is_computer_side(captor, lobby.total_sides, lobby.network_count, lobby.creator_is_computer) {
                                                        if let Some(creator) = lobby.players.iter().find(|p| p.side == Side::A) {
                                                            let json = serde_json::to_string(&msg).unwrap();
                                                            for t in &creator.txs { let _ = t.send(json.clone()); }
                                                        }
                                                    }
                                                    // Other players get WaitTurn
                                                    for p in &lobby.players {
                                                        if p.side != captor {
                                                            let json = serde_json::to_string(&ServerMsg::WaitTurn).unwrap();
                                                            for t in &p.txs { let _ = t.send(json.clone()); }
                                                        }
                                                    }
                                                    // Broadcast ransom moves so far
                                                    broadcast(
                                                        &lobby.players,
                                                        &ServerMsg::MovesApplied {
                                                            side: playing_side.letter().to_string(),
                                                            applied: applied_local.clone(),
                                                            state: session.game.state.clone(),
                                                            roll,
                                                            again: false,
                                                        },
                                                    );
                                                    deferred = true;
                                                    break;
                                                } else {
                                                    // Computer captor: auto-apply forced reply
                                                    let idx = 0;
                                                    let fm = options[idx];
                                                    session.game.state =
                                                        apply(&session.game.state, fm);
                                                    applied_local.push(fm);
                                                }
                                            }
                                        }

                                        if deferred {
                                            let pl = lobby_to_persisted(code, lobby);
                                            let _ = lobby;
                                            let _ = lobbies;
                                            persist_pl(&pl, &state).await;
                                            continue;
                                        }

                                        // Track who has finished (in order) — before remaining-pips branch
                                        for &s in &session.game.state.active {
                                            if session.game.state.has_won(s)
                                                && !session.finished.contains(&s)
                                            {
                                                session.finished.push(s);
                                            }
                                        }
                                        // Check for remaining pips: send another YourTurn (same nonce)
                                        let state_snapshot = session.game.state.clone();
                                        if !remaining_pips.is_empty()
                                            && legal_turns_remaining(&state_snapshot, &remaining_pips)
                                                .iter()
                                                .any(|v| !v.is_empty())
                                        {
                                            // Same player has more legal moves
                                            broadcast(
                                                &lobby.players,
                                                &ServerMsg::MovesApplied {
                                                    side: playing_side.letter().to_string(),
                                                    applied: applied_local,
                                                    state: state_snapshot.clone(),
                                                    roll,
                                                    again: false,
                                                },
                                            );
                                            let players = lobby.players.clone();
                                            let net_cnt = lobby.network_count;
                                            let cc = lobby.creator_is_computer;
                                            let nn = session.turn_nonce;
                                            send_turn(
                                                &players,
                                                nn,
                                                net_cnt,
                                                cc,
                                                &state_snapshot,
                                                playing_side,
                                                roll,
                                            )
                                            .await;
                                            let pl = lobby_to_persisted(code, lobby);
                                            let _ = lobby;
                                            let _ = lobbies;
                                            persist_pl(&pl, &state).await;
                                            continue;
                                        }

                                        let again = roll.is_double()
                                            && !session.game.state.has_won(playing_side);

                                        // Broadcast applied moves
                                        broadcast(
                                            &lobby.players,
                                            &ServerMsg::MovesApplied {
                                                side: playing_side.letter().to_string(),
                                                applied: applied_local,
                                                state: state_snapshot.clone(),
                                                roll,
                                                again,
                                            },
                                        );
                                        // Check game over (FFA: until n-1 finish)
                                        if server_game_over(&session.game.state) {
                                            let win_strs: Vec<String> = if session.game.state.teams
                                                && session.game.state.active.len() == 4
                                            {
                                                session.finished.iter().map(|s| s.letter().to_string()).collect()
                                            } else {
                                                vec![session.finished.first().map(|s| s.letter().to_string()).unwrap_or_default()]
                                            };
                                            let result_msg = if win_strs.len() > 1 {
                                                format!("Team {} wins!", win_strs.join("+"))
                                            } else {
                                                format!("Player {} wins!", win_strs[0])
                                            };
                                            let fin_strs: Vec<String> = session.finished
                                                .iter()
                                                .map(|s| s.letter().to_string())
                                                .collect();
                                            let go = ServerMsg::GameOver {
                                                winners: win_strs,
                                                result_msg,
                                                state: session.game.state.clone(),
                                                finished: fin_strs,
                                            };
                                            broadcast(&lobby.players, &go);
                                            if let Some(creator) = lobby.players.iter().find(|p| p.side == Side::A) {
                                                let json = serde_json::to_string(&go).unwrap();
                                                for t in &creator.txs { let _ = t.send(json.clone()); }
                                            }
                                            let pl = lobby_to_persisted(code, lobby);
                                            let _ = lobby;
                                            let _ = lobbies;
                                            persist_pl(&pl, &state).await;
                                            continue;
                                        }

                                        // Next turn: advance to_move if not again
                                        if !again {
                                            session.game.state.to_move =
                                                next_unfinished_active(&session.game.state, playing_side);
                                        }
                                        session.current_roll = session.dice.roll();
                                        session.turn_nonce += 1;
                                        let next_side = session.game.state.to_move;
                                        let new_roll = session.current_roll;
                                        let turn_state = session.game.state.clone();
                                        let players = lobby.players.clone();
                                        let net_cnt = lobby.network_count;
                                        let cc = lobby.creator_is_computer;
                                        let nn = session.turn_nonce;
                                        let pl = lobby_to_persisted(code, lobby);
                                        let _ = lobby;
                                        let _ = lobbies;
                                        persist_pl(&pl, &state).await;
                                        send_turn(
                                            &players,
                                            nn,
                                            net_cnt,
                                            cc,
                                            &turn_state,
                                            next_side,
                                            new_roll,
                                        )
                                        .await;
                                        continue;
                                    } else {
                                        send(
                                            &tx,
                                            &ServerMsg::Error {
                                                text: "Game not started".into(),
                                            },
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        ClientMsg::PickForced { idx } => {
                            if let (Some(code), Some(side)) = (&lobby_code, my_side) {
                                let mut lobbies = state.lobbies.lock().await;
                                if let Some(lobby) = lobbies.get_mut(code) {
                                    if let Some(pf) = lobby.pending_forced.take() {
                                        if pf.captor != side {
                                            send(
                                                &tx,
                                                &ServerMsg::Error {
                                                    text: "Not your forced pick".into(),
                                                },
                                            )
                                            .await;
                                            lobby.pending_forced = Some(pf);
                                            continue;
                                        }
                                        let idx = idx.min(pf.options.len() - 1);
                                        let fm = pf.options[idx];
                                        if let Some(ref mut session) = lobby.session {
                                            session.game.state =
                                                apply(&session.game.state, fm);
                                            let mut remaining = pf.remaining_pips;
                                            remaining = remaining_after(&remaining, fm.pips);
                                            let state_snapshot = session.game.state.clone();
                                            let roll = pf.roll;

                                            // Broadcast the forced move
                                            broadcast(
                                                &lobby.players,
                                                &ServerMsg::MovesApplied {
                                                    side: pf.captor.letter().to_string(),
                                                    applied: vec![fm],
                                                    state: state_snapshot.clone(),
                                                    roll,
                                                    again: false,
                                                },
                                            );

                                            // Track any new finishers (e.g. captor finished via forced reply)
                                            for &s in &session.game.state.active {
                                                if session.game.state.has_won(s)
                                                    && !session.finished.contains(&s)
                                                {
                                                    session.finished.push(s);
                                                }
                                            }

                                            if !remaining.is_empty()
                                                && legal_turns_remaining(
                                                    &state_snapshot,
                                                    &remaining,
                                                )
                                                .iter()
                                                .any(|v| !v.is_empty())
                                            {
                                                // Original player continues with remaining pips (same nonce)
                                                let players = lobby.players.clone();
                                                let net_cnt = lobby.network_count;
                                                let cc = lobby.creator_is_computer;
                                                let nn = session.turn_nonce;
                                                send_turn(
                                                    &players,
                                                    nn,
                                                    net_cnt,
                                                    cc,
                                                    &state_snapshot,
                                                    pf.original_side,
                                                    roll,
                                                )
                                                .await;
                                                let pl = lobby_to_persisted(code, lobby);
                                                let _ = lobby;
                                                let _ = lobbies;
                                                persist_pl(&pl, &state).await;
                                                continue;
                                            }

                                            // Finalize turn
                                            let again = roll.is_double()
                                                && !session.game.state.has_won(pf.original_side);

                                            if server_game_over(&session.game.state) {
                                                let win_strs: Vec<String> =
                                                    if session.game.state.teams
                                                        && session.game.state.active.len() == 4
                                                    {
                                                        session.finished.iter().map(|s| s.letter().to_string()).collect()
                                                    } else {
                                                        vec![session.finished.first().map(|s| s.letter().to_string()).unwrap_or_default()]
                                                    };
                                                let result_msg = if win_strs.len() > 1 {
                                                    format!("Team {} wins!", win_strs.join("+"))
                                                } else {
                                                    format!("Player {} wins!", win_strs[0])
                                                };
                                                let fin_strs: Vec<String> = session.finished
                                                    .iter()
                                                    .map(|s| s.letter().to_string())
                                                    .collect();
                                                let go = ServerMsg::GameOver {
                                                    winners: win_strs,
                                                    result_msg,
                                                    state: session.game.state.clone(),
                                                    finished: fin_strs,
                                                };
                                                broadcast(&lobby.players, &go);
                                                if let Some(creator) = lobby.players.iter().find(|p| p.side == Side::A) {
                                                    let json = serde_json::to_string(&go).unwrap();
                                                    for t in &creator.txs { let _ = t.send(json.clone()); }
                                                }
                                                let pl = lobby_to_persisted(code, lobby);
                                                let _ = lobby;
                                                let _ = lobbies;
                                                persist_pl(&pl, &state).await;
                                                continue;
                                            }

                                            if !again {
                                                session.game.state.to_move =
                                                    next_unfinished_active(
                                                        &session.game.state,
                                                        pf.original_side,
                                                    );
                                            }
                                            session.current_roll =
                                                session.dice.roll();
                                            session.turn_nonce += 1;
                                            let next_side =
                                                session.game.state.to_move;
                                            let new_roll = session.current_roll;
                                            let turn_state =
                                                session.game.state.clone();
                                            let players = lobby.players.clone();
                                            let net_cnt = lobby.network_count;
                                            let cc = lobby.creator_is_computer;
                                            let nn = session.turn_nonce;
                                            let pl = lobby_to_persisted(code, lobby);
                                            let _ = lobby;
                                            let _ = lobbies;
                                            persist_pl(&pl, &state).await;
                                            send_turn(
                                                &players,
                                                nn,
                                                net_cnt,
                                                cc,
                                                &turn_state,
                                                next_side,
                                                new_roll,
                                            )
                                            .await;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        ClientMsg::RestartGame => {
                            if let (Some(code), Some(side)) = (&lobby_code, my_side) {
                                // Only creator (side A) can restart
                                if side == Side::A {
                                    let mut lobbies = state.lobbies.lock().await;
                                    if let Some(lobby) = lobbies.get_mut(code) {
                                        let active: Vec<Side> =
                                            (0..lobby.total_sides).map(|i| side_from_index(i, lobby.total_sides)).collect();
                                        let mut dice = RandomDice::from_entropy();
                                        let game = Game::start(active, &mut dice);
                                        let first_roll = dice.roll();
                                        let st = game.state.clone();
                                        let session = GameSession {
                                            game,
                                            dice,
                                            current_roll: first_roll,
                                            finished: Vec::new(),
                                            turn_nonce: 1,
                                        };
                                        lobby.session = Some(session);
                                        lobby.pending_forced = None;
                                        let nicknames: Vec<(String, String)> = lobby
                                            .players
                                            .iter()
                                            .filter(|p| !(p.side == Side::A && lobby.creator_is_computer))
                                            .map(|p| (p.side.letter().to_string(), p.nickname.clone()))
                                            .collect();
                                        for p in &lobby.players {
                                            let json = serde_json::to_string(&ServerMsg::GameStart {
                                                side: p.side.letter().to_string(),
                                                state: st.clone(),
                                                nicknames: nicknames.clone(),
                                                sid: String::new(),
                                            }).unwrap();
                                            for t in &p.txs { let _ = t.send(json.clone()); }
                                        }
                                        // Computer sides: send GameStart to creator (Side::A)
                                        for comp_idx in (1 + lobby.network_count)..lobby.total_sides {
                                            let comp_side = side_from_index(comp_idx, lobby.total_sides);
                                            if let Some(creator) = lobby.players.iter().find(|p| p.side == Side::A) {
                                                let json = serde_json::to_string(&ServerMsg::GameStart {
                                                    side: comp_side.letter().to_string(),
                                                    state: st.clone(),
                                                    nicknames: nicknames.clone(),
                                                    sid: String::new(),
                                                }).unwrap();
                                                for t in &creator.txs { let _ = t.send(json.clone()); }
                                            }
                                        }
                                        lobby.session.as_mut().unwrap().turn_nonce += 1;
                                        let next_side = lobby.session.as_ref().unwrap().game.state.to_move;
                                        let new_roll = lobby.session.as_ref().unwrap().current_roll;
                                        let nn = lobby.session.as_ref().unwrap().turn_nonce;
                                        send_turn(
                                            &lobby.players,
                                            nn,
                                            lobby.network_count,
                                            lobby.creator_is_computer,
                                            &st,
                                            next_side,
                                            new_roll,
                                        )
                                        .await;
                                        let pl = lobby_to_persisted(code, lobby);
                                        let _ = lobby;
                                        let _ = lobbies;
                                        persist_pl(&pl, &state).await;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // --- disconnect: keep player in lobby for reconnect ---
    if let (Some(code), Some(side)) = (lobby_code, my_side) {
        let lobbies = state.lobbies.lock().await;
        if let Some(lobby) = lobbies.get(&code) {
            let json = serde_json::to_string(&ServerMsg::Disconnected {
                side: side.letter().to_string(),
            })
            .unwrap();
            for p in &lobby.players {
                if p.side != side {
                    for t in &p.txs { let _ = t.send(json.clone()); }
                }
            }
        }
    }

    send_task.abort();
}
