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
    /// Open a chat room for a topic and print a ticket for others to join.
    Open,
    /// Join a chat room from a ticket.
    Join {
        /// The ticket, as base32 string.
        ticket: String,
    },
}

#[actix_rt::main]
async fn main() -> anyhow::Result<()> {
    //let system = actix::prelude::System::new();
    let args = Args::parse();

    let (topic, nodes) = match &args.command {
        Command::Open => {
            let topic = TopicId::from_bytes(rand::random());
            println!("> opening chat room for topic {topic}");
            (topic, vec![])
        }
        Command::Join { ticket } => {
            let Ticket { topic, nodes } = Ticket::from_str(ticket)?;
            println!("> joining chat room for topic {topic}");
            (topic, nodes)
        }
    };
    // Create Actix system manually for multi-threaded runtime

    //let execution = async move {
    let endpoint = Endpoint::builder().discovery_n0().bind().await.unwrap();
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(ALPN, gossip.clone())
        .spawn();

    let ticket = {
        let me = endpoint.node_addr();
        let nodes = vec![me];
        Ticket { topic, nodes }
    };
    println!("> ticket: {ticket}");

    let p2p = P2PActor::new().start();
    P2PActor::start_listener(
        p2p.clone(),
        endpoint.clone(),
        gossip,
        router.clone(),
        topic,
        nodes,
    )
    .await;

    let printer = LengthPrintActor.start();
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
pub struct LengthPrintActor;

impl Actor for LengthPrintActor {
    type Context = Context<Self>;
}

impl Handler<GotMessage> for LengthPrintActor {
    type Result = ();

    fn handle(&mut self, msg: GotMessage, _: &mut Self::Context) -> Self::Result {
        let msg = msg.0;
        println!("\"{}\" from {:?}", msg.text, msg.from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MessageBody {
    from: Option<NodeId>,
    text: String,
}

#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct GotMessage(MessageBody);

#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct SendMessage(MessageBody);

#[derive(Message)]
#[rtype(result = "()")]
struct Subscribe(pub Recipient<GotMessage>);

#[derive(Message)]
#[rtype(result = "()")]
struct SetSender(GossipSender);

pub struct P2PActor {
    subscribers: Vec<Recipient<GotMessage>>,
    sender: Option<GossipSender>,
}

impl P2PActor {
    pub fn new() -> Self {
        Self {
            subscribers: Default::default(),
            sender: None,
        }
    }

    pub fn subscribe(&mut self, recipient: Recipient<GotMessage>) {
        self.subscribers.push(recipient);
    }

    async fn start_listener(
        addr: Addr<Self>,
        endpoint: Endpoint,
        gossip: Gossip,
        router: Router,
        topic: TopicId,
        nodes: Vec<NodeAddr>,
    ) {
        let node_ids = nodes.iter().map(|p| p.node_id).collect();
        if nodes.is_empty() {
            println!("> waiting for nodes to join us...");
        } else {
            println!("> trying to connect to {} nodes...", nodes.len());
        };

        let (sender, receiver) = gossip
            .subscribe_and_join(topic, node_ids)
            .await
            .expect("unable to start listener")
            .split();

        addr.do_send(SetSender(sender));

        tokio::spawn(P2PActor::subscribe_loop(addr, receiver));
    }

    async fn subscribe_loop(addr: Addr<Self>, mut receiver: GossipReceiver) {
        loop {
            let Ok(Some(event)) = receiver.try_next().await else {
                continue;
            };
            match event {
                iroh_gossip::api::Event::Received(message) => {
                    let Ok((message, _)) =
                        bincode::serde::decode_from_slice(&message.content, BINCODE_CONFIG)
                    else {
                        continue;
                    };
                    addr.do_send(GotMessage(message));
                }
                _ => continue,
            }
        }
    }
}

impl Handler<Subscribe> for P2PActor {
    type Result = ();

    fn handle(&mut self, msg: Subscribe, _: &mut Context<Self>) {
        self.subscribers.push(msg.0);
    }
}

impl Handler<SetSender> for P2PActor {
    type Result = ();

    fn handle(&mut self, msg: SetSender, _: &mut Self::Context) -> Self::Result {
        self.sender = Some(msg.0)
    }
}

impl Handler<GotMessage> for P2PActor {
    type Result = ();

    fn handle(&mut self, msg: GotMessage, _: &mut Self::Context) -> Self::Result {
        self.subscribers
            .iter()
            .for_each(|s| s.do_send(msg.clone()).unwrap());
    }
}

impl Handler<SendMessage> for P2PActor {
    type Result = ();

    fn handle(&mut self, msg: SendMessage, _: &mut Self::Context) -> Self::Result {
        let sender = self.sender.as_ref().expect("no P2P sender").clone();
        let bytes = bincode::serde::encode_to_vec(msg.0, BINCODE_CONFIG).unwrap();
        tokio::spawn(async move {
            sender.broadcast(bytes.into()).await.unwrap();
        });
    }
}

impl Actor for P2PActor {
    type Context = Context<Self>;
}

pub const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

#[derive(Debug, Serialize, Deserialize)]
struct Ticket {
    topic: TopicId,
    nodes: Vec<NodeAddr>,
}

impl Ticket {
    fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        bincode::serde::decode_from_slice(bytes, BINCODE_CONFIG)
            .map_err(Into::into)
            .map(|t| t.0)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serde::encode_to_vec(self, BINCODE_CONFIG)
            .expect("bincode::encode_to_vec is infallible")
    }
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut text = data_encoding::BASE32_NOPAD.encode(&self.to_bytes()[..]);
        text.make_ascii_lowercase();
        write!(f, "{}", text)
    }
}

impl FromStr for Ticket {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::BASE32_NOPAD.decode(s.to_ascii_uppercase().as_bytes())?;
        Self::from_bytes(&bytes)
    }
}
