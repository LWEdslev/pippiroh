use iroh::NodeId;
use serde::{Deserialize, Serialize};

pub mod p2p;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBody {
    pub from: Option<NodeId>,
    pub text: String,
}