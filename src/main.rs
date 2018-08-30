#![allow(unused_variables)]
extern crate env_logger;
extern crate uuid;
extern crate qrcode;

#[macro_use]
extern crate actix;
extern crate actix_web;
#[macro_use]
extern crate tera;
#[macro_use]
extern crate serde_derive;
extern crate serde;
#[macro_use]
extern crate serde_json;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use qrcode::{QrCode, Version, EcLevel};
use qrcode::render::svg;
use actix::prelude::*;
use actix_web::{error, fs, http, middleware, server, ws, App, Error, HttpContext, HttpRequest, HttpResponse};
use serde_json::Value;

static BASE_URI: &'static str = "127.0.0.1:8080";

fn pair(r: &HttpRequest<ArcAppState>) -> Result<HttpResponse, Error> {
    let uuid: String = r.match_info().query("uuid")?;
    let key: usize = r.match_info().query("key")?;
    let rooms = r.state().rooms.read().unwrap();
    if let Some(room) = rooms.get(&uuid) {
        if let Some(player) = room.players.get(key) {
            if !player.is_connected {
                let mut ctx = tera::Context::new();
                ctx.add("scheme", if r.connection_info().scheme()=="https" {"wss"} else {"ws"} );
                ctx.add("host", r.connection_info().host());
                ctx.add("room_id", &uuid.to_owned());
                ctx.add("player_key", &key);
                ctx.add("player_token", &player.token.to_owned());
                let s = r.state()
                    .template
                    .render("pair.html", &ctx)
                    .map_err(|_| error::ErrorInternalServerError("Template error"))?;
                Ok(HttpResponse::Ok().content_type("text/html").body(s))
            } else {
                Ok(HttpResponse::NotFound().reason("Player has connected").finish())
            }
        } else {
            Ok(HttpResponse::NotFound().reason("Controller count exceeded").finish())
        }
    } else {
        Ok(HttpResponse::NotFound().reason("Not found room").finish())
    }
}

fn room(r: &HttpRequest<ArcAppState>) -> Result<HttpResponse, Error> {
    let uuid: String = r.match_info().query("uuid")?;
    let mut links: Vec<PlayerLink> = Vec::new();
    let rooms = r.state().rooms.read().unwrap();
    if let Some(room) = rooms.get(&uuid) {
        let player_count = room.players.len();
        for i in 0..player_count {
            let uri = format!("{}://{}/pair/{}/{}/", r.connection_info().scheme(), r.connection_info().host(), &uuid, i);
            let code = QrCode::new(&uri.as_bytes()).unwrap();
            let link = PlayerLink {
                qr: code.render::<svg::Color>()
                    .min_dimensions(200, 200)
                    .build(),
                uri: uri
            };
            links.push(link);
        }
        let mut ctx = tera::Context::new();
        ctx.add("scheme", if r.connection_info().scheme()=="https" {"wss"} else {"ws"} );
        ctx.add("host", r.connection_info().host());
        ctx.add("room_id", &uuid.to_owned());
        ctx.add("player_count", &player_count);
        ctx.add("links", &links);
        let s = r.state()
            .template
            .render("room.html", &ctx)
            .map_err(|_| error::ErrorInternalServerError("Template error"))?;
        Ok(HttpResponse::Ok().content_type("text/html").body(s))
    } else {
        Ok(HttpResponse::NotFound().reason("Not found room").finish())
    }
}

fn ws_index(r: &HttpRequest<ArcAppState>) -> Result<HttpResponse, Error> {
    ws::start(r, Controller)
}

#[derive(Serialize, Deserialize, Debug)]
struct PlayerLink {
    qr: String,
    uri: String,
}

struct Controller;

type ArcAppState = Arc<AppState>;
#[derive(Debug)]
struct AppState {
    template: tera::Tera,
    rooms: RwLock<HashMap<String, RoomState<Game>>>,
}

impl Actor for Controller {
    type Context = ws::WebsocketContext<Self, ArcAppState>;
}

impl StreamHandler<ws::Message, ws::ProtocolError> for Controller {
    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        match msg {
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Text(text) => {
                let vopt: Result<Value, _> = serde_json::from_str(&text);
                if let Ok(v) = vopt {
                    if let Some(t) = v.get("type") {
                        if let Some(tt) = t.as_str() {
                            match tt {
                                "CHKCON" => {
                                    let payload = v.get("payload").unwrap().clone();
                                    let state = ctx.state().clone();
                                    ctx.text(check_connection(state, serde_json::from_value(payload).unwrap()))
                                }
                                "CONNECT" => {
                                    let payload = v.get("payload").unwrap().clone();
                                    let state = ctx.state().clone();
                                    ctx.text(connect(state, serde_json::from_value(payload).unwrap()))
                                }
                                "DISCONNECT" => {
                                    let payload = v.get("payload").unwrap().clone();
                                    let state = ctx.state().clone();
                                    ctx.text(disconnect(state, serde_json::from_value(payload).unwrap()))
                                }
                                "INPUT" => {
                                    let payload = v.get("payload").unwrap().clone();
                                    let state = ctx.state().clone();
                                    ctx.text(input(state, serde_json::from_value(payload).unwrap()))
                                }
                                "DATA" => {
                                    let payload = v.get("payload").unwrap().clone();
                                    let state = ctx.state().clone();
                                    ctx.text(data(state, serde_json::from_value(payload).unwrap()))
                                }
                                _ => ctx.text(r#"{
                                    "type": "ERR",
                                    "payload": {"message": "Unknown type"}
                                }"#)
                            }
                        } else {
                            ctx.text(r#"{
                                "type": "ERR",
                                "payload": {"message": "Type is not string"}
                            }"#)
                        }
                    } else {
                        ctx.text(r#"{
                            "type": "ERR",
                            "payload": {"message": "Not found type"}
                        }"#)
                    }
                } else {
                    ctx.text(r#"{
                        "type": "ERR",
                        "payload": {"message": "Not JSON format"}
                    }"#)
                }
            }
            ws::Message::Binary(bin) => ctx.binary(bin),
            ws::Message::Close(_) => {
                ctx.stop();
            }
            _ => (),
        }
    }
}

fn check_connection(state: ArcAppState, payload: CheckConnectionPayload) -> String {
    let rooms = state.rooms.read().unwrap();
    if let Some(room) = rooms.get(&payload.roomId) {
        let mut connection: HashMap<usize, bool> = HashMap::new();
        for key in payload.players {
            connection.insert(key, room
                              .players.get(key)
                              .map_or(false, |x| x.is_connected));
        }
        let json = json!({
            "type": "CHKCON_OK",
            "payload": &connection,
        });
        json.to_string()
    } else {
        r#"{
        "type": "ERR",
        "payload": {
            "message": "Not found room"
        }}"#.to_string()
    }
}

fn connect(state: ArcAppState, payload: ConnectPayload) -> String {
    let mut auth = false;
    let key = payload.key.parse::<usize>().unwrap();
    {
        let rooms = state.rooms.read().unwrap();
        if let Some(room) = rooms.get(&payload.roomId) {
            if let Some(player) = room.players.get(key) {
                if player.token == payload.token {
                    println!("CONNECT: P{} of {}", key, payload.roomId);
                    auth = true;
                }
            }
        }
    }
    if auth {
        let mut rooms = state.rooms.write().unwrap();
        let room = rooms.get_mut(&payload.roomId).unwrap();
        let player = room.players.get_mut(key).unwrap();
        player.is_connected = true;
        r#"{
            "type": "CONNECT_OK",
            "payload": {
                "connection": true
            }
        }"#.to_string()
    } else {
        r#"{
        "type": "ERR",
        "payload": {
            "message": "Invalid"
        }}"#.to_string()
    }
}

fn disconnect(state: ArcAppState, payload: DisconnectPayload) -> String {
    let mut auth = false;
    let key = payload.key.parse::<usize>().unwrap();
    {
        let rooms = state.rooms.read().unwrap();
        if let Some(room) = rooms.get(&payload.roomId) {
            if let Some(player) = room.players.get(key) {
                if player.token == payload.token {
                    println!("DISCONNECT: P{} of {}", key, payload.roomId);
                    auth = true;
                }
            }
        }
    }
    if auth {
        let mut rooms = state.rooms.write().unwrap();
        let room = rooms.get_mut(&payload.roomId).unwrap();
        let player = room.players.get_mut(key).unwrap();
        player.is_connected = false;
        r#"{
            "type": "DISCONNECT_OK",
            "payload": {
                "connection": false
            }
        }"#.to_string()
    } else {
        r#"{
        "type": "ERR",
        "payload": {
            "message": "Invalid"
        }}"#.to_string()
    }
}

fn input(state: ArcAppState, payload: InputPayload) -> String {
    let mut auth = false;
    let key = payload.key.parse::<usize>().unwrap();
    {
        let rooms = state.rooms.read().unwrap();
        if let Some(room) = rooms.get(&payload.roomId) {
            if let Some(player) = room.players.get(key) {
                if player.token == payload.token {
                    auth = true;
                }
            }
        }
    }
    if auth {
        let mut rooms = state.rooms.write().unwrap();
        let room = rooms.get_mut(&payload.roomId).unwrap();
        let player = room.game.players.get_mut(key).unwrap();
        player.0 += payload.x;
        player.1 += payload.y;
        r#"{
            "type": "INPUT_OK",
            "payload": {}
        }"#.to_string()
    } else {
        r#"{
        "type": "ERR",
        "payload": {
            "message": "Invalid"
        }}"#.to_string()
    }
}

fn data(state: ArcAppState, payload: DataPayload) -> String {
    let rooms = state.rooms.read().unwrap();
    if let Some(room) = rooms.get(&payload.roomId) {
        let json = json!({
            "type": "DATA_OK",
            "payload": &room.game.clone()
        });
        json.to_string()
    } else {
        r#"{
        "type": "ERR",
        "payload": {
            "message": "Invalid"
        }}"#.to_string()
    }
}


#[derive(Deserialize, Debug)]
struct CheckConnectionPayload {
    roomId: String,
    players: Vec<usize>,
}

#[derive(Deserialize, Debug)]
struct ConnectPayload {
    roomId: String,
    key: String,
    token: String,
}

#[derive(Deserialize, Debug)]
struct DisconnectPayload {
    roomId: String,
    key: String,
    token: String,
}

#[derive(Deserialize, Debug)]
struct InputPayload {
    roomId: String,
    key: String,
    token: String,
    x: i32,
    y: i32
}

#[derive(Deserialize, Debug)]
struct DataPayload {
    roomId: String,
}

#[derive(Serialize, Default, Debug, Clone)]
struct Game {
    players: Vec<(i32, i32)>
}

#[derive(Debug)]
struct Player {
    token: String,
    is_connected: bool,
}

impl Player {
    fn new() -> Player {
        Player {
            token: Uuid::new_v4().to_string(),
            is_connected: false,
        }
    }
}

#[derive(Debug)]
struct RoomState<T: Default> {
    players: Vec<Player>,
    game: T
}

impl<T: Default> RoomState<T> {
    fn new() -> RoomState<T> {
        RoomState {
            players: Vec::new(),
            game: T::default()
        }
    }
}

fn main() {
    ::std::env::set_var("RUST_LOG", "actix_web=info");
    env_logger::init();
    let sys = actix::System::new("ws-example");
    let tera = compile_templates!(concat!(env!("CARGO_MANIFEST_DIR"), "/static/*"));
    let mut state = Arc::new(AppState {
        template: tera,
        rooms: RwLock::new(HashMap::new()),
    });

    server::new(move || {
        App::with_state(state.clone())
            .middleware(middleware::Logger::default())
            .resource("/ws/", |r| r.method(http::Method::GET).f(ws_index))
            .resource("/pair/{uuid}/{key}/", |r| r.method(http::Method::GET).f(pair))
            .resource("/room/{uuid}/", |r| r.method(http::Method::GET).f(room))
            .resource("/new/", |r| r.method(http::Method::GET).f(|req| {
                let mut room: RoomState<Game> = RoomState::new();
                let room_id = Uuid::new_v4().to_string();
                room.players.push(Player::new());
                room.players.push(Player::new());
                room.game.players.push((50,50));
                room.game.players.push((150,50));
                //{
                    let mut rooms = req.state().rooms.write().unwrap();
                    rooms.insert(room_id.clone(), room);
                //}
                HttpResponse::Found()
                    .header("LOCATION", format!("/room/{}/", &room_id))
                    .finish()}))
            .handler("/", fs::StaticFiles::new("static/")
                     .unwrap()
                     .index_file("index.html"))
            })
        .bind(BASE_URI).unwrap()
        .start();

    println!("Started http server: 127.0.0.1:8080");
    let _ = sys.run();
}
