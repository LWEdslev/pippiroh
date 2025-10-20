use actix::{Actor, Addr, Context, Handler, Message, Recipient};
use futures_lite::StreamExt;
use iroh::NodeAddr;
use iroh_gossip::{api::{GossipReceiver, GossipSender}, net::Gossip, proto::TopicId};
use serde::{Deserialize, Serialize};

use crate::MessageBody;

#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct GotMessage(pub MessageBody);

#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct SendMessage(pub MessageBody);

#[derive(Message)]
#[rtype(result = "()")]
pub struct Subscribe(pub Recipient<GotMessage>);

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

    pub async fn start_listener(
        addr: Addr<Self>,
        gossip: Gossip,
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
            .for_each(|s| s.do_send(msg.clone()));
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
pub struct P2PTicket {
    pub topic: TopicId,
    pub nodes: Vec<NodeAddr>,
}

impl P2PTicket {
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

impl std::fmt::Display for P2PTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut text = data_encoding::BASE32_NOPAD.encode(&self.to_bytes()[..]);
        text.make_ascii_lowercase();
        write!(f, "{}", text)
    }
}

impl std::str::FromStr for P2PTicket {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::BASE32_NOPAD.decode(s.to_ascii_uppercase().as_bytes())?;
        Self::from_bytes(&bytes)
    }
}
