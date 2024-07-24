pub async fn handle_connection(
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

    let (tx, rx) = unbounded();
    let rx: futures_channel::mpsc::UnboundedReceiver<Vec<u8>> = rx;
    println!("{:?}", tx);

    let (outgoing, incoming) = ws_stream.split();
    let outgoing_multi = Arc::new(Mutex::new(outgoing));

    let ongoing_match: Arc<Mutex<OngoingMatch>> = Arc::new(Mutex::new(Default::default()));
    let thread_consumer: Arc<Mutex<IncomingUser>> = Arc::new(Mutex::new(Default::default()));

    let broadcast_incoming = incoming.try_for_each(|msg| {
        let ongoing_match = ongoing_match.clone();
        let outgoing_multi = outgoing_multi.clone();
        let wait_queue = wait_queue.clone();
        let found_queue = found_queue.clone();
        let tx = tx.clone();

        async move {
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
                        wait_queue,
                        found_queue,
                    );
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
                            let omc = ongoing_match.clone();
                            let mut omc = omc.lock().unwrap();

                            *omc = OngoingMatch {
                                reward: incoming_user.entrance_amount,
                                user: users[1].clone(),
                                contestant: users[0].clone(),
                            };

                            omc.contestant
                                .thread_tx
                                .clone()
                                .unwrap()
                                .unbounded_send(
                                    bincode::serialize(&InnerMsg::Handshake {
                                        user: omc.user.address.clone(),
                                    })
                                    .unwrap(),
                                )
                                .unwrap();

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
                    let omc = ongoing_match.clone();
                    let mut omc = omc.lock().unwrap();
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
                        outgoing_multi
                            .clone()
                            .lock()
                            .unwrap()
                            .send(Message::text(serde_json::to_string(&res).unwrap()))
                            .await
                            .unwrap();

                        outgoing_multi
                            .clone()
                            .lock()
                            .unwrap()
                            .close()
                            .await
                            .unwrap();

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
                }
            };

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

            future::ok(())
        }
    });

    let receive_from_others = rx
        .map(|msg| {
            let ongoing_match = ongoing_match.clone();
            let outgoing_multi = outgoing_multi.clone();
            let found_queue = found_queue.clone();
            let tx = tx.clone();

            async move {
                match bincode::deserialize(&msg).unwrap() {
                    InnerMsg::Fetch {} => {
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().unwrap();
                        let tc = thread_consumer.clone();
                        let tc = tc.lock().unwrap();
                        let fq = found_queue.clone();
                        let fq = fq.lock().unwrap();

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
                            .unwrap()
                            .send(Message::text(
                                serde_json::to_string(&OutgoingMsg::FoundMatch {
                                    user: omc.contestant.address.clone(),
                                })
                                .unwrap(),
                            ))
                            .await
                            .unwrap();
                    }
                    InnerMsg::Update { res } => {
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().unwrap();

                        let con = omc.contestant.address.clone();
                        if !omc.update_result(&con, res) {
                            panic!("couldn't update the contestant result");
                        };

                        if omc.contestant.result.len() == omc.user.result.len() {
                            outgoing_multi
                                .clone()
                                .lock()
                                .unwrap()
                                .send(Message::text(
                                    serde_json::to_string(&OutgoingMsg::NextRound {}).unwrap(),
                                ))
                                .await
                                .unwrap();
                        }
                    }
                    InnerMsg::Done { res } => {
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().unwrap();
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
                            outgoing_multi
                                .clone()
                                .lock()
                                .unwrap()
                                .send(Message::text(serde_json::to_string(&res).unwrap()))
                                .await
                                .unwrap();

                            outgoing_multi
                                .clone()
                                .lock()
                                .unwrap()
                                .close()
                                .await
                                .unwrap();
                        }
                    }
                    InnerMsg::Handshake { user } => {
                        let omc = ongoing_match.clone();
                        let mut omc = omc.lock().unwrap();
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

                        outgoing_multi
                            .clone()
                            .lock()
                            .unwrap()
                            .send(Message::text(
                                serde_json::to_string(&OutgoingMsg::Start {}).unwrap(),
                            ))
                            .await
                            .unwrap();

                        let fq = found_queue.clone();
                        let mut fq = fq.lock().unwrap();
                        fq.remove(&omc.user.address).unwrap();
                    }
                }
            }
        })
        .into_future();

    pin_mut!(broadcast_incoming, receive_from_others);
    future::select(broadcast_incoming, receive_from_others).await;

    println!("{} disconnected", &addr);
}
