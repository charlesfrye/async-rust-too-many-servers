use mio::event::Source;
use mio::net::{TcpListener, TcpStream};
use std::sync::Once;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    os::fd::AsRawFd,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    fn wake(&self) {
        (self.0)()
    }
}

trait Future {
    type Output;

    fn poll(&mut self, waker: Waker) -> Option<Self::Output>;
}

use mio::{Events, Poll, Token};

struct Reactor {
    poll: Poll,
    tasks: RefCell<HashMap<Token, Waker>>,
}

impl Reactor {
    pub fn new() -> Reactor {
        Reactor {
            poll: Poll::new().unwrap(),
            tasks: RefCell::new(HashMap::new()),
        }
    }

    pub fn add<S: Source + AsRawFd>(&self, source: &mut S, waker: Waker) {
        let token = Token(source.as_raw_fd() as usize); // Assigning a token using raw fd
        self.poll
            .registry()
            .register(
                source,
                token,
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .unwrap();
        self.tasks.borrow_mut().insert(token, waker);
    }

    pub fn remove<S: Source + AsRawFd>(&self, source: &mut S) {
        let token = Token(source.as_raw_fd() as usize);
        if let Err(e) = self.poll.registry().deregister(source) {
            eprintln!(
                "Failed to deregister source with token {:?} due to error {:?}",
                token, e
            ); // or handle it appropriately
        }
        self.tasks.borrow_mut().remove(&token);
    }

    // Drive tasks forward, blocking forever until an event arrives.
    pub fn wait(&mut self) {
        let mut events = Events::with_capacity(1024);

        self.poll.poll(&mut events, None).unwrap();

        for event in events.iter() {
            let token = event.token();

            // wake the task
            if let Some(waker) = self.tasks.borrow().get(&token) {
                waker.clone().wake();
            }
        }
    }
}

type SharedTask = Arc<Mutex<dyn Future<Output = ()> + Send>>;

// The scheduler.
#[derive(Default)]
struct Scheduler {
    runnable: Mutex<VecDeque<SharedTask>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            runnable: Mutex::new(VecDeque::new()),
        }
    }
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        self.runnable
            .lock()
            .unwrap()
            .push_back(Arc::new(Mutex::new(task)));
    }
    pub fn run(&self) {
        loop {
            loop {
                // pop a runnable task off the queue
                let Some(task) = self.runnable.lock().unwrap().pop_front() else { break };
                let t2 = task.clone();

                // create a waker that pushes the task back on
                let wake = Arc::new(move || {
                    get_scheduler()
                        .runnable
                        .lock()
                        .unwrap()
                        .push_back(t2.clone());
                });

                // poll the task
                task.lock().unwrap().poll(Waker(wake));
            }

            // if there are no runnable tasks, block on epoll until something becomes ready
            REACTOR.with(|reactor| reactor.borrow_mut().wait());
        }
    }
}

thread_local! {
    static REACTOR: RefCell<Reactor> = RefCell::new(Reactor::new());
}
static SCHEDULER_INIT: Once = Once::new();
static mut SCHEDULER: Option<Scheduler> = None;

fn get_scheduler() -> &'static Scheduler {
    unsafe {
        SCHEDULER_INIT.call_once(|| {
            SCHEDULER = Some(Scheduler::new());
        });
        SCHEDULER.as_ref().unwrap()
    }
}

fn main() {
    get_scheduler().spawn(Main::Start);
    get_scheduler().run();
}

// main task: accept loop
enum Main {
    Start,
    Accept { listener: TcpListener },
}

impl Future for Main {
    type Output = ();

    fn poll(&mut self, waker: Waker) -> Option<()> {
        if let Main::Start = self {
            let mut listener = TcpListener::bind("127.0.0.1:3000".parse().unwrap()).unwrap();

            REACTOR.with(|reactor| {
                reactor.borrow_mut().add(&mut listener, waker);
            });

            *self = Main::Accept { listener };
        }

        if let Main::Accept { listener } = self {
            match listener.accept() {
                Ok((connection, _)) => {
                    // ...
                    get_scheduler().spawn(Handler {
                        connection,
                        state: HandlerState::Start,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                Err(e) => panic!("{e}"),
            }
        }

        None
    }
}

// handler task: handles every connection
struct Handler {
    connection: TcpStream,
    state: HandlerState,
}

enum HandlerState {
    Start,
    Read {
        request: [u8; 1024],
        read: usize,
    },
    Write {
        response: &'static [u8],
        written: usize,
    },
    Flush,
}

impl Future for Handler {
    type Output = ();

    fn poll(&mut self, waker: Waker) -> Option<Self::Output> {
        if let HandlerState::Start = self.state {
            REACTOR.with(|reactor| {
                reactor.borrow_mut().add(&mut self.connection, waker);
            });

            self.state = HandlerState::Read {
                request: [0u8; 1024],
                read: 0,
            };
        }

        if let HandlerState::Read { request, read } = &mut self.state {
            loop {
                match self.connection.read(&mut request[*read..]) {
                    Ok(0) => {
                        println!("client disconnected unexpectedly");
                        return Some(());
                    }
                    Ok(n) => *read += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                    Err(e) => panic!("{e}"),
                }

                // did we reach the end of the request?
                let read = *read;
                if read >= 4 && &request[read - 4..read] == b"\r\n\r\n" {
                    break;
                }
            }

            // we're done, print the request
            let request = String::from_utf8_lossy(&request[..*read]);
            println!("{}", request);

            // and move into the write state
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Length: 13\n",
                "Connection: close\r\n\r\n",
                "Hello world!\n"
            );

            self.state = HandlerState::Write {
                response: response.as_bytes(),
                written: 0,
            };
        }

        if let HandlerState::Write { response, written } = &mut self.state {
            loop {
                match self.connection.write(&response[*written..]) {
                    Ok(0) => return Some(()),
                    Ok(n) => *written += n,
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return None,
                    // some other error occurred
                    Err(e) => panic!("encountered IO error: {e}"),
                }
                // have we written the entire response?
                if *written == response.len() {
                    break;
                }
            }
            self.state = HandlerState::Flush;
        }

        if let HandlerState::Flush = self.state {
            match self.connection.flush() {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return None, // 👈
                Err(e) => panic!("{e}"),
            }
        }

        REACTOR.with(|reactor| {
            reactor.borrow_mut().remove(&mut self.connection);
        });

        Some(())
    }
}
