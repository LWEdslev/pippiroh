use std::{fmt, str::FromStr, time::Duration};

use actix::{Actor, Addr, Context, Handler, Message, Recipient};
use clap::Parser;
use futures_lite::StreamExt;
use iroh::{Endpoint, NodeAddr, NodeId, protocol::Router};
use iroh_gossip::{
    ALPN,
    api::{GossipReceiver, GossipSender},
    net::Gossip,
    proto::TopicId,
};
use pippiroh::{p2p::{GotMessage, P2PActor, P2PTicket, SendMessage, Subscribe}, MessageBody};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
};

#[derive(Parser, Debug)]
struct Args {
    #[clap(short, long, default_value = "0")]
    bind_port: u16,
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser, Debug)]
enum Command {
    Open,
    Join {
        ticket: String,
    },
}

#[actix::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let (topic, nodes) = match &args.command {
        Command::Open => {
            let topic = TopicId::from_bytes(rand::random());
            println!("> opening chat room for topic {topic}");
            (topic, vec![])
        }
        Command::Join { ticket } => {
            let P2PTicket { topic, nodes } = P2PTicket::from_str(ticket)?;
            println!("> joining chat room for topic {topic}");
            (topic, nodes)
        }
    };
    let endpoint = Endpoint::builder().discovery_n0().bind().await.unwrap();
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(ALPN, gossip.clone())
        .spawn();

    let ticket = {
        let me = endpoint.node_addr();
        let nodes = vec![me];
        P2PTicket { topic, nodes }
    };
    println!("> ticket: {ticket}");

    let p2p = P2PActor::new().start();
    P2PActor::start_listener(
        p2p.clone(),
        gossip,
        topic,
        nodes,
    )
    .await;

    let printer = PrintActor.start();
    p2p.do_send(Subscribe(printer.recipient()));

    tokio::time::sleep(Duration::from_millis(10)).await;
    println!("Write something:");

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let message = MessageBody {
            from: Some(endpoint.node_id()),
            text: line,
        };
        p2p.do_send(SendMessage(message));
    }

    tokio::signal::ctrl_c().await.ok();
    router.shutdown().await.ok();
    Ok(())
}


pub struct PrintActor;

impl Actor for PrintActor {
    type Context = Context<Self>;
}

impl Handler<GotMessage> for PrintActor {
    type Result = ();

    fn handle(&mut self, msg: GotMessage, _: &mut Self::Context) -> Self::Result {
        let msg = msg.0;
        println!("\"{}\" from {:?}", msg.text, msg.from)
    }
}



