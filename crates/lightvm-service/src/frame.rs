//! Framing a lunghezza prefissata: 4 byte big-endian di lunghezza, poi il
//! payload JSON. Lunghezza zero o superiore al limite → errore, senza leggere
//! il payload (nessuna allocazione guidata dal peer oltre il limite).

use std::io;

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Limite predefinito di byte per frame (payload).
pub const DEFAULT_MAX_FRAME: u32 = 64 * 1024;

/// Errore di lettura di un frame.
#[derive(Debug)]
pub enum FrameError {
    /// Il peer ha chiuso la connessione in modo pulito tra due frame.
    Closed,
    /// Lunghezza dichiarata oltre il limite.
    TooLarge {
        /// Lunghezza dichiarata.
        declared: u32,
        /// Limite.
        max: u32,
    },
    /// Lunghezza zero.
    Empty,
    /// Payload non decodificabile.
    Malformed(serde_json::Error),
    /// Errore di I/O.
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "connessione chiusa"),
            Self::TooLarge { declared, max } => {
                write!(f, "frame di {declared} byte oltre il limite di {max}")
            }
            Self::Empty => write!(f, "frame vuoto"),
            Self::Malformed(e) => write!(f, "messaggio non valido: {e}"),
            Self::Io(e) => write!(f, "I/O: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Legge il payload grezzo di un frame.
pub async fn read_frame_bytes<R: AsyncRead + Unpin>(
    r: &mut R,
    max: u32,
) -> Result<Vec<u8>, FrameError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) if is_disconnect(&e) => return Err(FrameError::Closed),
        Err(e) => return Err(FrameError::Io(e)),
    }
    let declared = u32::from_be_bytes(len);
    if declared == 0 {
        return Err(FrameError::Empty);
    }
    if declared > max {
        return Err(FrameError::TooLarge { declared, max });
    }
    let mut buf = vec![0u8; declared as usize];
    r.read_exact(&mut buf).await.map_err(FrameError::Io)?;
    Ok(buf)
}

/// Legge e decodifica un frame JSON.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
    max: u32,
) -> Result<T, FrameError> {
    let buf = read_frame_bytes(r, max).await?;
    serde_json::from_slice(&buf).map_err(FrameError::Malformed)
}

/// Codifica e scrive un frame JSON. Rifiuta payload oltre `max`.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    msg: &T,
    max: u32,
) -> io::Result<()> {
    let payload = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame_bytes(w, &payload, max).await
}

/// Scrive un payload grezzo come frame.
pub async fn write_frame_bytes<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
    max: u32,
) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|l| *l <= max && *l > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame fuori limite"))?;
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    w.write_all(&out).await?;
    w.flush().await
}

/// Errori che equivalgono a una chiusura del peer (pipe rotta, reset).
pub(crate) fn is_disconnect(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &serde_json::json!({"x": 1}), 100)
            .await
            .unwrap();
        let v: serde_json::Value = read_frame(&mut b, 100).await.unwrap();
        assert_eq!(v["x"], 1);
    }

    #[tokio::test]
    async fn too_large_is_rejected_before_reading_payload() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        match read_frame_bytes(&mut b, 1024).await {
            Err(FrameError::TooLarge { declared, max }) => {
                assert_eq!(declared, u32::MAX);
                assert_eq!(max, 1024);
            }
            other => panic!("atteso TooLarge, ottenuto {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_and_closed() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&0u32.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_frame_bytes(&mut b, 10).await,
            Err(FrameError::Empty)
        ));
        drop(a);
        assert!(matches!(
            read_frame_bytes(&mut b, 10).await,
            Err(FrameError::Closed)
        ));
    }

    #[tokio::test]
    async fn writer_refuses_oversized() {
        let (mut a, _b) = tokio::io::duplex(1024);
        let big = "x".repeat(200);
        assert!(write_frame(&mut a, &big, 100).await.is_err());
    }
}
