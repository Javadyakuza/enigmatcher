use std::{
    borrow::BorrowMut, collections::HashMap, env, fmt::format, io::Error, net::SocketAddr,
    ops::Deref, rc::Rc, sync::Arc,
};

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{
    future::{self, Map, Ready},
    pin_mut, SinkExt, StreamExt, TryStreamExt,
};
use log::info;
use rocket::http::ext::IntoCollection;
use serde::Serialize;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc::UnboundedReceiver, Mutex},
};
use tokio_tungstenite::{
    tungstenite::{Message, WebSocket},
    WebSocketStream,
};

use crate::models::{
    FoundQueue, IncomingUser, InnerMsg, MatchResponse, OngoingMatch, OuterMsg, User, WaitQueue,
};

pub type Tx = UnboundedSender<Message>;
// pub type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

pub async fn handle_connection(
    // peer_map: PeerMap,
    raw_stream: TcpStream,
    addr: SocketAddr,
    wait_queue: WaitQueue,
    found_queue: FoundQueue,
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

    let mut ongoing_match: Arc<Mutex<OngoingMatch>> = Arc::new(Mutex::new(Default::default()));
    let mut thread_consumer: Arc<Mutex<IncomingUser>> = Arc::new(Mutex::new(Default::default()));
    // web socket
    let broadcast_incoming = incoming.try_for_each(|msg| {
        let mut match_responses: MatchResponse = Default::default();
        let mut sending_message: Message = Message::text("initial text");
        let some = match bincode::deserialize(&msg.into_data()).unwrap() {
            OuterMsg::FindMatch { incoming_user } => {
                let _ = async {
                    let mut tc = thread_consumer.clone();
                    let mut tc = tc.lock().await;
                    *tc = incoming_user.clone();
                    drop(tc);
                };

                let _ = async {
                    match_responses = find_match(
                        User {
                            address: incoming_user.wallet_address.clone(),
                            result: vec![],
                            thread_tx: Some(tx.clone()),
                        },
                        incoming_user.entrance_amount,
                        wait_queue.clone(),
                        found_queue.clone(),
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
                        sending_message = Message::text(format!("{:?}", users[0]));
                        let _ = async {
                            let omc = ongoing_match.clone();
                            let mut omc = omc.lock().await;

                            *omc = OngoingMatch {
                                reward: incoming_user.entrance_amount,
                                user: users[1].clone(),
                                contestant: users[0].clone(),
                            };

                            // handshaking
                            let _ = omc
                                .contestant
                                .thread_tx
                                .clone()
                                .unwrap()
                                .unbounded_send(
                                    bincode::serialize(&&InnerMsg::Handshake {
                                        user: omc.user.address.clone(),
                                    })
                                    .unwrap(),
                                )
                                .unwrap();
                            drop(omc);
                        };
                        sending_message = Message::text("match found");
                    }
                    MatchResponse::Undefined(msg) => {
                        sending_message = Message::text("undefined behavior accrued !!");
                    }
                }
            }
            OuterMsg::Update { res } => {
                let _ = async {
                    let omc = ongoing_match.clone();
                    let mut omc = omc.lock().await;
                    let user_addr = omc.user.address.clone();

                    let _ = omc.update_result(&user_addr.clone(), res);

                    // checking for done situation
                    if (omc.contestant.result.len() + omc.user.result.len()) == 6_usize {
                        let res: Vec<bool> = omc
                            .user
                            .result
                            .iter()
                            .chain(omc.contestant.result.iter())
                            .cloned()
                            .collect(); // sending inner done message
                        omc.contestant
                            .thread_tx
                            .as_mut()
                            .unwrap()
                            .unbounded_send(
                                bincode::serialize(&InnerMsg::Done { res: res.clone() }).unwrap(),
                            )
                            .unwrap();

                        outgoing_multi
                            .clone()
                            .lock()
                            .await
                            .send(Message::binary(
                                res.iter().map(|&b| b as u8).collect::<Vec<u8>>(),
                            ))
                            .await
                            .unwrap();
                        // Ensure all messages are sent before shutting down
                        outgoing_multi.clone().lock().await.close().await.unwrap();

                        todo!("now must gracefully shutdown the thread and send the finish message")
                    }
                    if omc.user.result.len() == 3_usize {
                        let res: Vec<bool> = omc.user.result.clone(); // sending inner done message
                        omc.contestant
                            .thread_tx
                            .as_mut()
                            .unwrap()
                            .unbounded_send(bincode::serialize(&InnerMsg::Done { res }).unwrap())
                            .unwrap();
                    }
                    if omc.contestant.result.len() == omc.user.result.len() {
                        sending_message = Message::text("next_round")
                    } else {
                        omc.contestant
                            .thread_tx
                            .as_mut()
                            .unwrap()
                            .unbounded_send(bincode::serialize(&InnerMsg::Update { res }).unwrap())
                            .unwrap();
                    }

                    drop(omc)
                };
            }
        };

        let _ = async {
            if outgoing_multi
                .clone()
                .lock()
                .await
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
    let receive_from_others = rx.map(|msg| {
        // in here we got the update result from the contestant operating thread. we will notify the client to go to the next round if not completed the match yet
        // and we will send a done message and the array of six specifying the results of the contestant and the current user.

            match bincode::deserialize(&msg).unwrap() {
                InnerMsg::Fetch {} => {
                    let _ = async {
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().await;
                        let tc = thread_consumer.clone();
                        let mut tc = tc.lock().await;
                        let fq = found_queue.clone();
                        let mut fq = fq.lock().await;

                        // means we are the fetched one we have to update our states and we need to wait for a handshake init message
                        let opponent_data = fq.get(&tc.wallet_address).unwrap();

                        *omc = OngoingMatch {
                            reward: tc.entrance_amount,
                            user: User {
                                address: tc.wallet_address.clone(),
                                result: vec![],
                                thread_tx: Some(tx.clone()),
                            },
                            contestant: opponent_data.clone(),
                        };

                        outgoing_multi
                            .clone()
                            .lock()
                            .await
                            .send(Message::text("match found"))
                            .await
                            .unwrap();

                        drop(omc);
                        drop(tc);
                        drop(fq);
                    };
                    future::ok::<Message, Error>(Message::text("string"))
                }
                InnerMsg::Update { res } => {
                    let _ = async {
                        // updating our contestant state
                        // our state will be updated through the ws stream message
                        // if our state was updated we tell the user to the next round and if not we do not do anything and the user will be announced through the ws stream handle
                        // letting the client go to the next round
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().await;

                        // the done situation must be handled using a different message
                        // updating the contestant result
                        let con = omc.contestant.address.clone();
                        if !omc.update_result(&con, res) {
                            panic!("couldn't update the contestant result");
                        };

                        if omc.contestant.result.len() == omc.user.result.len() {
                            outgoing_multi
                                .clone()
                                .lock()
                                .await
                                .send(Message::text("next_round"))
                                .await
                                .unwrap();
                        }
                    };
                    future::ok(Message::text("string"))
                }
                InnerMsg::Done { res } => {
                    let _ = async {
                        // updating our state
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().await;
                        if res.len() == 3_usize {
                            // the done situation must be handled using a different message
                            // updating the contestant result
                            let con = omc.contestant.address.clone();
                            if !omc.update_result(&con, res[2]) {
                                panic!("couldn't update the contestant result");
                            };
                            if omc.user.result != omc.contestant.result {
                                panic!("results different !");
                            }
                        } else {
                            // its 6, checking with our result and sending out the result
                            let thread_res: Vec<bool> = omc
                                .user
                                .result
                                .iter()
                                .chain(omc.contestant.result.iter())
                                .cloned()
                                .collect(); // sending inner done message

                            if res != thread_res {
                                panic!("results different !");
                            }

                            outgoing_multi
                                .clone()
                                .lock()
                                .await
                                .send(Message::binary(
                                    res.iter().map(|&b| b as u8).collect::<Vec<u8>>(),
                                ))
                                .await
                                .unwrap();

                            outgoing_multi.clone().lock().await.close().await.unwrap();
                        }
                    };
                    future::ok(Message::text("string"))
                }
                InnerMsg::Handshake { user } => {
                    // we must check the user pass in the message with the one set in our state
                    // we must send back the handshake stablish in case of success
                    // we must send a stream message to start the game
                    let _ = async {
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().await;
                        if omc.user.address != user {
                            // ping
                            omc.contestant
                                .thread_tx
                                .as_mut()
                                .unwrap()
                                .unbounded_send(
                                    bincode::serialize(&&InnerMsg::Handshake {
                                        user: user.clone(),
                                    })
                                    .unwrap(),
                                )
                                .unwrap();
                        }
                        // pong
                        outgoing_multi
                            .clone()
                            .lock()
                            .await
                            .send(Message::binary(
                                bincode::serialize(&IncomingUser {
                                    wallet_address: omc.contestant.address.clone(),
                                    entrance_amount: omc.reward,
                                })
                                .unwrap(),
                            ))
                            .await
                            .unwrap();

                        // deleting from the found queue
                        let fq = found_queue.clone();
                        let mut fq = fq.lock().await;
                        let _ = fq.remove(&omc.user.address).unwrap();
                    };

                    future::ok(Message::text("string"))
                }
            }
        // Ok(Message::text("asfdassa"))
    });

    pin_mut!(broadcast_incoming, receive_from_others);
    future::select(broadcast_incoming, future::ready(receive_from_others)).await;

    println!("{} disconnected", &addr);
    // peer_map.lock().unwrap().remove(&addr);
}

pub async fn find_match(
    _user: User,
    _entrance_tokens: i32,
    wait_queue: WaitQueue,
    found_queue: FoundQueue,
) -> MatchResponse {
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
                fq.insert(
                    matched_user.address.clone(),
                    User {
                        address: _user.address.clone(),
                        result: _user.result.clone(),
                        thread_tx: _user.thread_tx.clone(),
                    },
                );

                let _ = matched_user
                    .clone()
                    .thread_tx
                    .unwrap()
                    .unbounded_send(bincode::serialize(&InnerMsg::Fetch {}).unwrap())
                    .unwrap();
                MatchResponse::FoundMatch(vec![matched_user.clone(), _user.clone()])
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
                fq.insert(
                    matched_user.address.clone(),
                    User {
                        address: _user.address.clone(),
                        result: _user.result.clone(),
                        thread_tx: _user.thread_tx.clone(),
                    },
                );

                let _ = matched_user
                    .clone()
                    .thread_tx
                    .unwrap()
                    .unbounded_send(bincode::serialize(&InnerMsg::Fetch {}).unwrap())
                    .unwrap();
                MatchResponse::FoundMatch(vec![matched_user.clone(), _user.clone()])
            } else {
                panic!("impossible panic !!")
            }
        }
    }
}
