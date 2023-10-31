// Uses non-blocking I/O to accept connections and a state machine to manage their progress
// Single-threaded, so similar to async in Python or Node.js
use std::io;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::sleep;
use std::time::Duration;

#[allow(clippy::large_enum_variant)]
enum ConnectionState<'a> {
    ReadingRequest { request: [u8; 1024], read: usize },
    WritingResponse { response: &'a [u8], written: usize },
    Flushing,
}

pub fn main() {
    let listener = TcpListener::bind("localhost:3000").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut connections = Vec::new();
    loop {
        // try to accept a new connection
        // this does a context switch to the kernel -- a syscall
        match listener.accept() {
            // we got a new connection!
            Ok((connection, _)) => {
                connection.set_nonblocking(true).unwrap();

                let state = ConnectionState::ReadingRequest {
                    request: [0u8; 1024],
                    read: 0,
                };
                connections.push((connection, state));
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            // some other error occurred
            Err(e) => panic!("encountered IO error: {e}"),
        };

        let mut completed = Vec::new();

        'next: for (i, (connection, state)) in connections.iter_mut().enumerate() {
            if let ConnectionState::ReadingRequest { request, read } = state {
                // try reading from the stream
                loop {
                    match connection.read(&mut request[*read..]) {
                        Ok(0) => {
                            println!("client disconnected unexpectedly");
                            completed.push(i);
                            continue 'next;
                        }
                        Ok(num_bytes) => {
                            // keep track of how many bytes we've read
                            *read += num_bytes;
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            continue 'next;
                        }
                        // some other error occurred
                        Err(e) => panic!("encountered IO error: {e}"),
                    }
                    // have we reached the end of the request?
                    if request.get(*read - 4..*read) == Some(b"\r\n\r\n") {
                        break;
                    }
                }
                // we're done, print the request
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

                *state = ConnectionState::WritingResponse {
                    response: response.as_bytes(),
                    written: 0,
                };
            };
            if let ConnectionState::WritingResponse { response, written } = state {
                // try writing to the stream
                loop {
                    match connection.write(&response[*written..]) {
                        Ok(0) => {
                            println!("client disconnected unexpectedly");
                            completed.push(i);
                            continue 'next;
                        }
                        Ok(num_bytes) => {
                            // keep track of how many bytes we've written
                            *written += num_bytes;
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            continue 'next;
                        }
                        // some other error occurred
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
                // try flushing the stream
                match connection.flush() {
                    Ok(()) => {}
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        continue 'next;
                    }
                    // some other error occurred
                    Err(e) => panic!("encountered IO error: {e}"),
                }
                completed.push(i);
            }
        }
        for i in completed.into_iter().rev() {
            connections.remove(i);
        }
    }
}
