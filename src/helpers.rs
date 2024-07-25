use std::{io::Error, net::SocketAddr, sync::Arc};

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{
    future::{self},
    pin_mut, FutureExt, SinkExt, StreamExt, TryFutureExt, TryStreamExt,
};

use log::info;
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    accept_async,
    tungstenite::{protocol::CloseFrame, Result},
};

use crate::models::{
    FoundQueue, IncomingUser, InnerMsg, MatchResponse, OngoingMatch, OutcomingMsg, OutgoingMsg,
    User, WaitQueue,
};

pub type Tx = UnboundedSender<Message>;
// pub type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

pub async fn handle_connection(
    // peer_map: PeerMap,
    raw_stream: TcpStream,
    addr: SocketAddr,
    wait_queue: WaitQueue,
    found_queue: FoundQueue,
) -> Result<()> {
    println!("Incoming TCP connection from: {}", addr);

    let mut ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("WebSocket connection established: {}", addr);
    // let ws_stream = Arc::new(Mutex::new(ws_stream));
    let (mut outgoing, mut incoming) = ws_stream.split();
    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded();
    // peer_map.lock().await.insert(addr, tx);
    let mut rx: futures_channel::mpsc::UnboundedReceiver<Vec<u8>> = rx;
    println!("{:?}", tx);

    // let (outgoing, mut incoming) = ws_stream.split();
    // let ws_stream = Arc::new(Mutex::new(outgoing));

    let ongoing_match: Arc<Mutex<OngoingMatch>> = Arc::new(Mutex::new(Default::default()));
    let thread_consumer: Arc<Mutex<IncomingUser>> = Arc::new(Mutex::new(Default::default()));
    let outgoing: Arc<
        Mutex<
            futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<TcpStream>, Message>,
        >,
    > = Arc::new(Mutex::new(outgoing));

    let tx_clone = tx.clone();
    let outgoing_clone = outgoing.clone();
    let ongoing_match_clone = ongoing_match.clone();
    let thread_consumer_clone = thread_consumer.clone();
    let found_queue_clone = found_queue.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.next().await {
            // in here we got the update result from the contestant operating thread. we will notify the client to go to the next round if not completed the match yet
            // and we will send a done message and the array of six specifying the results of the contestant and the current user.
            println!("incomming inner msg {:?}", msg);
            match bincode::deserialize(&msg).unwrap() {
                InnerMsg::Fetch {} => {
                    println!("fetching");
                    let omc = ongoing_match.clone();
                    let mut omc = omc.lock().await;
                    let tc = thread_consumer.clone();
                    let tc = tc.lock().await;
                    let fq = found_queue.clone();
                    let fq = fq.lock().await;

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

                    outgoing_clone
                        .clone()
                        .lock()
                        .await
                        .send(Message::text(
                            serde_json::to_string(&OutgoingMsg::FoundMatch {
                                user: omc.contestant.address.clone(),
                            })
                            .unwrap(),
                        ))
                        .await
                        .unwrap();

                    drop(omc);
                    drop(tc);
                    drop(fq);
                }
                InnerMsg::Update { res } => {
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
                        outgoing_clone
                            .clone()
                            .lock()
                            .await
                            .send(Message::text(
                                serde_json::to_string(&OutgoingMsg::NextRound {}).unwrap(),
                            ))
                            .await
                            .unwrap();
                    }
                }
                InnerMsg::Done { res } => {
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
                        let res = OutgoingMsg::FinishMatchWithResults {
                            user: omc.user.result.clone(),
                            contestant: omc.contestant.result.clone(),
                        };
                        outgoing_clone
                            .clone()
                            .lock()
                            .await
                            .send(Message::text(serde_json::to_string(&res).unwrap()))
                            .await
                            .unwrap();

                        outgoing_clone
                            .clone()
                            .lock()
                            .await
                            .close(
                            //     Some(CloseFrame{
                            //     code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Protocol,
                            //     reason: "game finished".into(),
                            // })
                        )
                                .await
                                .unwrap();
                    }
                }
                InnerMsg::Handshake { user, initiator } => {
                    println!("handshaking");
                    // we must check the user pass in the message with the one set in our state
                    // we must send back the handshake stablish in case of success
                    // we must send a stream message to start the game
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
                                    initiator: user.clone(),
                                })
                                .unwrap(),
                            )
                            .unwrap();
                    }
                    // pong
                    outgoing_clone
                        .clone()
                        .lock()
                        .await
                        .send(Message::text(
                            serde_json::to_string(&OutgoingMsg::Start {}).unwrap(),
                        ))
                        .await
                        .unwrap();

                    if &initiator == "" {
                        let fq = found_queue.clone();
                        let mut fq = fq.lock().await;
                        let _ = fq.remove(&omc.user.address).unwrap();
                    }
                    // deleting from the found queue
                }
            }
            // Ok(Message::text("asfdassa"))
        }
        // web socket
    });
    while let Some(Ok(msg)) = incoming.next().await {
        let mut match_responses: MatchResponse = Default::default();
        let mut sending_message: Message = Message::text("initial text");

        println!("started the loop");
        // inner platform

        println!("passed the loop");
        match serde_json::from_str(&msg.into_text().unwrap()).unwrap() {
            OutcomingMsg::FindMatch { incoming_user } => {
                let tc = thread_consumer_clone.clone();
                let mut tc = tc.lock().await;
                *tc = incoming_user.clone();
                drop(tc);
                println!("finding match for {:?}", incoming_user);
                match_responses = find_match(
                    User {
                        address: incoming_user.wallet_address.clone(),
                        result: vec![],
                        thread_tx: Some(tx_clone.clone()),
                    },
                    incoming_user.entrance_amount,
                    wait_queue.clone(),
                    found_queue_clone.clone(),
                )
                .await;
                println!("result {:?}", match_responses);

                match match_responses {
                    MatchResponse::Added(_) => {
                        // telling user to wait
                        sending_message = Message::text(
                            serde_json::to_string(&OutgoingMsg::Wait {
                                msg: "added to queue, wait".into(),
                            })
                            .unwrap(),
                        );
                    }
                    MatchResponse::Wait(msg) => {
                        sending_message = Message::text(
                            serde_json::to_string(&OutgoingMsg::Wait { msg }).unwrap(),
                        );
                    }
                    MatchResponse::FoundMatch(users) => {
                        println!("matched the response");
                        let omc = ongoing_match_clone.clone();
                        let mut omc = omc.lock().await;

                        *omc = OngoingMatch {
                            reward: incoming_user.entrance_amount,
                            user: users[1].clone(),
                            contestant: users[0].clone(),
                        };

                        // handshaking
                        let k = omc
                            .contestant
                            .thread_tx
                            .clone()
                            .unwrap()
                            .unbounded_send(
                                bincode::serialize(&&InnerMsg::Handshake {
                                    user: omc.user.address.clone(),
                                    initiator: Default::default(),
                                })
                                .unwrap(),
                            )
                            .unwrap();
                        drop(omc);

                        sending_message = Message::text(
                            serde_json::to_string(&OutgoingMsg::FoundMatch {
                                user: users[0].address.clone(),
                            })
                            .unwrap(),
                        );
                    }
                    MatchResponse::Undefined(_) => {
                        sending_message = Message::text(
                            serde_json::to_string(&OutgoingMsg::Undefined {
                                msg: "undefined behavior".into(),
                            })
                            .unwrap(),
                        );
                    }
                };
            }

            OutcomingMsg::Update { res } => {
                let omc = ongoing_match_clone.clone();
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
                    let res = OutgoingMsg::FinishMatchWithResults {
                        user: omc.user.result.clone(),
                        contestant: omc.contestant.result.clone(),
                    };
                    // ws_stream
                    //     .clone()
                    //     .lock()
                    //     .unwrap()

                    outgoing
                        .clone()
                        .lock()
                        .await
                        .send(Message::text(serde_json::to_string(&res).unwrap()))
                        .await
                        .unwrap();
                    // Ensure all messages are sent before shutting down
                    outgoing
                    .clone()
                    .lock()
                    .await
                        .close(
                        //     Some(CloseFrame{
                        //     code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Protocol,
                        //     reason: "game finished".into(),
                        // })
                    )
                        .await
                        .unwrap();

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
                    sending_message =
                        Message::text(&serde_json::to_string(&OutgoingMsg::NextRound {}).unwrap())
                } else {
                    omc.contestant
                        .thread_tx
                        .as_mut()
                        .unwrap()
                        .unbounded_send(bincode::serialize(&InnerMsg::Update { res }).unwrap())
                        .unwrap();
                }

                drop(omc)
            }
        };

        if outgoing
            .clone()
            .lock()
            .await
            .send(sending_message)
            .await
            .is_err()
        {
            println!("Error sending message to WebSocket");
        }
    }

    // pin_mut!(broadcast_incoming, receive_from_others);
    // future::select(broadcast_incoming, receive_from_others).await;
    println!("{} disconnected", &addr);
    Ok(())
    // peer_map.lock().await.remove(&addr);
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
                println!("sending 1 ");
                let g = matched_user
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
                println!("sending 2");
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
