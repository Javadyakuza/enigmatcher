
use std::{
    io::Error,
    net::SocketAddr,
    sync::Arc,
};

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{
    future::{self}, StreamExt, TryStreamExt,
};
use log::info;
use tokio::net::TcpStream;
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

pub async fn handle_connection(
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

    let (tx, rx) = unbounded();
    let mut rx: futures_channel::mpsc::UnboundedReceiver<Vec<u8>> = rx;
    println!("{:?}", tx);

    let ongoing_match: Arc<tokio::sync::Mutex<OngoingMatch>> = Arc::new(tokio::sync::Mutex::new(Default::default()));
    let thread_consumer: Arc<tokio::sync::Mutex<IncomingUser>> = Arc::new(tokio::sync::Mutex::new(Default::default()));

    let ws_stream = Arc::new(tokio::sync::Mutex::new(ws_stream));

    while let Some(Ok(msg)) = ws_stream.lock().await.next().await {
        let ongoing_match = ongoing_match.clone();
        let wait_queue = wait_queue.clone();
        let found_queue = found_queue.clone();
        let tx = tx.clone();
        let ws_stream = ws_stream.clone();

        tokio::spawn(async move {
            let mut match_responses: MatchResponse = Default::default();
            let mut sending_message: Message = Message::text("initial text");

            match serde_json::from_str(&msg.into_text().unwrap()).unwrap() {
                OutcomingMsg::FindMatch { incoming_user } => {
                    println!("finding match for {:?}", incoming_user);
                    match_responses = find_match(
                        User {
                            address: incoming_user.wallet_address.clone(),
                            result: vec![],
                            thread_tx: Some(tx.clone()),
                        },
                        incoming_user.entrance_amount,
                        wait_queue.clone(),
                        found_queue.clone(),
                    ).await;
                    println!("result {:?}", match_responses);

                    match match_responses {
                        MatchResponse::Added(_) => {
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
                            let mut omc = ongoing_match.lock().await;

                            *omc = OngoingMatch {
                                reward: incoming_user.entrance_amount,
                                user: users[1].clone(),
                                contestant: users[0].clone(),
                            };

                            omc.contestant.thread_tx.clone().unwrap().unbounded_send(
                                bincode::serialize(&InnerMsg::Handshake {
                                    user: omc.user.address.clone(),
                                })
                                .unwrap(),
                            ).unwrap();
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
                    let mut omc = ongoing_match.lock().await;
                    let user_addr = omc.user.address.clone();

                    omc.update_result(&user_addr.clone(), res);

                    if (omc.contestant.result.len() + omc.user.result.len()) == 6_usize {
                        let res: Vec<bool> = omc
                            .user
                            .result
                            .iter()
                            .chain(omc.contestant.result.iter())
                            .cloned()
                            .collect();

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

                        ws_stream.lock().await.send(Message::text(serde_json::to_string(&res).unwrap())).await.unwrap();

                        ws_stream.lock().await.close(Some(CloseFrame{
                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Protocol,
                            reason: "game finished".into(),
                        })).await.unwrap();

                        todo!("now must gracefully shutdown the thread and send the finish message")
                    }
                    if omc.user.result.len() == 3_usize {
                        let res: Vec<bool> = omc.user.result.clone();
                        omc.contestant
                            .thread_tx
                            .as_mut()
                            .unwrap()
                            .unbounded_send(bincode::serialize(&InnerMsg::Done { res }).unwrap())
                            .unwrap();
                    }
                    if omc.contestant.result.len() == omc.user.result.len() {
                        sending_message = Message::text(
                            serde_json::to_string(&OutgoingMsg::NextRound {}).unwrap(),
                        )
                    } else {
                        omc.contestant
                            .thread_tx
                            .as_mut()
                            .unwrap()
                            .unbounded_send(bincode::serialize(&InnerMsg::Update { res }).unwrap())
                            .unwrap();
                    }

                    drop(omc);
                }
            };

            if ws_stream.lock().await.send(sending_message).await.is_err() {
                println!("Error sending message to WebSocket");
            }
        });
    }

    while let Some(Ok(msg)) = rx.next().await {
        let ongoing_match = ongoing_match.clone();
        let found_queue = found_queue.clone();
        let tx = tx.clone();
        let ws_stream = ws_stream.clone();

        tokio::spawn(async move {
            match bincode::deserialize(&msg).unwrap() {
                InnerMsg::Fetch {} => {
                    println!("fetching");
                    let mut omc = ongoing_match.lock().await;
                    let tc = thread_consumer.lock().await;
                    let fq = found_queue.lock().await;

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

                    ws_stream.lock().await.send(Message::text(
                        serde_json::to_string(&OutgoingMsg::FoundMatch {
                            user: omc.contestant.address.clone(),
                        })
                        .unwrap(),
                    )).await.unwrap();

                    drop(omc);
                    drop(tc);
                    drop(fq);
                }
                InnerMsg::Update { res } => {
                    let mut omc = ongoing_match.lock().await;

                    let con = omc.contestant.address.clone();
                    if !omc.update_result(&con, res) {
                        panic!("couldn't update the contestant result");
                    };

                    if omc.contestant.result.len() == omc.user.result.len() {
                        ws_stream.lock().await.send(Message::text(
                            serde_json::to_string(&OutgoingMsg::NextRound {}).unwrap(),
                        )).await.unwrap();
                    }
                }
                InnerMsg::Done { res } => {
                    let mut omc = ongoing_match.lock().await;
                    if res.len() == 3_usize {
                        let con = omc.contestant.address.clone();
                        if !omc.update_result(&con, res[2]) {
                            panic!("couldn't update the contestant result");
                        };
                        if omc.user.result != omc.contestant.result {
                            panic!("results different !");
                        }
                    } else {
                        let thread_res: Vec<bool> = omc
                            .user
                            .result
                            .iter()
                            .chain(omc.contestant.result.iter())
                            .cloned()
                            .collect();

                        if res != thread_res {
                            panic!("results different !");
                        }
                        let res = OutgoingMsg::FinishMatchWithResults {
                            user: omc.user.result.clone(),
                            contestant: omc.contestant.result.clone(),
                        };

                        ws_stream.lock().await.send(Message::text(serde_json::to_string(&res).unwrap())).await.unwrap();

                        ws_stream.lock().await.close(Some(CloseFrame{
                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Protocol,
                            reason: "game finished".into(),
                       

 })).await.unwrap();
                    }
                }
                InnerMsg::Handshake { user } => {
                    println!("handshaking");
                    let mut omc = ongoing_match.lock().await;
                    if omc.user.address != user {
                        omc.contestant
                            .thread_tx
                            .as_mut()
                            .unwrap()
                            .unbounded_send(
                                bincode::serialize(&InnerMsg::Handshake { user: user.clone() })
                                    .unwrap(),
                            )
                            .unwrap();
                    }

                    ws_stream.lock().await.send(Message::text(
                        serde_json::to_string(&OutgoingMsg::Start {}).unwrap(),
                    )).await.unwrap();

                    let mut fq = found_queue.lock().await;
                    fq.remove(&omc.user.address).unwrap();
                }
            }
        });
    }

    println!("{} disconnected", &addr);
    Ok(())
}

pub async fn find_match(
    _user: User,
    _entrance_tokens: i32,
    wait_queue: WaitQueue,
    found_queue: FoundQueue,
) -> MatchResponse {
    let mut _q = wait_queue.lock().await;
    let users = _q.entry(_entrance_tokens).or_insert_with(Vec::new);

    match users.len() {
        0 => {
            users.push(_user);
            MatchResponse::Added(users.len() - 1)
        }
        1 => {
            if users[0].address == _user.address {
                MatchResponse::Wait("No one found, wait ...".into())
            } else {
                let matched_user = users.pop().unwrap();
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
                matched_user
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
                matched_user
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
