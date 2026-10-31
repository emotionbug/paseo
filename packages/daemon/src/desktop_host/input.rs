use std::{
    io::{self, Read},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::mpsc,
};

/// Tokio's stdin uses a runtime-owned blocking task, which cannot be cancelled
/// and can prevent the dedicated helper process from exiting after `close`.
/// This detached reader is process-scoped instead; it never owns a daemon.
pub(crate) struct Input {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Option<(Vec<u8>, usize)>,
}
impl Input {
    pub fn open() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel(1);
        std::thread::Builder::new()
            .name("desktop-helper-stdin".into())
            .spawn(move || {
                let stdin = io::stdin();
                let mut stdin = stdin.lock();
                loop {
                    let mut bytes = vec![0; 8192];
                    match stdin.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(count) => {
                            bytes.truncate(count);
                            if sender.blocking_send(Ok(bytes)).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            let _ = sender.blocking_send(Err(error));
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            receiver,
            pending: None,
        })
    }
}
impl AsyncRead for Input {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if let Some((bytes, offset)) = &mut self.pending {
                let count = output.remaining().min(bytes.len() - *offset);
                output.put_slice(&bytes[*offset..*offset + count]);
                *offset += count;
                if *offset == bytes.len() {
                    self.pending = None;
                }
                return Poll::Ready(Ok(()));
            }
            match self.receiver.poll_recv(context) {
                Poll::Ready(Some(Ok(bytes))) => self.pending = Some((bytes, 0)),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
