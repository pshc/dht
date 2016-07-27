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

/// Short quasi-unique IDs for correlating queries to responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestId(u32);

/// RequestIds are serialized as `[a-z]{2}` for now.
const MAX_REQUEST_ID: u32 = 26 * 26;

impl RequestId {
    fn random() -> Self {
        // ought to use rand::distributions::Range...
        RequestId(rand::random::<u32>() % MAX_REQUEST_ID)
    }

    fn as_vec(&self) -> Vec<u8> {
        assert!(self.0 < MAX_REQUEST_ID);
        const LIMIT: u32 = 26;
        debug_assert_eq!(MAX_REQUEST_ID, LIMIT * LIMIT);
        let (hi, lo) = (self.0 / LIMIT, self.0 % LIMIT);
        vec![hi as u8 + 97, lo as u8 + 97]
    }
}

impl ToBencode for RequestId {
    fn to_bencode(&self) -> Bencode {
        ByteString(self.as_vec())
    }
}

#[derive(Debug)]
enum Query {
    Ping,
}

#[derive(Debug)]
struct FullQuery {
    query: Query,
    request_id: RequestId,
    sender_id: NodeId,
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
        dict.insert(Bytes::from_str("t"), self.request_id.to_bencode());
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
        let request_id = RequestId::random();
        let full = FullQuery {
            query: query,
            request_id: request_id,
            sender_id: self.id,
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
