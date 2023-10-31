// polling-based multiplexed I/O
// currently hangs if there are >2 clients :<
use std::collections::HashMap;
use std::io;
use std::io::{Read, Write};
// use std::os::fd::AsRawFd;

use mio::net::TcpListener;
use mio::{Events, Interest, Poll, Token};

use std::thread::sleep;
use std::time::Duration;

#[allow(clippy::large_enum_variant)]
enum ConnectionState {
    ReadingRequest {
        request: [u8; 1024],
        read: usize,
    },
    WritingResponse {
        response: &'static [u8],
        written: usize,
    },
    Flushing,
}

// stolen from blog_os
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct TaskId(usize);

use core::sync::atomic::{AtomicUsize, Ordering};

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

// Some token to allow us to identify which event is for the listener
const LISTENER: Token = Token(0);

pub fn main() {
    // create poll
    let mut poll = Poll::new().unwrap();

    // bind the listener
    let mut listener = TcpListener::bind("127.0.0.1:3000".parse().unwrap()).unwrap();

    // register the listener
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)
        .unwrap();

    let mut connections = HashMap::new();

    let mut events = Events::with_capacity(1024);
    loop {
        // block until poll wakes us up
        poll.poll(&mut events, Some(Duration::new(5, 0))).unwrap();
        let mut completed = Vec::new();

        println!(
            "{:#?}",
            events // .iter()
                   // .map(|event| { event.token() })
                   // .collect::<Vec<Token>>()
        );

        'next: for event in events.iter() {
            let token = event.token();
            // is the listener ready with a new connection?
            println!("processing event for token {:}", token.0);
            if token == LISTENER {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        let id = TaskId::new();
                        println!("accepted connection: {:?}", id);

                        // add the connection to the poller
                        poll.registry()
                            .register(&mut connection, Token(id.0), Interest::READABLE)
                            .unwrap();

                        // keep track of connection state
                        let state = ConnectionState::ReadingRequest {
                            request: [0u8; 1024],
                            read: 0,
                        };

                        connections.insert(id.0, (connection, state));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        println!("blocked in listener");
                    }
                    Err(e) => panic!("encountered IO error: {}", e),
                }
                continue 'next;
            }
            // otherwise, it must be a connection
            let (connection, state) = connections.get_mut(&token.0).unwrap();
            // is the connection readable?
            if let ConnectionState::ReadingRequest { request, read } = state {
                println!("reading from {:}", token.0);
                loop {
                    match connection.read(&mut request[*read..]) {
                        Ok(0) => {
                            println!("client disconnected unexpectedly");
                            completed.push(token.0);
                            continue 'next;
                        }
                        Ok(num_bytes) => {
                            // keep track of how many bytes we've read
                            *read += num_bytes;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            println!("blocked on read");
                        }
                        Err(e) => panic!("encountered IO error: {e}"),
                    }

                    // have we reached the end of the request?
                    if *read >= 4 {
                        if request.get(*read - 4..*read) == Some(b"\r\n\r\n") {
                            break;
                        }
                    }
                }
                let _request = String::from_utf8_lossy(&request[..*read]);
                // println!("{request}");
                // sleep for 10 ms to simulate doing some work
                sleep(Duration::from_millis(10));
                let response = concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Length: 13\n",
                    "Connection: close\r\n\r\n",
                    "Hello world!\n"
                );

                // add the connection to the poller
                poll.registry()
                    .reregister(connection, token, Interest::WRITABLE)
                    .unwrap();

                *state = ConnectionState::WritingResponse {
                    response: response.as_bytes(),
                    written: 0,
                }
            };

            // is the connection writable?
            if let ConnectionState::WritingResponse { response, written } = state {
                println!("writing to {:?}", token.0);
                loop {
                    match connection.write(&response[*written..]) {
                        Ok(0) => {
                            println!("client disconnected unexpectedly");
                            completed.push(token.0);
                            continue 'next;
                        }
                        Ok(num_bytes) => {
                            // keep track of how many bytes we've written
                            *written += num_bytes;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            println!("blocked on write");
                        }
                        Err(e) => panic!("encountered IO error: {e}"),
                    }

                    // have we written the entire response?
                    if *written == response.len() {
                        break;
                    }
                }

                *state = ConnectionState::Flushing;
            }

            if let ConnectionState::Flushing = state {
                //try to flush the connection
                println!("flushing {:?}", token.0);
                loop {
                    match connection.flush() {
                        Ok(()) => {
                            completed.push(token.0);
                            break;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            println!("blocked on flush");
                        }
                        Err(e) => {
                            panic!("encountered IO error: {e}");
                        }
                    }
                }
            }
        }

        // remove completed connections
        for id in completed.iter() {
            match connections.remove(&id) {
                Some((mut connection, _)) => {
                    poll.registry().deregister(&mut connection).unwrap();
                    drop(connection);
                    println!("connection closed: {}", id);
                }
                None => {
                    println!("connection not found: {}", id)
                }
            }
        }
    }
}
