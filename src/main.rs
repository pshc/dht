#![feature(ip, question_mark)]

extern crate bencode;
extern crate mio;
extern crate rand;

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;

use bencode::{Bencode, FromBencode, ToBencode};
use mio::{EventLoop, EventSet, Handler, PollOpt, Timeout, Token};
use mio::udp::UdpSocket;

use messages::*;
use table::{NodeState, Slot, Table};

mod messages;
mod table;

fn main() {
    serve().unwrap()
}

const SERVER: Token = Token(0);

struct ServerHandler {
    sock: UdpSocket,
    table: Table,
    txs: HashMap<TxId, Tx>,
}

impl Handler for ServerHandler {
    type Timeout = TxId;
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

    fn timeout(&mut self, _: &mut EventLoop<ServerHandler>, id: TxId) {
        if let Some(_) = self.txs.remove(&id) {
            println!("timeout {:?}", id);
        }
    } 
}

impl ServerHandler {
    fn send(&mut self, event_loop: &mut EventLoop<ServerHandler>, dest: &SocketAddr, query: Query)
        -> io::Result<()>
    {
        // Generate a unique ID for this transaction.
        let tx_id;
        let mut attempts = 0;
        loop {
            let try_id = TxId::random();
            if !self.txs.contains_key(&try_id) {
                tx_id = try_id;
                break
            }
            attempts += 1;
            if attempts > 10 {
                // should make a long random ID here
                return Err(io::Error::new(io::ErrorKind::Other, "tx IDs unavailable"))
            }
        }

        let full = FullQuery {
            query: query,
            sender_id: self.table.our_id().clone(),
            tx_id: tx_id.clone(),
        };
        println!("send to {:?}: {:?}", dest, full);
        let bytes = full.to_bencode().to_bytes()?;

        // TODO completion closure?

        if let Some(n_sent) = self.sock.send_to(&bytes, dest)? {
            assert_eq!(n_sent, bytes.len());

            let timeout = event_loop.timeout_ms(tx_id.clone(), 5000).unwrap();
            let tx = Tx::FirstPing(dest.clone(), timeout);
            let overwritten = self.txs.insert(tx_id, tx);
            debug_assert!(overwritten.is_none());

            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "ServerHandler::send: got None"))
        }
    }

    fn received(&mut self, event_loop: &mut EventLoop<ServerHandler>, addr: &SocketAddr, msg: &Bencode)
        -> io::Result<()>
    {
        match DhtMessage::from_bencode(msg)? {
            DhtMessage::Query(query) => {
                println!("query from {:?}: {:?}", addr, query);
                Ok(())
            }
            DhtMessage::Response(resp) => {
                match self.txs.remove(&resp.tx_id) {
                    Some(tx) => self.handle(event_loop, addr, resp, tx),
                    None => {
                        Err(io::Error::new(io::ErrorKind::Other,
                            format!("{:?}: {:?} has unknown tx", addr, resp)))
                    }
                }
            }
            DhtMessage::Error(e) => {
                println!("error from {:?}: {:?}", addr, e);
                Ok(())
            }
        }
    }

    fn handle(&mut self, event_loop: &mut EventLoop<ServerHandler>, addr: &SocketAddr,
              resp: FullResponse, tx: Tx) -> io::Result<()>
    {
        let ref sender = resp.sender_id;
        match resp.response {
            Response::Pong => {
                println!("pong from {:?}", resp.sender_id);

                match tx {
                    Tx::FirstPing(pinged_addr, timeout) => {
                        if addr != &pinged_addr {
                            return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong addr"))
                        }
                        event_loop.clear_timeout(timeout);

                        // okay, we got the first-ping back from our peer.
                        // try to add them to our routing table.
                        if let Some(slot) = self.table.allocate(sender) {
                            match *slot {
                                Slot::Empty => {
                                    *slot = Slot::Node(sender.clone(), NodeState::Good);
                                    // set timeout here...
                                }
                                Slot::Node(_, ref mut state@NodeState::Pinging) => {
                                    // XXX there's no way to *get* to here from a FirstPing!
                                    //     or is there??? could be a collision...
                                    *state = NodeState::Good;
                                }
                                Slot::Node(_, NodeState::Good) => {
                                    // refresh timeout?
                                    println!("first-pong: {:?} already Good", sender)
                                }
                            }
                        } else {
                            println!("first-pong from now-evicted {:?}", sender);
                        }
                    }
                }

                let target = NodeId::random();
                println!("ask for {:?}", target);
                self.send(event_loop, addr, Query::FindNode(target))
            }
            Response::FoundNodes {nodes4} => {
                println!("found {} nodes...", nodes4.len());
                for found_node in nodes4 {
                    let ping: bool;
                    if let Some(slot) = self.table.allocate(&found_node.id) {
                        ping = slot.is_empty();
                        if ping {
                            println!("{:?} is new, will ping", found_node.id);
                            *slot = Slot::Node(found_node.id, NodeState::Pinging);
                        }
                    } else {
                        // no space for it, so just drop it
                        ping = false;
                    }
                    if ping {
                        self.send(event_loop, &found_node.peer.socket_addr(), Query::Ping)?
                    }
                }
                Ok(())
            }
        }
    }
}

enum Tx {
    FirstPing(SocketAddr, Timeout),
}

fn serve() -> io::Result<()> {
    let ref my_addr = "0.0.0.0:6881".parse().unwrap(); // todo cast to io error
    let sock = UdpSocket::bound(my_addr)?;

    let ref mut event_loop: EventLoop<ServerHandler> = EventLoop::new()?;
    event_loop.register(&sock, SERVER, EventSet::readable(), PollOpt::edge())?;

    let ref bootstrap_addr = "212.129.33.50:6881".parse().unwrap(); // dht.transmissionbt.com

    let my_id = NodeId::random();
    let ref mut handler = ServerHandler {
        sock: sock,
        table: Table::new(my_id),
        txs: HashMap::new(),
    };
    handler.send(event_loop, bootstrap_addr, Query::Ping)?;

    event_loop.run(handler)
}
