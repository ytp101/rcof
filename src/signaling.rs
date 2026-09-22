use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

#[derive(Clone, Serialize, Deserialize)]
pub struct Join {
    pub room: String,
    pub id: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Joined {
    pub token: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Poll {
    pub peer: Option<String>,
    pub messages: Vec<RTCSessionDescription>,
}
struct Member {
    room: String,
    id: String,
    last_seen: Instant,
    inbox: VecDeque<RTCSessionDescription>,
}
#[derive(Default)]
struct Rooms {
    members: HashMap<String, Member>,
}
type Shared = Arc<Mutex<Rooms>>;
type ApiError = (StatusCode, String);
impl Rooms {
    fn expire(&mut self) {
        self.members
            .retain(|_, m| m.last_seen.elapsed() < Duration::from_secs(10));
    }
    fn join(&mut self, request: Join) -> Result<Joined, ApiError> {
        self.expire();
        let valid = |s: &str| {
            !s.is_empty()
                && s.len() <= 48
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        if !valid(&request.room) || !valid(&request.id) {
            return Err((
                StatusCode::BAD_REQUEST,
                "room/id must be 1–48 letters, digits, - or _".into(),
            ));
        }
        let members: Vec<_> = self
            .members
            .values()
            .filter(|m| m.room == request.room)
            .collect();
        if members.iter().any(|m| m.id == request.id) {
            return Err((StatusCode::CONFLICT, "instance id already joined".into()));
        }
        if members.len() >= 2 {
            return Err((
                StatusCode::CONFLICT,
                "room already has two participants".into(),
            ));
        }
        if self.members.len() >= 32 {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "local server is full".into(),
            ));
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.members.insert(
            token.clone(),
            Member {
                room: request.room,
                id: request.id,
                last_seen: Instant::now(),
                inbox: VecDeque::new(),
            },
        );
        Ok(Joined { token })
    }
}
async fn join(
    State(state): State<Shared>,
    Json(request): Json<Join>,
) -> Result<Json<Joined>, ApiError> {
    state.lock().unwrap().join(request).map(Json)
}
async fn poll(
    State(state): State<Shared>,
    Path(token): Path<String>,
) -> Result<Json<Poll>, ApiError> {
    let mut rooms = state.lock().unwrap();
    rooms.expire();
    let me = rooms
        .members
        .get_mut(&token)
        .ok_or((StatusCode::NOT_FOUND, "session expired or left".into()))?;
    me.last_seen = Instant::now();
    let room = me.room.clone();
    let id = me.id.clone();
    let messages = me.inbox.drain(..).collect();
    let peer = rooms
        .members
        .values()
        .find(|m| m.room == room && m.id != id)
        .map(|m| m.id.clone());
    Ok(Json(Poll { peer, messages }))
}
async fn signal(
    State(state): State<Shared>,
    Path(token): Path<String>,
    Json(sdp): Json<RTCSessionDescription>,
) -> Result<StatusCode, ApiError> {
    // The server is local-only. Still reject non-loopback SDP at the receiving client.
    let mut rooms = state.lock().unwrap();
    rooms.expire();
    let me = rooms
        .members
        .get(&token)
        .ok_or((StatusCode::NOT_FOUND, "unknown session".into()))?;
    let room = me.room.clone();
    let id = me.id.clone();
    let peer = rooms
        .members
        .values_mut()
        .find(|m| m.room == room && m.id != id)
        .ok_or((StatusCode::CONFLICT, "peer has left".into()))?;
    if peer.inbox.len() >= 8 {
        return Err((StatusCode::TOO_MANY_REQUESTS, "signal queue full".into()));
    }
    peer.inbox.push_back(sdp);
    Ok(StatusCode::NO_CONTENT)
}
async fn leave(State(state): State<Shared>, Path(token): Path<String>) -> StatusCode {
    state.lock().unwrap().members.remove(&token);
    StatusCode::NO_CONTENT
}
pub fn router() -> Router {
    Router::new()
        .route("/health", get(|| async { "rcof local signaling" }))
        .route("/join", post(join))
        .route("/session/{token}", get(poll).delete(leave).post(signal))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(Shared::default())
}
pub fn check_address(address: SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() || !address.is_ipv4() {
        bail!("P0 only accepts IPv4 loopback addresses (127.0.0.1)");
    }
    Ok(())
}
pub async fn serve(address: SocketAddr) -> Result<()> {
    check_address(address)?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("cannot bind signaling server")?;
    println!(
        "signaling listening on http://{} (loopback only)",
        listener.local_addr()?
    );
    axum::serve(listener, router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
pub struct Client {
    http: reqwest::Client,
    url: String,
}
impl Client {
    pub fn new(address: SocketAddr) -> Result<Self> {
        check_address(address)?;
        Ok(Self {
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()?,
            url: format!("http://{address}"),
        })
    }
    async fn checked(response: reqwest::Response) -> Result<reqwest::Response> {
        if !response.status().is_success() {
            bail!(
                "signaling: {} {}",
                response.status(),
                response.text().await?
            );
        }
        Ok(response)
    }
    pub async fn join(&self, room: &str, id: &str) -> Result<String> {
        let r = self
            .http
            .post(format!("{}/join", self.url))
            .json(&Join {
                room: room.into(),
                id: id.into(),
            })
            .send()
            .await
            .context("cannot reach local signaling server; run `rcof server` first")?;
        Ok(Self::checked(r).await?.json::<Joined>().await?.token)
    }
    pub async fn poll(&self, token: &str) -> Result<Poll> {
        Self::checked(
            self.http
                .get(format!("{}/session/{token}", self.url))
                .send()
                .await?,
        )
        .await?
        .json()
        .await
        .map_err(Into::into)
    }
    pub async fn send(&self, token: &str, sdp: &RTCSessionDescription) -> Result<()> {
        Self::checked(
            self.http
                .post(format!("{}/session/{token}", self.url))
                .json(sdp)
                .send()
                .await?,
        )
        .await?;
        Ok(())
    }
    pub async fn leave(&self, token: &str) -> Result<()> {
        Self::checked(
            self.http
                .delete(format!("{}/session/{token}", self.url))
                .send()
                .await?,
        )
        .await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_limits_and_expiry() {
        let mut r = Rooms::default();
        let req = |id: &str| Join {
            room: "test".into(),
            id: id.into(),
        };
        let a = r.join(req("a")).unwrap();
        assert_eq!(r.join(req("a")).err().unwrap().0, StatusCode::CONFLICT);
        r.join(req("b")).unwrap();
        assert!(r.join(req("c")).is_err());
        r.members.get_mut(&a.token).unwrap().last_seen = Instant::now() - Duration::from_secs(11);
        r.join(req("c")).unwrap();
        assert!(r.join(req("../invalid")).is_err());
    }
}
