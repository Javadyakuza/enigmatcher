// use rocket::tokio::net::TcpListener;
// use rocket::tokio::task;
// use tokio_tungstenite::accept_async;
// use tokio_tungstenite::tungstenite::protocol::Message;
// use std::collections::HashMap;
// use std::sync::{Arc, Mutex};

// type Clients = Arc<Mutex<HashMap<String, tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>>>;
// type MatchmakingQueue = Arc<Mutex<Vec<String>>>;

// async fn handle_connection(
//     raw_stream: tokio::net::TcpStream,
//     // following argumnets are the platforms state
//     clients: Clients, 
//     queue: MatchmakingQueue,
// ) {
//     let ws_stream = accept_async(raw_stream)
//         .await
//         .expect("Error during the websocket handshake");

//     // For simplicity, we assume the wallet address is sent as the first message
//     let wallet_address = ws_stream.get_ref();
//     //  {
//     //     Some(Ok(Message::Text(wallet_address))) => wallet_address,
//     //     _ => return, // Handle error or invalid connection
//     // };

//     clients.lock().unwrap().insert(wallet_address.clone(), ws_stream);

//     {
//         let mut queue = queue.lock().unwrap();
//         queue.push(wallet_address.clone());
//         if queue.len() >= 2 {
//             // Matchmaking logic: pair the first two clients in the queue
//             let client1 = queue.remove(0);
//             let client2 = queue.remove(0);

//             // Notify clients they have been matched (you can extend this)
//             clients.lock().unwrap().get_mut(&client1).unwrap().send(Message::Text("Matched!".to_string())).await.unwrap();
//             clients.lock().unwrap().get_mut(&client2).unwrap().send(Message::Text("Matched!".to_string())).await.unwrap();
//         }
//     }

//     // Handle incoming messages from clients here
//     loop {
//         let message = clients
//             .lock()
//             .unwrap()
//             .get_mut(&wallet_address)
//             .unwrap()
//             .next()
//             .await;

//         match message {
//             Some(Ok(msg)) => {
//                 if msg.is_text() {
//                     println!("Received a message: {}", msg.to_text().unwrap());
//                 }
//             }
//             _ => break,
//         }
//     }

//     clients.lock().unwrap().remove(&wallet_address);
// }

// pub async fn run_websocket_server() {
//     let addr = "127.0.0.1:9000";
//     let listener = TcpListener::bind(&addr).await.expect("Can't bind to address");

//     let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
//     let queue: MatchmakingQueue = Arc::new(Mutex::new(Vec::new()));

//     while let Ok((stream, _)) = listener.accept().await {
//         let clients = clients.clone();
//         let queue = queue.clone();
//         task::spawn(async move {
//             handle_connection(stream, clients, queue).await;
//         });
//     }
// }

//! A chat server that broadcasts a message to all connections.
//!
//! This is a simple line-based server which accepts WebSocket connections,
//! reads lines from those connections, and broadcasts the lines to all other
//! connected clients.
//!
//! You can test this out by running:
//!
//!     cargo run --example server 127.0.0.1:12345
//!
//! And then in another window run:
//!
//!     cargo run --example client ws://127.0.0.1:12345/
//!
//! You can run the second command in multiple windows and then chat between the
//! two, seeing the messages from the other client as they're received. For all
//! connected clients they'll all join the same room and see everyone else's
//! messages.

use std::{
    collections::HashMap,
    env,
    io::Error as IoError,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};

use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message;

type Tx = UnboundedSender<Message>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

// @param peer map is the state of the platform, it will store the send and the recieve handle of the msco channels of the websocket strea
// @param raw_stream is the actual ws stream transcation 
// @param addr is the address that the request is being sent from
async fn handle_connection(peer_map: PeerMap, raw_stream: TcpStream, addr: SocketAddr) {

    println!("Incoming TCP connection from: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("WebSocket connection established: {}", addr);

    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded();
    peer_map.lock().unwrap().insert(addr, tx);

    let (outgoing, incoming) = ws_stream.split();

    let broadcast_incoming = incoming.try_for_each(|msg| {
        println!("Received a message from {}: {}", addr, msg.to_text().unwrap());
        let peers = peer_map.lock().unwrap();

        // We want to broadcast the message to everyone except ourselves.
        let broadcast_recipients =
            peers.iter().filter(|(peer_addr, _)| peer_addr != &&addr).map(|(_, ws_sink)| ws_sink);

        for recp in broadcast_recipients {
            recp.unbounded_send(msg.clone()).unwrap();
        }

        future::ok(())
    });

    let receive_from_others = rx.map(Ok).forward(outgoing);

    pin_mut!(broadcast_incoming, receive_from_others);
    future::select(broadcast_incoming, receive_from_others).await;

    println!("{} disconnected", &addr);
    peer_map.lock().unwrap().remove(&addr);
}

#[tokio::main]
async fn main() -> Result<(), IoError> {
    let addr = env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8080".to_string());

    let state = PeerMap::new(Mutex::new(HashMap::new()));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket.expect("Failed to bind");
    println!("Listening on: {}", addr);

    // Let's spawn the handling of each connection in a separate task.
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(state.clone(), stream, addr));
    }

    Ok(())
}