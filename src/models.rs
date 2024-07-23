// --- MODELS --- //

// the below struct represents the on-going matche
// the clients will interactively set the questions result based on the below struct format on the state.
// the struct will have the update and finish game room functionlaities.
// the struct must have a function that will finilize the game room. the transaction part i mean.

use std::{
    collections::HashMap, default, hash::Hash, io::{Read, Write}, sync::Arc
};

use futures_channel::mpsc;
use futures_util::{
    stream::{SplitSink, SplitStream},
    AsyncRead, AsyncWrite, Future, Sink, Stream,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_tungstenite::{tungstenite::{stream::MaybeTlsStream, Message}, WebSocketStream};
// use futures_util::Future;
// the ongoing matche must be saved in a state so that we know the users can have a game in two different dvices.
#[derive(Default)]
pub struct OngoingMatch {
    pub reward: i32,
    pub user: User,
    pub contestant: User,
}

impl OngoingMatch {
    // only returns false when a use is done answering.
    pub fn update_result(&mut self, user: &str, q_res: bool) -> bool {
        if !user.eq(&self.user.address) {
            if self.contestant.result.len() < 3 {
                self.contestant.result.push(q_res);
                return true;
            }
            return false;
        }
        if self.user.result.len() < 3 {
            self.user.result.push(q_res);
            return true;
        }
        false
    }
}

// the user will be initiated during the match and will be dropped after the match is over.
#[derive(Debug, Default, Clone)]
pub struct User {
    pub address: String,
    pub result: Vec<bool>,
    // 1 for true 0 for false
    pub thread_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

// --- STATE TYPES --- //

// users will request to the server and the server will save their data in the queue.
// and if a match was found the match will be started.
// and if there was no other user already in the queue, the user will be added to the queue.

pub trait New {
    fn new_empty() -> Self;
}

// mapping the entrance fees to the wallet addresses requesting for a game
pub type WaitQueue = Arc<Mutex<HashMap<i32, Vec<User>>>>;
pub type FoundQueue = Arc<Mutex<HashMap<String, User>>>;

impl New for FoundQueue {
    fn new_empty() -> Self {
        let hm: HashMap<String, User> = HashMap::new();
        Arc::new(Mutex::new(hm))
    }
}

impl New for WaitQueue {
    fn new_empty() -> Self {
        let hm: HashMap<i32, Vec<User>> = HashMap::new();
        Arc::new(Mutex::new(hm))
    }
}


pub enum MatchResponse {
    Added(usize),
    Wait(String),
    FoundMatch(Vec<User>),
    UpdateResult(Vec<bool>),
    Done(Vec<bool>),
    Undefined(String),
}

impl Default for MatchResponse {
    fn default() -> Self {
        Self::Undefined("Undefined".to_string())
    }
}

// impl Future for MatchResponse {
//     type Output = Self;

//     fn poll(
//         self: std::pin::Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//     ) -> std::task::Poll<Self::Output> {
//         todo!()
//     }
// }

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct IncomingUser {
    pub wallet_address: String,
    pub entrance_amount: i32,
}

#[derive(Serialize, Deserialize)]
pub enum InnerMsg {
    Fetch{}, 
    Update{res: bool},
    HandshakeInit {user: String}, // the initiator address
    HandshakeStablish {user: String} // the initiating address
}
