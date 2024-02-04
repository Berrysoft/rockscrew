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

    let (sock_read, sock_write) = sock.split();

    let read_task = copy_io(sock_read, stdout);
    let write_task = copy_io(stdin, sock_write);

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

async fn get_response(sock: &mut impl AsyncRead) -> (bool, Vec<u8>, usize) {
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
        target
            .write_all(buffer.slice(..len))
            .await
            .expect("cannot write target");
        target.flush().await.expect("cannot flush target");
    }
}

#[cfg(windows)]
mod stdio {
    use std::{
        io::IsTerminal, os::windows::io::AsRawHandle, pin::Pin, ptr::null_mut, sync::OnceLock,
        task::Poll,
    };

    use compio::{
        buf::{IntoInner, IoBuf, IoBufMut},
        driver::{
            op::{BufResultExt, Recv, Send, Sync},
            OpCode, RawFd,
        },
        io::{AsyncRead, AsyncWrite},
        runtime::Runtime,
        BufResult,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    struct SyncRead<B: IoBufMut> {
        op: Recv<B>,
    }

    impl<B: IoBufMut> SyncRead<B> {
        pub fn new(fd: RawFd, buffer: B) -> Self {
            Self {
                op: Recv::new(fd, buffer),
            }
        }
    }

    impl<B: IoBufMut> OpCode for SyncRead<B> {
        fn is_overlapped(&self) -> bool {
            false
        }

        unsafe fn operate(
            self: Pin<&mut Self>,
            _optr: *mut OVERLAPPED,
        ) -> Poll<std::io::Result<usize>> {
            self.map_unchecked_mut(|this| &mut this.op)
                .operate(null_mut())
        }
    }

    impl<B: IoBufMut> IntoInner for SyncRead<B> {
        type Inner = B;

        fn into_inner(self) -> Self::Inner {
            self.op.into_inner()
        }
    }

    struct SyncWrite<B: IoBuf> {
        op: Send<B>,
    }

    impl<B: IoBuf> SyncWrite<B> {
        pub fn new(fd: RawFd, buffer: B) -> Self {
            Self {
                op: Send::new(fd, buffer),
            }
        }
    }

    impl<B: IoBuf> OpCode for SyncWrite<B> {
        fn is_overlapped(&self) -> bool {
            false
        }

        unsafe fn operate(
            self: Pin<&mut Self>,
            _optr: *mut OVERLAPPED,
        ) -> Poll<std::io::Result<usize>> {
            self.map_unchecked_mut(|this| &mut this.op)
                .operate(null_mut())
        }
    }

    impl<B: IoBuf> IntoInner for SyncWrite<B> {
        type Inner = B;

        fn into_inner(self) -> Self::Inner {
            self.op.into_inner()
        }
    }

    static STDIN_ISATTY: OnceLock<bool> = OnceLock::new();

    pub struct Stdin {
        fd: RawFd,
        isatty: bool,
    }

    pub fn stdin() -> Stdin {
        let stdin = std::io::stdin();
        let isatty = *STDIN_ISATTY.get_or_init(|| {
            stdin.is_terminal() || Runtime::current().attach(stdin.as_raw_handle()).is_err()
        });
        Stdin {
            fd: stdin.as_raw_handle() as _,
            isatty,
        }
    }

    impl AsyncRead for Stdin {
        async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
            let runtime = Runtime::current();
            if self.isatty {
                let op = SyncRead::new(self.fd, buf);
                runtime.submit(op).await.into_inner()
            } else {
                let op = Recv::new(self.fd, buf);
                runtime.submit(op).await.into_inner()
            }
            .map_advanced()
        }
    }

    static STDOUT_ISATTY: OnceLock<bool> = OnceLock::new();

    pub struct Stdout {
        fd: RawFd,
        isatty: bool,
    }

    pub fn stdout() -> Stdout {
        let stdout = std::io::stdout();
        let isatty = *STDOUT_ISATTY.get_or_init(|| {
            stdout.is_terminal() || Runtime::current().attach(stdout.as_raw_handle()).is_err()
        });
        Stdout {
            fd: stdout.as_raw_handle() as _,
            isatty,
        }
    }

    impl AsyncWrite for Stdout {
        async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
            let runtime = Runtime::current();
            if self.isatty {
                let op = SyncWrite::new(self.fd, buf);
                runtime.submit(op).await.into_inner()
            } else {
                let op = Send::new(self.fd, buf);
                runtime.submit(op).await.into_inner()
            }
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            if !self.isatty {
                let op = Sync::new(self.fd, true);
                Runtime::current().submit(op).await.0?;
            }
            Ok(())
        }

        async fn shutdown(&mut self) -> std::io::Result<()> {
            self.flush().await
        }
    }
}

#[cfg(unix)]
mod stdio {
    use std::mem::ManuallyDrop;

    use compio::{
        buf::{IoBuf, IoBufMut},
        driver::AsRawFd,
        fs::pipe::{Receiver, Sender},
        io::{AsyncRead, AsyncWrite},
        runtime::FromRawFd,
        BufResult,
    };

    pub struct Stdin(ManuallyDrop<Receiver>);

    pub fn stdin() -> Stdin {
        Stdin(ManuallyDrop::new(unsafe {
            Receiver::from_raw_fd(std::io::stdin().as_raw_fd())
        }))
    }

    impl AsyncRead for Stdin {
        async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
            self.0.read(buf).await
        }
    }

    pub struct Stdout(ManuallyDrop<Sender>);

    pub fn stdout() -> Stdout {
        Stdout(ManuallyDrop::new(unsafe {
            Sender::from_raw_fd(std::io::stdout().as_raw_fd())
        }))
    }

    impl AsyncWrite for Stdout {
        async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T> {
            self.0.write(buf).await
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush().await
        }

        async fn shutdown(&mut self) -> std::io::Result<()> {
            self.0.shutdown().await
        }
    }
}
