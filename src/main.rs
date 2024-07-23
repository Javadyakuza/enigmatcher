use std::{collections::HashMap, env, io::Error, sync::{Arc, Mutex}};

pub mod models;
pub mod helpers;

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};

use helpers::{handle_connection};
use models::{New, WaitQueue, FoundQueue, User};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message;
#[tokio::main]
async fn main() -> Result<(), Error> {
    let addr = env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8080".to_string());

    // let state = PeerMap::new(Mutex::new(HashMap::new()));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket.expect("Failed to bind");
    println!("Listening on: {}", addr);

    let wait_queue: WaitQueue = WaitQueue::new_empty();
    let found_queue: FoundQueue = FoundQueue::new_empty();

    // Let's spawn the handling of each connection in a separate task.
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(stream, addr, wait_queue.clone(), found_queue.clone()));
    }

    Ok(())
}