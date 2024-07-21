
// --- MODELS --- // 

// the below struct represents the on-going matche
// the clients will interactively set the questions result based on the below struct format on the state.
// the struct will have the update and finish game room functionlaities.
// the struct must have a function that will finilize the game room. the transaction part i mean.

use std::{collections::HashMap, io::{Read, Write}, sync::Arc};

use futures_util::{stream::{SplitSink, SplitStream}, AsyncRead, AsyncWrite, Sink, Stream};
use tokio::sync::Mutex;
use tokio_tungstenite::{ tungstenite::stream::MaybeTlsStream, WebSocketStream};
use std::fmt::Debug as DebugTrait;

// the ongoing matche must be saved in a state so that we know the users can have a game in two different dvices.
pub trait WS: Read + Write + Unpin + DebugTrait{}
pub struct OngoingMatch<T: WS>  {
    pub reward: i32,
    pub user_1: User<T>,  
    pub user_2: User<T>, 

}

// impl OngoingMatch {
//     // only returns false when a use is done answering.
//     pub fn update_result(&mut self, user: &str, q_res: bool) -> bool{
//     if !user.eq(&self.user_1.address) {
//         if self.user_2.result.len() < 3 {
//             self.user_2.result.push(q_res);
//         return true;
//         } 
//         return false;       
//     } 
//     if self.user_1.result.len() < 3 {
//         self.user_1.result.push(q_res);
//         return true;
//     } 
//     false
// }

// }
// the user will be inititated during the match and will be dropped after the match is over.
#[derive(Debug)]
pub struct User<T:  WS> 
{
    pub address: String, 
    pub result: Vec<bool>,
    pub ws_socket: WebSocketStream<MaybeTlsStream<T>>
}

// --- STATE TYPES --- //

// users will request to the server and the server will save their data in the queue.
// and if a matach was found the match will be started.
// and if there was no other user already in the queue, the user will be added to the queue.

// mapping the entrance fees to the wallet addresses requesting for a game
type Queue<T> = Arc<Mutex<HashMap<i32, Vec<User<T>>>>>;

pub trait QueueOperations<T: WS> {
    async fn find_match(_user: User<T>, _entrance_tokens: i32, _queue: Queue<T>){}
    fn remove(){}
}
impl<T: WS> QueueOperations<T> for Queue<T> {
// this will search for the matches and if found it will return the contestant user address for the game room and if not will add the user to the queue.
    async fn find_match(_user: User<T>, _entrance_tokens: i32, _queue: Queue<T> ) {
        // getting the lock over the queue
        let mut _q = _queue.lock().await;  
         // Get the list of users for the given entrance tokens
    let users = _q.entry(_entrance_tokens).or_insert_with(Vec::new);

    match users.len() {
        0 => {
            // No users in the queue, add the user
            users.push(_user);
            println!("User added to the queue. Needs to wait for a match.");
        }
        1 => {
            // One user in the queue
            if users[0].address == _user.address {
                println!("User needs to wait for a match.");
            } else {
                // Match found with the existing user
                let matched_user = users.pop().unwrap();
                println!("Match found between {:?} and {:?}", matched_user, _user);
                // Return the match
            }
        }
        _ => {
            if let Some((index, _)) = users.iter().enumerate().find(|(_, u)| u.address != _user.address) {
                // Remove the matched user and the requesting user
                let matched_user = users.remove(index);
                println!("Match found between {:?} and {:?}", matched_user, _user);
                // Return the match or handle the matched pair here
            } else {
                panic!("impossible panic !!")
            }
        }
    }
}

}
