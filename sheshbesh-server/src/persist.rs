use serde::{Deserialize, Serialize};
use sheshbesh::{DiceRoll, GameState, Side};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

/// Timeout for any single Redis operation — prevents the entire server from
/// hanging when the Upstash connection stalls (single-threaded tokio runtime
/// on a shared-cpu-1x Fly machine).
const REDIS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct PersistedLobby {
    pub(crate) code: String,
    pub(crate) players: Vec<(Side, String, String)>, // (side, nickname, sid)
    pub(crate) max_players: usize,
    pub(crate) total_sides: usize,
    pub(crate) network_count: usize,
    pub(crate) creator_sid: String,
    pub(crate) game_state: Option<GameState>,
    pub(crate) current_roll: Option<DiceRoll>,
    /// fields only used during PickForced — see pending_forced in main.rs
    pub(crate) forced_captor: Option<Side>,
    pub(crate) forced_options: Option<Vec<sheshbesh::Move>>,
    pub(crate) forced_roll: Option<DiceRoll>,
    pub(crate) forced_remaining: Option<Vec<u8>>,
    pub(crate) forced_original_side: Option<Side>,
}

pub(crate) struct RedisStore {
    conn: redis::aio::MultiplexedConnection,
}

/// Create a sender that silently discards messages (placeholder for disconnected players).
pub(crate) fn dead_tx() -> mpsc::UnboundedSender<String> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while rx.recv().await.is_some() {}
    });
    tx
}

impl RedisStore {
    pub(crate) async fn connect(url: &str) -> Result<Self, String> {
        let client = redis::Client::open(url).map_err(|e| e.to_string())?;
        let conn = timeout(REDIS_TIMEOUT, client.get_multiplexed_async_connection())
            .await
            .map_err(|_| "Redis connection timeout".to_string())?
            .map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// 7 days TTL — finished lobbies auto-clean on restart.
    const TTL: u64 = 604800;

    pub(crate) async fn save_lobby(&mut self, lobby: &PersistedLobby) {
        let key = format!("lobby:{}", lobby.code);
        let json = serde_json::to_string(lobby).unwrap();
        let _: () = timeout(REDIS_TIMEOUT, redis::cmd("SET")
            .arg(&key)
            .arg(&json)
            .arg("EX")
            .arg(Self::TTL)
            .query_async(&mut self.conn))
            .await
            .unwrap_or(Ok(()))
            .unwrap_or(());
    }

    pub(crate) async fn load_all_lobbies(&mut self) -> Vec<PersistedLobby> {
        let mut out = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = timeout(
                REDIS_TIMEOUT,
                redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg("lobby:*")
                    .arg("COUNT")
                    .arg(100)
                    .query_async(&mut self.conn),
            )
            .await
            .unwrap_or(Ok((0, Vec::new())))
            .unwrap_or_default();
            for key in &keys {
                if let Ok(Some(json)) = timeout(
                    REDIS_TIMEOUT,
                    redis::cmd("GET")
                        .arg(key)
                        .query_async::<Option<String>>(&mut self.conn),
                )
                .await
                .unwrap_or(Ok(None))
                {
                    if let Ok(lobby) = serde_json::from_str::<PersistedLobby>(&json) {
                        out.push(lobby);
                    }
                }
            }
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        out
    }

    #[allow(dead_code)]
    pub(crate) async fn remove_lobby(&mut self, code: &str) {
        let key = format!("lobby:{code}");
        let _: () = timeout(REDIS_TIMEOUT, redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut self.conn))
            .await
            .unwrap_or(Ok(()))
            .unwrap_or(());
    }
}
