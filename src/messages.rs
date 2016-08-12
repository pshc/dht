use std::{self, io};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use bencode::{Bencode, DictMap, FromBencode, ListVec, ToBencode};
use bencode::Bencode::{ByteString, Dict, List, Number};
use bencode::util::ByteString as Bytes;
use rand;

// ! Primitives

/// The 160-bit space of BitTorrent infohashes.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct NodeId(pub [u8; NODE_ID_LEN]);

pub const NODE_ID_LEN: usize = 20;

impl NodeId {
    pub fn random() -> Self {
        NodeId(rand::random())
    }

    pub fn from_slice(bytes: &[u8]) -> DecodeResult<Self> {
        if bytes.len() == NODE_ID_LEN {
            let mut fixed: [u8; NODE_ID_LEN] = [0; NODE_ID_LEN];
            fixed.copy_from_slice(bytes);
            Ok(NodeId(fixed))
        } else {
            Err(DecodeError::WrongLength)
        }
    }

    pub fn bit(&self, index: usize) -> bool {
        debug_assert!(index < NODE_ID_LEN * 8);
        let mask = 1 << (7 - (index % 8));
        (self.0[index / 8] & mask) != 0
    }
}

impl Debug for NodeId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Node(")?;
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        write!(f, ")")
    }
}

impl FromBencode for NodeId {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        NodeId::from_slice(b.bytes()?)
    }
}

impl ToBencode for NodeId {
    fn to_bencode(&self) -> Bencode {
        ByteString(self.0.to_vec())
    }
}

/// Correlates queries to responses.
///
/// (Should use an optimized SmallVec rather than rolling our own...)
#[derive(Clone, Eq)]
pub enum TxId {
    Short([u8; 2]),
    Arbitrary(Bytes),
}

impl TxId {
    pub fn random() -> Self {
        fn alpha() -> u8 {
            let n = rand::random::<u8>() % 52;
            let c = if n >= 26 { n - 26 + 97 } else { n + 65 };
            debug_assert!(match c as char { 'A'...'Z' | 'a'...'z' => true, _ => false });
            c
        }
        TxId::Short([alpha(), alpha()])
    }

    pub fn as_slice(&self) -> &[u8] {
        match *self {
            TxId::Short(ref two) => two,
            TxId::Arbitrary(ref bytes) => bytes.as_slice(),
        }
    }
}

impl Debug for TxId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Tx(")?;
        for b in self.as_slice() {
            write!(f, "{:02x}", b)?;
        }
        write!(f, ")")
    }
}

impl FromBencode for TxId {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        let bytes = b.bytes()?;
        if bytes.len() == 2 {
            Ok(TxId::Short([bytes[0], bytes[1]]))
        } else {
            Ok(TxId::Arbitrary(Bytes::from_slice(bytes)))
        }
    }
}

impl Hash for TxId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash_slice(self.as_slice(), state)
    }
}

impl PartialEq for TxId {
    fn eq(&self, other: &TxId) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl ToBencode for TxId {
    fn to_bencode(&self) -> Bencode {
        ByteString(self.as_slice().to_vec())
    }
}

// ! bencoded messages

/// Returned by every decoding function.
pub type DecodeResult<T> = Result<T, DecodeError>;

/// Granular reason for a failure to decode.
#[derive(Debug)]
pub enum DecodeError {
    KeyMissing(&'static str),
    InvalidAddress(Ipv4Addr),
    InvalidDiscrim,
    OutOfRange,
    WrongDiscrim,
    WrongLength,
    WrongType,
}

impl Error for DecodeError {
    fn description(&self) -> &str {
        use self::DecodeError::*;
        match *self {
            KeyMissing(_) => "required key missing",
            InvalidAddress(_) => "invalid peer address",
            InvalidDiscrim => "invalid tag",
            OutOfRange => "number out of range",
            WrongDiscrim => "wrong tag",
            WrongLength => "wrong array/value length",
            WrongType => "wrong type",
        }
    }
}

impl Display for DecodeError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match *self {
            DecodeError::KeyMissing(key) => write!(f, "<DecodeError: key {:?} missing>", key),
            DecodeError::InvalidAddress(addr) => write!(f, "<DecodeError: {} invalid>", addr),
            _ => write!(f, "<DecodeError: {}>", self.description())
        }
    }
}

impl From<DecodeError> for io::Error {
    fn from(error: DecodeError) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

/// The requests a node may make of another.
#[derive(Debug)]
pub enum Query {
    Ping,
    FindNode(NodeId),
}

/// The full payload for a `Query`.
#[derive(Debug)]
pub struct FullQuery {
    pub query: Query,
    pub sender_id: NodeId,
    pub tx_id: TxId,
}

impl FromBencode for FullQuery {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        let dict = b.dict()?;
        if dict.lookup("y")?.bytes()? != b"q" {
            return Err(DecodeError::WrongDiscrim)
        }
        let args = dict.lookup("a")?.dict()?;
        let sender_id = NodeId::from_bencode(args.lookup("id")?)?;
        let tx_id = TxId::from_bencode(dict.lookup("t")?)?;

        let query = match dict.lookup("q")?.bytes()? {
            b"ping" => Query::Ping,
            b"find_node" => Query::FindNode(NodeId::from_bencode(args.lookup("target")?)?),
            _ => return Err(DecodeError::InvalidDiscrim)
        };

        Ok(FullQuery {
            query: query,
            sender_id: sender_id,
            tx_id: tx_id,
        })
    }
}


impl ToBencode for FullQuery {
    fn to_bencode(&self) -> Bencode {
        let mut args = BTreeMap::new();
        let query_type: &'static [u8];
        args.insert(Bytes::from_str("id"), self.sender_id.to_bencode());
        match self.query {
            Query::Ping => query_type = b"ping",
            Query::FindNode(ref target) => {
                query_type = b"find_node";
                args.insert(Bytes::from_str("target"), target.to_bencode());
            }
        }

        let mut dict = BTreeMap::new();
        dict.insert(Bytes::from_str("y"), 'q'.to_bencode());
        dict.insert(Bytes::from_str("q"), ByteString(query_type.to_vec()));
        dict.insert(Bytes::from_str("t"), self.tx_id.to_bencode());
        dict.insert(Bytes::from_str("a"), Dict(args));
        Dict(dict)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Peer4Info(SocketAddrV4);

impl Peer4Info {
    fn parse(b: &[u8]) -> DecodeResult<Self> {
        if b.len() != 6 {
            return Err(DecodeError::WrongLength);
        }
        let ip = Ipv4Addr::new(b[0], b[1], b[2], b[3]);
        if !ip.is_global() {
            return Err(DecodeError::InvalidAddress(ip));
        }
        let port = ((b[4] as u16) << 8) + b[5] as u16;
        if port == 0 {
            return Err(DecodeError::OutOfRange);
        }
        Ok(Peer4Info(SocketAddrV4::new(ip, port)))
    }

    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::V4(self.0)
    }

}

/// Contact info for one IPv4 node.
#[derive(Clone, Copy, Debug)]
pub struct Node4Info {
    pub id: NodeId,
    pub peer: Peer4Info,
}

const NODE4_LEN: usize = NODE_ID_LEN + 6;

impl Node4Info {
    fn parse(bytes: &[u8]) -> DecodeResult<Self> {
        if bytes.len() == NODE4_LEN {
            Ok(Node4Info {
                id: NodeId::from_slice(&bytes[..NODE_ID_LEN])?,
                peer: Peer4Info::parse(&bytes[NODE_ID_LEN..])?,
            })
        } else {
            Err(DecodeError::WrongLength)
        }
    }

    fn parse_list(bytes: &[u8]) -> DecodeResult<Vec<Self>> {
        if bytes.len() % NODE4_LEN != 0 {
            return Err(DecodeError::WrongLength);
        }
        let mut nodes = Vec::with_capacity(bytes.len() / NODE4_LEN);
        // should this be limited to K=8 max?
        for entry in bytes.chunks(NODE4_LEN) {
            nodes.push(Node4Info::parse(entry)?);
        }
        Ok(nodes)
    }
}

/// Possible responses to a `Query`.
#[derive(Debug)]
pub enum Response {
    Pong,
    FoundNodes {nodes4: Vec<Node4Info>},
}

/// Full payload for a `Response`.
#[derive(Debug)]
pub struct FullResponse {
    pub response: Response,
    pub sender_id: NodeId,
    pub tx_id: TxId,
}

impl FromBencode for FullResponse {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        let dict = b.dict()?;
        if dict.lookup("y")?.bytes()? != b"r" {
            return Err(DecodeError::WrongDiscrim)
        }
        let args = dict.lookup("r")?.dict()?;

        // there's no explicit discriminator but we can tell by the args...
        let response: Response;
        if let Ok(token) = args.lookup("token") {
            panic!("get_peers not implemented {:?}", token);
        } else if let Ok(nodes) = args.lookup("nodes") {
            let nodes = Node4Info::parse_list(nodes.bytes()?)?;
            response = Response::FoundNodes {nodes4: nodes};
        } else {
            response = Response::Pong;
        }

        Ok(FullResponse {
            response: response,
            sender_id: NodeId::from_bencode(args.lookup("id")?)?,
            tx_id: TxId::from_bencode(dict.lookup("t")?)?,
        })
    }
}

/// Describes an error reported by one node to another.
#[derive(Debug)]
pub struct DhtError {
    pub message: String,
    pub code: u32,
    pub tx_id: TxId,
}

impl FromBencode for DhtError {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        let dict = b.dict()?;
        if dict.lookup("y")?.bytes()? != b"e" {
            return Err(DecodeError::WrongDiscrim)
        }
        let tx_id = TxId::from_bencode(dict.lookup("t")?)?;

        let args = dict.lookup("e")?.array()?;
        if args.len() != 2 {
            return Err(DecodeError::WrongLength);
        }
        let code = args[0].u32()?;
        let message = String::from_utf8_lossy(args[1].bytes()?).into_owned();
        Ok(DhtError {
            message: message,
            code: code,
            tx_id: tx_id,
        })
    }
}

/// Any message that can be sent and received.
#[derive(Debug)]
pub enum DhtMessage {
    Query(FullQuery),
    Response(FullResponse),
    Error(DhtError),
}

impl FromBencode for DhtMessage {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        use self::DhtMessage::*;
        let discrim = b.dict()?.lookup("y")?.bytes()?;
        Ok(match discrim {
            b"q" => Query(FullQuery::from_bencode(b)?),
            b"r" => Response(FullResponse::from_bencode(b)?),
            b"e" => Error(DhtError::from_bencode(b)?),
            _ => return Err(DecodeError::InvalidDiscrim),
        })
    }
}

// ! Helpers

/// Provides Result-based Bencode unwrapping.
trait BencodeExt {
    fn array(&self) -> DecodeResult<&ListVec>;
    fn bytes(&self) -> DecodeResult<&[u8]>;
    fn dict(&self) -> DecodeResult<&DictMap>;
    fn u32(&self) -> DecodeResult<u32>;
}

impl BencodeExt for Bencode {
    fn array(&self) -> DecodeResult<&ListVec> {
        match self {
            &List(ref vec) => Ok(vec),
            _ => Err(DecodeError::WrongType),
        }
    }
    fn bytes(&self) -> DecodeResult<&[u8]> {
        match self {
            &ByteString(ref vec) => Ok(vec),
            _ => Err(DecodeError::WrongType),
        }
    }
    fn dict(&self) -> DecodeResult<&DictMap> {
        match self {
            &Dict(ref map) => Ok(map),
            _ => Err(DecodeError::WrongType),
        }
    }
    fn u32(&self) -> DecodeResult<u32> {
        match self {
            // use ToPrimitive?
            &Number(n) if n >= 0 && n <= (std::u32::MAX as i64) => Ok(n as u32),
            &Number(_) => Err(DecodeError::OutOfRange),
            _ => Err(DecodeError::WrongType),
        }
    }
}

/// Provides Result-based Bencode::Dict lookups.
trait DictExt {
    fn lookup<'a>(&'a self, &'static str) -> DecodeResult<&'a Bencode>;
}

impl DictExt for DictMap {
    fn lookup<'a>(&'a self, key: &'static str) -> DecodeResult<&'a Bencode> {
        // would be nice to avoid constructing a new Bytes every lookup
        self.get(&Bytes::from_str(key)).ok_or(DecodeError::KeyMissing(key))
    }
}
