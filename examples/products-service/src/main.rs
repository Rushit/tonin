use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:50053")?;
    println!("products-service listening on 127.0.0.1:50053");

    for stream in listener.incoming() {
        let _stream = stream?;
    }
    Ok(())
}
