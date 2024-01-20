#![feature(core_io_borrowed_buf, read_buf)]
#![feature(maybe_uninit_write_slice)]

use base64::{prelude::BASE64_STANDARD, Engine};
use compio::{
    buf::{IntoInner, IoBuf},
    fs::File,
    io::{AsyncRead, AsyncReadAtExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use httparse::{Response, Status, EMPTY_HEADER};
use stdio::{stdin, stdout};

#[compio::main]
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

    sock.write_all(connection_string(dest_host, dest_port, auth_file).await)
        .await
        .expect("cannot send connect request");

    let (connected, buffer, len) = get_response(&mut sock).await;
    if !connected {
        panic!(
            "Proxy could not open connection to {}:{}",
            dest_host, dest_port
        );
    }

    let stdin = stdin();
    let mut stdout = stdout();

    if len < buffer.len() {
        stdout
            .write_all(buffer.slice(len..))
            .await
            .expect("cannot write to stdout");
        stdout.flush().await.expect("cannot flush stdout");
    }

    let sock2 = sock.try_clone().expect("cannot clone socket");

    let read_task = copy_io(sock, stdout);
    let write_task = copy_io(stdin, sock2);

    futures_util::join!(read_task, write_task);
}

fn usage() {
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    println!("rockscrew {} (Strawberry_Str@hotmail.com)\n\n", VERSION);
    println!("usage: rockscrew <proxyhost> <proxyport> <desthost> <destport> [authfile]\n");
}

async fn connection_string(dest_host: &str, dest_port: &str, auth_file: Option<&String>) -> String {
    dest_port.parse::<u16>().expect("invalid destination port");

    let prefix = format!("CONNECT {}:{} HTTP/1.0", dest_host, dest_port);
    let suffix = "\r\n\r\n";
    match auth_file {
        None => prefix + suffix,
        Some(auth_file) => {
            let file = File::open(auth_file).await.expect("cannot open auth file");

            let (_, buffer) = file
                .read_to_end_at(vec![], 0)
                .await
                .expect("cannot read auth file");

            let encoded = BASE64_STANDARD.encode(buffer);
            prefix + &format!("\nProxy-Authorization: Basic {}", encoded) + suffix
        }
    }
}

async fn get_response<R: AsyncRead + Unpin>(sock: &mut R) -> (bool, Vec<u8>, usize) {
    let mut buffer = Vec::with_capacity(4096);
    'outer: loop {
        let len = buffer.len();
        let (_, slice) = sock
            .read(buffer.slice(len..))
            .await
            .expect("cannot read_connect response");
        buffer = slice.into_inner();

        let mut headers = vec![EMPTY_HEADER; 16];
        loop {
            let mut resp = Response::new(&mut headers);
            let status = resp.parse(&buffer);
            if let Err(httparse::Error::TooManyHeaders) = status {
                headers.resize(headers.len() + 16, EMPTY_HEADER);
                continue;
            }
            let status = status.expect("cannot parse connect response");
            match status {
                Status::Complete(len) => {
                    let succeeded = match resp.code {
                        Some(code) => code <= 407,
                        None => false,
                    };
                    return (succeeded, buffer, len);
                }
                Status::Partial => {
                    if buffer.len() == buffer.capacity() {
                        buffer.reserve(4096);
                    }
                    continue 'outer;
                }
            }
        }
    }
}

async fn copy_io(mut src: impl AsyncRead, mut target: impl AsyncWrite) {
    loop {
        let buffer = [0u8; 4096];
        let (len, buffer) = src.read(buffer).await.expect("cannot read source");
        if len == 0 {
            break;
        }
        let (len, _) = target
            .write_all(buffer.slice(..len))
            .await
            .expect("cannot write target");
        target.flush().await.expect("cannot flush target");
        if len == 0 {
            break;
        }
    }
}

#[cfg(windows)]
mod stdio {
    use std::{
        io::{BorrowedBuf, Read, Write},
        mem::MaybeUninit,
    };

    use compio::{
        buf::{IoBuf, IoBufMut},
        io::{AsyncRead, AsyncWrite},
        BufResult,
    };

    pub struct Stdin;

    pub fn stdin() -> Stdin {
        Stdin
    }

    impl AsyncRead for Stdin {
        async fn read<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
            let capacity = buf.buf_capacity();
            let BufResult(res, buffer) = compio::runtime::spawn_blocking(move || {
                let mut buffer = Vec::with_capacity(capacity);
                let mut bbuf = BorrowedBuf::from(buffer.spare_capacity_mut());
                let mut cursor = bbuf.unfilled();
                let res = std::io::stdin().read_buf(cursor.reborrow());
                let len = cursor.written();
                unsafe { buffer.set_len(len) };
                BufResult(res.map(|()| len), buffer)
            })
            .await;
            if let Ok(len) = res {
                MaybeUninit::write_slice(&mut buf.as_mut_slice()[..len], &buffer[..len]);
                unsafe { buf.set_buf_init(len) };
            }
            BufResult(res, buf)
        }
    }

    pub struct Stdout;

    pub fn stdout() -> Stdout {
        Stdout
    }

    impl AsyncWrite for Stdout {
        async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
            let buffer = buf.as_slice().to_vec();
            let res =
                compio::runtime::spawn_blocking(move || std::io::stdout().write(&buffer)).await;
            BufResult(res, buf)
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            compio::runtime::spawn_blocking(|| std::io::stdout().flush()).await
        }

        async fn shutdown(&mut self) -> std::io::Result<()> {
            self.flush().await
        }
    }
}
