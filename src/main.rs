#![feature(ip, question_mark)]

extern crate bencode;
extern crate mio;
extern crate rand;

use std::io;
use std::net::SocketAddr;

use bencode::{Bencode, FromBencode, ToBencode};
use mio::{EventLoop, EventSet, Handler, PollOpt, Token};
use mio::udp::UdpSocket;

use messages::*;

mod messages;

fn main() {
    serve().unwrap()
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

    fn received(&self, event_loop: &mut EventLoop<ServerHandler>, addr: &SocketAddr, msg: &Bencode)
        -> io::Result<()>
    {
        match DhtMessage::from_bencode(msg)? {
            DhtMessage::Query(query) => {
                println!("query from {:?}: {:?}", addr, query);
            }
            DhtMessage::Response(resp) => match resp.response {
                Response::Pong => {
                    println!("pong {:?} => {:?}", resp.tx_id, resp.sender_id);

                    let target = NodeId::random();
                    println!("ask for {:?}", target);
                    self.send(event_loop, addr, Query::FindNode(target))?;
                }
                Response::FoundNodes {nodes4} => {
                    println!("found nodes {:?}", nodes4);
                }
            },
            DhtMessage::Error(e) => {
                println!("error from {:?}: {:?}", addr, e);
            }
        }
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
