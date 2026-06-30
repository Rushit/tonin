use std::net::TcpListener;

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:50051")?;
    println!("users-service listening on 127.0.0.1:50051");

    for stream in listener.incoming() {
        let _stream = stream?;
    }
    Ok(())
}
