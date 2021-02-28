use futures::prelude::*;
use futures::stream::StreamExt;

use std::io;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_stream::wrappers::ReceiverStream;

use log::info;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use tungstenite::protocol::Message;

type Tx = mpsc::Sender<Message>;
type ClientState = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    env_logger::init();

    let addr = "127.0.0.1:8080";

    let try_socket = TcpListener::bind(addr).await;
    let listener = try_socket.expect("Could not bind");

    info!("Listening on {}", addr);

    let state = ClientState::new(Mutex::new(HashMap::new()));

    loop {
        let (stream, addr) = listener.accept().await.expect("Could not accept client");
        tokio::spawn(handle_client(state.clone(), stream, addr));
    }
}

async fn handle_client(state: ClientState, stream: TcpStream, addr: SocketAddr) {
    info!("Incoming TCP connection from: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .expect("Error during the websocket handshake occurred");

    info!("WebSocket connection established: {}", addr);

    // Get a transmitter, producer pair for this client
    let (tx, rx) = mpsc::channel::<Message>(100);

    state.lock().unwrap().insert(addr, tx);

    let (sender, receiver) = ws_stream.split();

    let ws_receive_future = receiver.for_each(|msg| {
        if let Ok(msg) = msg {
            let clients = state.lock().unwrap();

            let transmiters = clients
                .iter()
                .filter(|(peer_addr, _)| peer_addr != &&addr)
                .map(|(_, ws_sink)| ws_sink);

            for t in transmiters {
                t.try_send(msg.clone()).unwrap();
            }
        }

        future::ready(())
    });

    let rx_stream = ReceiverStream::new(rx);
    let channel_receive_future = rx_stream.map(Ok).forward(sender);

    future::select(ws_receive_future, channel_receive_future).await;
}
