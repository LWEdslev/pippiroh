use std::{io::Write, str::FromStr, time::Duration};

use actix::{Actor, Addr, Context, Handler, Message};
use clap::Parser;
use iroh::{
    Endpoint, NodeAddr, protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_gossip::{
    net::Gossip,
    proto::TopicId,
};
use pippiroh::{
    MessageBody,
    p2p::{BINCODE_CONFIG, GotMessage, P2PActor, P2PTicket, SendMessage, Subscribe},
};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser, Debug)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser, Debug)]
enum Command {
    Open,
    Join { ticket: String },
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
            let P2PTicket { topic, nodes, bootstrap_node: _ } = P2PTicket::from_str(ticket)?;
            println!("> joining chat room for topic {topic}");
            (topic, nodes)
        }
    };
    let bootstarp_endpoint = Endpoint::builder()
        .discovery_n0()
        .bind()
        .await
        .unwrap();

    let gossip_endpoint = Endpoint::builder()
        .discovery_n0()
        .secret_key(bootstarp_endpoint.secret_key().clone())
        .bind()
        .await
        .unwrap();

    let history = match &args.command {
        Command::Open => vec![],
        Command::Join { ticket } => {
            let P2PTicket { topic: _, nodes: _, bootstrap_node } = P2PTicket::from_str(ticket)?;
            let addr = bootstrap_node;
            let history = Bootstrap::download(&bootstarp_endpoint, addr).await.unwrap();
            println!("-- Historic messages START --");
            history.iter().for_each(|m| m.log());
            println!("-- Historic messages END --");
            std::io::stdout().flush().unwrap();
            history
        }
    };

    tokio::time::sleep(Duration::from_millis(10)).await;
    let ticket = {
        let me = gossip_endpoint.node_addr();
        let bootstrap_node = bootstarp_endpoint.node_addr();
        let nodes = vec![me];
        P2PTicket { topic, nodes, bootstrap_node }
    };
    println!("> ticket: {ticket}");
    std::io::stdout().flush().unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let printer = PrintActor::new(history).start();

    let bootstrap = Bootstrap {
        store: printer.clone(),
    };
    let bootstrap_router = Router::builder(bootstarp_endpoint.clone())
        .accept(BOOTSTRAP_ALPN, bootstrap)
        .spawn();

    let gossip = Gossip::builder().spawn(gossip_endpoint.clone());
    let router = Router::builder(gossip_endpoint.clone())
        .accept(iroh_gossip::ALPN, gossip.clone())
        .spawn();

    let p2p = P2PActor::new().start();
    P2PActor::start_listener(p2p.clone(), gossip, topic, nodes).await;
    p2p.do_send(Subscribe(printer.clone().recipient()));

    println!("Write something:");
    std::io::stdout().flush().unwrap();
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let message = MessageBody {
            from: Some(gossip_endpoint.node_id()),
            text: line,
        };
        p2p.do_send(SendMessage(message.clone()));
        p2p.do_send(GotMessage(message));

    }

    tokio::signal::ctrl_c().await.ok();
    bootstrap_router.shutdown().await.ok();
    router.shutdown().await.ok();
    Ok(())
}

pub struct PrintActor {
    pub history: Vec<MessageBody>,
}

impl PrintActor {
    pub fn new(history: Vec<MessageBody>) -> Self {
        Self { history }
    }
}

impl Actor for PrintActor {
    type Context = Context<Self>;
}

impl Handler<GotMessage> for PrintActor {
    type Result = ();

    fn handle(&mut self, msg: GotMessage, _: &mut Self::Context) -> Self::Result {
        let msg = msg.0;
        msg.log();
        self.history.push(msg);
    }
}

#[derive(Message, Debug, Clone)]
#[rtype(result = "Vec<MessageBody>")]
pub struct GetHistory;

impl Handler<GetHistory> for PrintActor {
    type Result = Vec<MessageBody>;

    fn handle(&mut self, _: GetHistory, _: &mut Self::Context) -> Self::Result {
        self.history.clone()
    }
}

pub const BOOTSTRAP_ALPN: &[u8] = b"bootstrap/0";

#[derive(Debug, Clone)]
pub struct Bootstrap {
    store: Addr<PrintActor>,
}

impl Bootstrap {
    async fn get_data(&self) -> Vec<MessageBody> {
        self.store
            .send(GetHistory)
            .await
            .expect("unable to get history from printactor")
    }

    pub async fn download(endpoint: &Endpoint, addr: NodeAddr) -> anyhow::Result<Vec<MessageBody>> {
        let conn = endpoint.connect(addr, BOOTSTRAP_ALPN).await?;
        let (_, mut recv) = conn.open_bi().await?;
        let response = recv.read_to_end(512_000).await?;
        let (messages, _) = bincode::serde::decode_from_slice(&response, BINCODE_CONFIG)?;
        Ok(messages)
    }
}

impl ProtocolHandler for Bootstrap {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let data = bincode::serde::encode_to_vec(self.get_data().await, BINCODE_CONFIG)
            .expect("unable to get bootstrap data");
        let (mut send, _) = connection.accept_bi().await?;
        send.write_all(data.as_slice())
            .await
            .map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}
