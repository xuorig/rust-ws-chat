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
type RedisRx = mpsc::Receiver<String>;
type RedisTx = mpsc::Sender<String>;
type ClientState = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

use redis::AsyncCommands;

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    env_logger::init();

    let addr = "127.0.0.1:8080";

    let state = ClientState::new(Mutex::new(HashMap::new()));

    let redis = redis::Client::open("redis://localhost:6379").expect("Error connecting to redis");

    let (tx, rx) = mpsc::channel::<String>(100);

    tokio::spawn(redis_subscriber(redis.clone(), state.clone()));
    tokio::spawn(redis_publisher(redis.clone(), rx));

    let try_socket = TcpListener::bind(addr).await;
    let listener = try_socket.expect("Could not bind");

    info!("Listening on {}", addr);

    loop {
        let (stream, addr) = listener.accept().await.expect("Could not accept client");
        tokio::spawn(handle_client(state.clone(), tx.clone(), stream, addr));
    }
}

async fn redis_publisher(client: redis::Client, rx: RedisRx) {
    let mut publish_conn = client
        .get_async_connection()
        .await
        .expect("Could not connect");

    let mut rx_stream = ReceiverStream::new(rx);
    while let Some(msg) = rx_stream.next().await {
        publish_conn
            .publish::<&str, String, i32>("chat", msg)
            .await
            .expect("Could not publish to chat channel");
    }
    info!("Publisher exiting");
}

async fn redis_subscriber(client: redis::Client, state: ClientState) {
    let mut pubsub_conn = client
        .get_async_connection()
        .await
        .expect("Could not get Redis connection")
        .into_pubsub();

    pubsub_conn
        .subscribe("chat")
        .await
        .expect("Could not subscribe to chat topic");

    let pubsub_stream = pubsub_conn.on_message();

    pubsub_stream
        .for_each(|msg| {
            info!("Redis SUB {:?}", msg);
            let payload: String = msg.get_payload().unwrap();

            let clients = state.lock().unwrap();

            let transmiters = clients.iter().map(|(_, ws_sink)| ws_sink);

            for t in transmiters {
                info!("Got {} in redis pub/sub, sending to channel!", payload);
                t.try_send(Message::Text(payload.clone())).unwrap();
            }

            future::ready(())
        })
        .await;

    info!("Subscriber exiting");
}

async fn handle_client(
    state: ClientState,
    redis_notifier_tx: RedisTx,
    stream: TcpStream,
    addr: SocketAddr,
) {
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
            let text = msg.into_text().unwrap();
            info!("Sending {} to redis channel", text);
            redis_notifier_tx.try_send(text).unwrap();
        }

        future::ready(())
    });

    let rx_stream = ReceiverStream::new(rx);
    let channel_receive_future = rx_stream.map(Ok).forward(sender);

    future::select(ws_receive_future, channel_receive_future).await;
}
