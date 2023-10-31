mod busted_polling;
mod mio;
mod multithread;
mod nonblocking;
mod nonblocking_spin;
mod simple;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        println!("Please specify a version of the webserver to run.");
        return;
    }

    let version = args[1].as_str();
    match version {
        "simple" => simple::main(),
        "multithread" => multithread::main(),
        "mio" => mio::main(),
        "nonblocking_spin" => nonblocking_spin::main(),
        "nonblocking" => nonblocking::main(),
        "busted_polling" => busted_polling::main(),
        _ => println!("Invalid version specified: {:}.", version),
    }
}
