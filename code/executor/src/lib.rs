#![feature(unsize)]

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::Parser;
use protocol::{ClientPacket, ServerPacket};
use tokio::{
    net::TcpListener,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};
use tokio_tungstenite::accept_async;
use tracing::{error, info};

use crate::process::{Process, WebSocketComm};

mod command;
mod process;

#[derive(Parser)]
pub struct Args {
    #[clap(short, long, default_value = "127.0.0.1")]
    pub ip: String,

    #[clap(short, long, default_value = "8080")]
    pub port: u16,
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected,
}

#[async_trait]
pub trait Comm {
    async fn send(&mut self, packet: ServerPacket) -> Result<()>;
    async fn recv(&mut self) -> Result<ClientPacket>;
}

struct SimpleComm {
    tx: UnboundedSender<ServerPacket>,
    rx: UnboundedReceiver<ClientPacket>,
}

#[async_trait]
impl Comm for SimpleComm {
    async fn send(&mut self, packet: ServerPacket) -> Result<()> {
        self.tx.send(packet)?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<ClientPacket> {
        self.rx.recv().await.context("Failed to receive packet")
    }
}

/// Launch using [`SimpleComm`] and return (tx, rx) for sending and receiving packets.
pub fn launch() -> Result<(
    UnboundedSender<ClientPacket>,
    UnboundedReceiver<ServerPacket>,
)> {
    let executor = Executor::new()?;

    let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
    let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel();

    let comm = SimpleComm { tx: tx1, rx: rx2 };

    tokio::spawn(async move {
        handle_client(executor, comm).await;
    });

    Ok((tx2, rx1))
}

#[must_use]
pub fn launch_websocket(args: Args) -> UnboundedReceiver<Event> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        info!("Starting executor");

        let executor = match Executor::new() {
            Ok(v) => v,
            Err(e) => {
                error!("{e}");
                return;
            }
        };

        let Args { ip, port } = args;

        let addr = format!("{ip}:{port}");

        let listener = match TcpListener::bind(&addr).await {
            Ok(v) => v,
            Err(e) => {
                error!("{e}");
                return;
            }
        };

        if tx.send(Event::Connected).is_err() {
            error!("Failed to send connected event.")
        };

        info!("Listening on: {addr}");

        loop {
            let (socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    error!("{e}");
                    return;
                }
            };

            let ws_stream = match accept_async(socket).await {
                Ok(v) => v,
                Err(e) => {
                    error!("{e}");
                    return;
                }
            };

            info!(
                "New WebSocket connection: {}",
                match ws_stream.get_ref().peer_addr() {
                    Ok(v) => v.to_string(),
                    Err(e) => format!("Failed to get peer address: {e}"),
                }
            );

            let ws = WebSocketComm::new(ws_stream);

            let executor = executor.clone();
            tokio::spawn(async move {
                handle_client(executor, ws).await;
            });
        }
    });

    rx
}

type Ctx = Arc<Inner>;

struct Inner {
    ai: tokio_openai::Client,
    req: reqwest::Client,
}

#[derive(Clone)]
pub struct Executor {
    ctx: Ctx,
}

/// construct a new context
fn ctx() -> Result<Ctx> {
    let inner = Inner {
        ai: tokio_openai::Client::simple()?,
        req: reqwest::Client::new(),
    };

    Ok(Arc::new(inner))
}

impl Executor {
    fn new() -> Result<Self> {
        Ok(Self { ctx: ctx()? })
    }
}

async fn handle_client(executor: Executor, comm: impl Comm + Send) {
    let process = Process::new(executor, comm);

    if let Err(e) = process.run().await {
        error!("Error: {}", e);
    }
}
