#![allow(unused)]
mod signaling_server;
use signaling_server::SignalingServer;

#[tokio::main]
async fn main() {
    println!("Hello, world!");
    const WS_URL: &'static str = "ws://35.200.208.109:9006";
    let mut server = SignalingServer::new(WS_URL).await.unwrap();

    let mut buf = String::with_capacity(128);
    let stdin = std::io::stdin();
    loop {
        buf.clear();
        let size = stdin.read_line(&mut buf).unwrap();
        if size == 0 {
            continue;
        }
        let buf = buf.trim();
        if buf.starts_with("send ") {
            let data = &buf[5..];
            server.send_message(data).await.unwrap();
        } else if buf.starts_with("offer") {
            let offer = server.send_offer().await.unwrap();
            println!("sent offer");
        } else if buf.starts_with("exit") {
            break;
        }
    }

    server.stop_stream().await;
}
