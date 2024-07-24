use std::{
    collections::HashMap,
    env,
};

pub mod helpers;
pub mod models;

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};

use helpers::handle_connection;
use models::{FoundQueue, IncomingUser, New, User, WaitQueue};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message;
#[tokio::main]
async fn main() -> Result<(), Error> {
    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());

    // let state = PeerMap::new(Mutex::new(HashMap::new()));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket.expect("Failed to bind");
    println!("Listening on: {}", addr);

    let wait_queue: WaitQueue = WaitQueue::new_empty();
    let found_queue: FoundQueue = FoundQueue::new_empty();

    // Let's spawn the handling of each connection in a separate task.
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(accept_connection(
            stream,
            addr,
            wait_queue.clone(),
            found_queue.clone(),
        ));
    }

    Ok(())
}

use futures_util::SinkExt;
use log::*;
use std::net::SocketAddr;
use tokio_tungstenite::{
    accept_async,
    tungstenite::{Error, Result},
};

async fn accept_connection(
    stream: TcpStream,
    addr: SocketAddr,
    wait_queue: WaitQueue,
    found_queue: FoundQueue,
) {
    if let Err(e) = handle_connection(stream, addr, wait_queue, found_queue).await {
        match e {
            Error::ConnectionClosed | Error::Protocol(_) | Error::Utf8 => (),
            err => error!("Error processing connection: {}", err),
        }
    }
}

// #[tokio::main]
// async fn main() {
//     env_logger::init();

//     let addr = "127.0.0.1:9002";
//     let listener = TcpListener::bind(&addr).await.expect("Can't listen");
//     info!("Listening on: {}", addr);

//     while let Ok((stream, _)) = listener.accept().await {
//         let peer = stream
//             .peer_addr()
//             .expect("connected streams should have a peer address");
//         info!("Peer address: {}", peer);

//         tokio::spawn(accept_connection(peer, stream));
//     }
// }
