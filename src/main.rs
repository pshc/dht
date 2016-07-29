#![feature(question_mark)]

extern crate bencode;
extern crate mio;
extern crate rand;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io;
use std::net::SocketAddr;

use bencode::{Bencode, DictMap, FromBencode, ListVec, ToBencode};
use bencode::Bencode::{ByteString, Dict, List, Number};
use bencode::util::ByteString as Bytes;
use mio::{EventLoop, EventSet, Handler, PollOpt, Token};
use mio::udp::UdpSocket;

fn main() {
    serve().unwrap()
}

/// The 160-bit space of BitTorrent infohashes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NodeId([u8; 20]);

impl NodeId {
    fn random() -> Self {
        NodeId(rand::random())
    }
}

impl FromBencode for NodeId {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        let bytes = b.bytes()?;
        if bytes.len() == 20 {
            let mut fixed: [u8; 20] = [0; 20];
            fixed.copy_from_slice(bytes);
            Ok(NodeId(fixed))
        } else {
            Err(DecodeError::WrongLength)
        }
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TxId {
    Short([u8; 2]),
    Arbitrary(Bytes),
}

impl TxId {
    fn random() -> Self {
        fn alpha() -> u8 {
            let n = rand::random::<u8>() % 52;
            let c = if n >= 26 { n - 26 + 97 } else { n + 65 };
            debug_assert!(match c as char { 'A'...'Z' | 'a'...'z' => true, _ => false });
            c
        }
        TxId::Short([alpha(), alpha()])
    }

    fn as_slice(&self) -> &[u8] {
        match *self {
            TxId::Short(ref two) => two,
            TxId::Arbitrary(ref bytes) => bytes.as_slice(),
        }
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

impl ToBencode for TxId {
    fn to_bencode(&self) -> Bencode {
        ByteString(self.as_slice().to_vec())
    }
}

// BENCODED MESSAGES

#[derive(Debug)]
pub enum DecodeError {
    KeyMissing(&'static str),
    InvalidDiscrim,
    OutOfRange,
    WrongDiscrim,
    WrongLength,
    WrongType,
}

impl Error for DecodeError {
    fn description(&self) -> &str {
        use DecodeError::*;
        match *self {
            KeyMissing(_) => "required key missing",
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
            _ => write!(f, "<DecodeError: {}>", self.description())
        }
    }
}

impl From<DecodeError> for io::Error {
    fn from(error: DecodeError) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

pub type DecodeResult<T> = Result<T, DecodeError>;

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

#[derive(Debug)]
enum Query {
    Ping,
}

#[derive(Debug)]
struct FullQuery {
    query: Query,
    sender_id: NodeId,
    tx_id: TxId,
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

        let kind = dict.lookup("q")?.bytes()?;
        if kind != b"ping" {
            return Err(DecodeError::InvalidDiscrim);
        }
        Ok(FullQuery {
            query: Query::Ping,
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
            Query::Ping => {
                query_type = b"ping";
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

#[derive(Debug)]
enum Response {
    Pong,
}

#[derive(Debug)]
struct FullResponse {
    response: Response,
    sender_id: NodeId,
    tx_id: TxId,
}

impl FromBencode for FullResponse {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        let dict = b.dict()?;
        if dict.lookup("y")?.bytes()? != b"r" {
            return Err(DecodeError::WrongDiscrim)
        }
        let args = dict.lookup("r")?.dict()?;
        let response = Response::Pong;

        Ok(FullResponse {
            response: response,
            sender_id: NodeId::from_bencode(args.lookup("id")?)?,
            tx_id: TxId::from_bencode(dict.lookup("t")?)?,
        })
    }
}


#[derive(Debug)]
struct DhtError {
    message: String,
    code: u32,
    tx_id: TxId,
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
enum DhtMessage {
    Query(FullQuery),
    Response(FullResponse),
    Error(DhtError),
}

impl FromBencode for DhtMessage {
    type Err = DecodeError;
    fn from_bencode(b: &Bencode) -> DecodeResult<Self> {
        use DhtMessage::*;
        let discrim = b.dict()?.lookup("y")?.bytes()?;
        Ok(match discrim {
            b"q" => Query(FullQuery::from_bencode(b)?),
            b"r" => Response(FullResponse::from_bencode(b)?),
            b"e" => Error(DhtError::from_bencode(b)?),
            _ => return Err(DecodeError::InvalidDiscrim),
        })
    }
}

const SERVER: Token = Token(0);

struct ServerHandler {
    sock: UdpSocket,
    id: NodeId,
}

impl Handler for ServerHandler {
    type Timeout = &'static str;
    type Message = ();

    fn ready(&mut self, event_loop: &mut EventLoop<ServerHandler>, token: Token, _: EventSet) {
        if token == SERVER {
            let mut buf = [0u8; 512];
            match self.sock.recv_from(&mut buf) {
                Ok(Some((len, addr))) => {
                    assert!(len < 512, "big packet");

                    match bencode::from_buffer(&buf[..len]) {
                        Ok(msg) => {
                            match self.received(event_loop, &addr, &msg) {
                                Ok(()) => (),
                                Err(e) => println!("{:?}: {:?}", addr, e)
                            }
                        }
                        Err(e) => println!("{:?}: at pos {}: {}", addr, e.pos, e.msg)
                    }
                }
                Ok(None) => println!("S: got nothing?"),
                Err(e) => println!("S: error: {}", e),
            }

        } else {
            panic!(token);
        }
    }

    fn timeout(&mut self, _: &mut EventLoop<ServerHandler>, timeout: &'static str) {
        println!("timeout {}", timeout);
    } 
}

impl ServerHandler {
    fn send(&self, event_loop: &mut EventLoop<ServerHandler>, dest: &SocketAddr, query: Query) -> io::Result<()> {
        let tx_id = TxId::random();
        let full = FullQuery {
            query: query,
            sender_id: self.id,
            tx_id: tx_id,
        };
        println!("send to {:?}: {:?}", dest, full);
        let bytes = full.to_bencode().to_bytes()?;

        // TODO completion closure?

        if let Some(n_sent) = self.sock.send_to(&bytes, dest)? {
            assert_eq!(n_sent, bytes.len());

            let _timeout = event_loop.timeout_ms("1sec after send", 1000);
            // keep _timeout to cancel later

            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "ServerHandler::send: got None"))
        }
    }

    fn received(&self, _: &mut EventLoop<ServerHandler>, addr: &SocketAddr, msg: &Bencode) -> io::Result<()> {
        let resp = DhtMessage::from_bencode(msg)?;
        println!("{:?}: valid {:?})", addr, resp);
        Ok(())
    }
}

fn serve() -> io::Result<()> {
    let ref my_addr = "0.0.0.0:6881".parse().unwrap(); // todo cast to io error
    let sock = UdpSocket::bound(my_addr)?;

    let ref mut event_loop: EventLoop<ServerHandler> = EventLoop::new()?;
    event_loop.register(&sock, SERVER, EventSet::readable(), PollOpt::edge())?;

    let ref bootstrap_addr = "212.129.33.50:6881".parse().unwrap(); // dht.transmissionbt.com

    let ref mut handler = ServerHandler {
        sock: sock,
        id: NodeId::random(),
    };
    handler.send(event_loop, bootstrap_addr, Query::Ping)?;

    event_loop.run(handler)
}
