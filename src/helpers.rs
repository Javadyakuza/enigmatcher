use std::{
    borrow::BorrowMut,
    collections::HashMap,
    env,
    fmt::format,
    io::Error,
    net::SocketAddr,
    ops::Deref,
    sync::{Arc, Mutex},
};

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{
    future::{self, Ready},
    pin_mut, SinkExt, StreamExt, TryStreamExt,
};
use log::info;
use rocket::http::ext::IntoCollection;
use serde::Serialize;
use tokio::{net::{TcpListener, TcpStream}, sync::mpsc::UnboundedReceiver};
use tokio_tungstenite::{
    tungstenite::{Message, WebSocket},
    WebSocketStream,
};

use crate::models::{Fetch, FoundQueue, IncomingUser, InnerMsg, MatchResponse, OngoingMatch, User, WaitQueue};

pub type Tx = UnboundedSender<Message>;
// pub type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

pub async fn handle_connection(
    // peer_map: PeerMap,
    raw_stream: TcpStream,
    addr: SocketAddr,
    wait_queue: WaitQueue,
    found_queue: FoundQueue

) {
    println!("Incoming TCP connection from: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("WebSocket connection established: {}", addr);

    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded();
    // peer_map.lock().unwrap().insert(addr, tx);
    let mut rx: futures_channel::mpsc::UnboundedReceiver<Vec<u8>> = rx;
    println!("{:?}", tx);
    
    let (mut outgoing, incoming) = ws_stream.split();
    let outgoing_multi = Arc::new(Mutex::new(outgoing));

    let mut ongoing_match: OngoingMatch = Default::default();

    // web socket
    let broadcast_incoming = incoming.try_for_each(|msg| {
        let mut match_responses: MatchResponse = Default::default();
        let mut sending_message: Message = Message::text("initial text");
        let text_msg = msg.to_text().unwrap();
        if text_msg.contains("wallet_address") {
            let incoming_user: IncomingUser = serde_json::from_str(text_msg).unwrap();
            let _ = async {
                match_responses = find_match(
                    User {
                        address: incoming_user.wallet_address,
                        result: vec![],
                        thread_tx: Some(tx.clone()),
                    },
                    incoming_user.entrance_amount,
                    wait_queue.clone(),
                    found_queue.clone()
                )
                .await;
            };
            match match_responses {
                MatchResponse::Added(idx) => {
                    // telling user to wait
                    sending_message = Message::text("wait ...");
                }
                MatchResponse::Wait(msg) => {
                    sending_message = Message::text(msg);
                }
                MatchResponse::FoundMatch(users) => {
                    sending_message = Message::text(format!("{:?}", users));
                    // notifying the contestant thread that we are the its contestant  
                    // handshaking


                }
                MatchResponse::Undefined(msg) => {
                    sending_message = Message::text("undefined behavior accrued !!");
                }

                _ => {}
            }
        } else {
            let user_addr = &ongoing_match.user.address;
            let res = if text_msg == "true" {
                true
            } else if text_msg == "false" {
                false
            } else {
                panic!("unsupported result")
            };
            let _ = ongoing_match.update_result(&user_addr.clone(), res);

            // notifying the contestant thread that we updated or results 
            // recp.unbounded_send(msg.clone()).unwrap();
        }

        // let peers = peer_map.lock().unwrap();

        // // We want to broadcast the message to everyone except ourselves.
        // let broadcast_recipients = peers
        //     .iter()
        //     .filter(|(peer_addr, _)| peer_addr != &&addr)
        //     .map(|(_, ws_sink)| ws_sink);

        // // broad casting
        // for recp in broadcast_recipients {
        //     println!("sending message to {:?}", recp);
        //     recp.unbounded_send(msg.clone()).unwrap();
        // }
        let _ = async {
            if outgoing_multi
                .clone()
                .lock()
                .unwrap()
                .send(sending_message)
                .await
                .is_err()
            {
                println!("Error sending message to WebSocket");
            }
        };
        future::ok(())
    });
    
    // inner platform
    let receive_from_others = async {
        rx.map(|msg: Vec<u8>| {
            // in here we got the update result from the contestant operating thread. we will notify the client to go to the next round if not completed the match yet
            // and we will send a done message and the array of six specifying the results of the contestant and the current user.
            if msg.len() < 3 {
                // still updating, routing the result letting for the next match
                let _ = async {
                    if outgoing_multi
                        .clone()
                        .lock()
                        .unwrap()
                        .send(Message::binary(msg))
                        .await
                        .is_err()
                    {
                        println!("Error sending message to WebSocket");
                    }
                };
            } else {
                // in this case the result is 3 we just set update the result
            
            }
        })
    };

    // .forward(lock);
    pin_mut!(broadcast_incoming, receive_from_others);
    future::select(broadcast_incoming, receive_from_others).await;

    println!("{} disconnected", &addr);
    // peer_map.lock().unwrap().remove(&addr);
}

pub async fn find_match(_user: User, _entrance_tokens: i32, wait_queue: WaitQueue, found_queue: FoundQueue) -> MatchResponse {
    // getting the lock over the queue
    let mut _q = wait_queue.lock().await;
    // Get the list of users for the given entrance tokens
    let users = _q.entry(_entrance_tokens).or_insert_with(Vec::new);

    match users.len() {
        0 => {
            // No users in the queue, add the  user
            users.push(_user);
            MatchResponse::Added(users.len() - 1)
        }
        1 => {
            // One user in the queue
            if users[0].address == _user.address {
                MatchResponse::Wait("No one found, wait ...".into())
            } else {
                // Match found with the existing user
                let matched_user = users.pop().unwrap();
                // telling each thread to pick their opponents
                // adding the users to the found queue 
                let mut fq = found_queue.lock().await;
                fq.insert(matched_user.address.clone(), User {
                    address: _user.address.clone(),  
                    result: _user.result.clone(),
                    thread_tx:  _user.thread_tx.clone(),
                });

                let _ = matched_user.thread_tx.unwrap().unbounded_send(bincode::serialize(&InnerMsg::Fetch {  }).unwrap()).unwrap();
                MatchResponse::FoundMatch(vec![matched_user.address.clone(), _user.address.clone()])
            }
        }
        _ => {
            if let Some((index, _)) = users
                .iter()
                .enumerate()
                .find(|(_, u)| u.address != _user.address)
            {
                // Remove the matched user and the requesting user
                let matched_user = users.remove(index);
                let mut fq = found_queue.lock().await;
                fq.insert(matched_user.address.clone(), User {
                    address: _user.address.clone(),  
                    result: _user.result.clone(),
                    thread_tx:  _user.thread_tx.clone(),
                });
                
                let _ = matched_user.thread_tx.unwrap().unbounded_send(bincode::serialize(&InnerMsg::Fetch {  }).unwrap()).unwrap();
                MatchResponse::FoundMatch(vec![matched_user.address.clone(), _user.address.clone()])
            } else {
                panic!("impossible panic !!")
            }
        }
    }
}
