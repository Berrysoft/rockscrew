use base64::{prelude::BASE64_STANDARD, Engine};
use httparse::{Response, Status, EMPTY_HEADER};
use tokio::{
    fs::File,
    io::{stdin, stdout, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 && args.len() != 6 {
        usage();
        return;
    }
    let host = &args[1];
    let port = &args[2];
    let dest_host = &args[3];
    let dest_port = &args[4];
    let auth_file = args.get(5);

    let port = port.parse::<u16>().expect("invalid proxy port");
    let mut sock = TcpStream::connect((host.as_str(), port))
        .await
        .expect("cannot connect to proxy");

    sock.write_all(
        connection_string(dest_host, dest_port, auth_file)
            .await
            .as_bytes(),
    )
    .await
    .expect("cannot send connect request");

    {
        let mut buffer = [0u8; 4096];
        let len = sock
            .read(&mut buffer)
            .await
            .expect("cannot read connect response");
        let mut headers = [EMPTY_HEADER; 16];
        let mut resp = Response::new(&mut headers);
        let status = resp
            .parse(&buffer[..len])
            .expect("cannot parse connect response");
        match status {
            Status::Complete(_) => {}
            Status::Partial => panic!("connect response incomplete"),
        }
        match resp.code {
            Some(code) => {
                if code > 407 {
                    panic!(
                        "Proxy could not open connection to {}:{}",
                        dest_host, dest_port
                    );
                }
            }
            None => unreachable!(),
        }
    }

    let mut stdin = stdin();
    let mut stdout = stdout();

    let mut sock_buffer = [0u8; 4096];
    let mut stdin_buffer = [0u8; 4096];
    loop {
        tokio::select! {
            len = sock.read(&mut sock_buffer) => {
                let len = len.expect("cannot read socket");
                if len == 0 {
                    break;
                }
                stdout.write_all(&sock_buffer[..len]).await.expect("cannot write to stdout");
                stdout.flush().await.expect("cannot flush stdout");
            }
            len = stdin.read(&mut stdin_buffer) => {
                let len = len.expect("cannot read socket");
                if len == 0 {
                    break;
                }
                sock.write_all(&stdin_buffer[..len]).await.expect("cannot write to socket");
                sock.flush().await.expect("cannot flush socket");
            }
        }
    }
}

fn usage() {
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    println!("rorkscrew {} (Strawberry_Str@hotmail.com)\n\n", VERSION);
    println!("usage: rorkscrew <proxyhost> <proxyport> <desthost> <destport> [authfile]\n");
}

async fn connection_string(dest_host: &str, dest_port: &str, auth_file: Option<&String>) -> String {
    dest_port.parse::<u16>().expect("invalid destination port");

    let prefix = format!("CONNECT {}:{} HTTP/1.0", dest_host, dest_port);
    let suffix = "\r\n\r\n";
    match auth_file {
        None => prefix + suffix,
        Some(auth_file) => {
            let mut file = File::open(auth_file).await.expect("cannot open auth file");

            let mut buffer = vec![];
            file.read_to_end(&mut buffer)
                .await
                .expect("cannot read auth file");

            let encoded = BASE64_STANDARD.encode(&buffer);
            prefix + &format!("\nProxy-Authorization: Basic {}", encoded) + suffix
        }
    }
}