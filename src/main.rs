#![feature(question_mark)]

extern crate bencode;
extern crate mio;
extern crate rand;

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;

use bencode::{Bencode, ToBencode};
use bencode::Bencode::{ByteString, Dict};
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

impl ToBencode for TxId {
    fn to_bencode(&self) -> Bencode {
        ByteString(self.as_slice().to_vec())
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

const SERVER: Token = Token(0);

struct ServerHandler {
    sock: UdpSocket,
    id: NodeId,
}

impl Handler for ServerHandler {
    type Timeout = &'static str;
    type Message = ();

    fn ready(&mut self, _: &mut EventLoop<ServerHandler>, token: Token, _: EventSet) {
        if token == SERVER {
            let mut buf = [0u8; 512];
            match self.sock.recv_from(&mut buf) {
                Ok(Some((len, addr))) => {
                    assert!(len < 512, "big packet");
                    println!("{:?}: ((({:?})))", addr, &buf[..len]);
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
