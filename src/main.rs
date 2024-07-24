use std::{
    env,
    io::Error,
};

pub mod helpers;
pub mod models;


use helpers::handle_connection;
use models::{FoundQueue, IncomingUser, New, OutcomingMsg,  WaitQueue};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());


    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket.expect("Failed to bind");
    println!("Listening on: {}", addr);

    let wait_queue: WaitQueue = WaitQueue::new_empty();
    let found_queue: FoundQueue = FoundQueue::new_empty();
    println!(
        "{:?}{:?}",
        bincode::serialize(&OutcomingMsg::FindMatch {
            incoming_user: IncomingUser {
                wallet_address: "some wallet address".into(),
                entrance_amount: 2
            }
        })
        .unwrap(), 
        bincode::serialize(&r#""OutcomingMsg"::"FindMatch" {
            "incoming_user": "IncomingUser" {
                "wallet_address": "some wallet address",
                "entrance_amount": 2
            }
        }"#)
        .unwrap()
    );
    // Let's spawn the handling of each connection in a separate task.
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(
            stream,
            addr,
            wait_queue.clone(),
            found_queue.clone(),
        ));
    }

    Ok(())
}
