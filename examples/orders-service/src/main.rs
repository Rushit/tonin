use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:50052")?;
    println!("orders-service listening on 127.0.0.1:50052");

    for stream in listener.incoming() {
        let _stream = stream?;
    }
    Ok(())
}
