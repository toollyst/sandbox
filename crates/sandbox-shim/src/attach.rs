//! 붙기(`sbx attach`) 규약. shim이 듣고, sbx가 붙는다. 크레이트의 가장 아래 모듈 중 하나다.
//!
//! - 소켓: [`socket_path`] (`<socket_dir>/<id>.io`, Unix 소켓). shim이 기본 프로세스를 시작한 뒤 듣고,
//!   끝날 때 지운다. Unix에서만 있다.
//! - 메시지 하나는 `[종류 1바이트][길이 u32 빅 엔디언][내용]`이다.
//! - 붙으면 shim이 먼저 [`ServerMsg::Hello`]를 보낸다. 그 뒤로 기본 프로세스의 출력이 나오는 대로 온다
//!   (붙기 전 출력은 오지 않는다. 그것은 `sbx logs`가 보여준다).
//!   기본 프로세스가 끝나면 [`ServerMsg::Exit`]가 마지막으로 온다.
//! - 붙은 쪽은 [`ClientMsg`]를 보낸다. 표준 입력은 기본 프로세스의 표준 입력이 열려 있을 때(`-i`)만 전한다.
//!   붙은 쪽이 끊거나 입력을 끝내도 기본 프로세스의 표준 입력은 닫지 않는다(Docker `run -d -i`와 같다).
//! - 여럿이 붙을 수 있다. 출력은 모두에게 가고, 입력은 섞인다. 터미널 크기는 마지막에 알린 것이다.
//! - 출력을 제때 읽지 않는 쪽은 [`QUEUE`]개가 쌓이면 끊는다(다른 쪽과 기본 프로세스를 막지 않는다).

use std::io;
use std::path::{Path, PathBuf};

use sandbox_model::SandboxId;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 붙은 쪽 하나에 쌓아 두는 출력 메시지 수.
pub const QUEUE: usize = 1024;

/// 메시지 내용의 최대 길이.
const MAX_LEN: u32 = 1024 * 1024;

/// 붙기 소켓 경로.
pub fn socket_path(socket_dir: &Path, id: &SandboxId) -> PathBuf {
    socket_dir.join(format!("{id}.io"))
}

/// shim → 붙은 쪽.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    /// 첫 메시지: 기본 프로세스의 입출력 방식.
    Hello {
        /// 표준 입력이 열려 있다(`-i`).
        stdin: bool,
        /// 터미널이 붙어 있다(`-t`). 출력은 [`ServerMsg::Stdout`]으로만 온다.
        tty: bool,
    },
    /// 표준 출력 (터미널이면 터미널 출력 전체).
    Stdout(Vec<u8>),
    /// 표준 오류.
    Stderr(Vec<u8>),
    /// 기본 프로세스가 끝났다. 종료 코드.
    Exit(i32),
}

/// 붙은 쪽 → shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    /// 표준 입력.
    Stdin(Vec<u8>),
    /// 터미널 크기 (줄, 열).
    Resize(u16, u16),
    /// 기본 프로세스에 보낼 시그널 (Linux 번호).
    Signal(i32),
}

async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, kind: u8, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("message too long"))?;
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(kind);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(body);
    w.write_all(&buf).await?;
    w.flush().await
}

/// 메시지 하나. 깨끗하게 끝났으면(메시지 사이에서 닫힘) `None`.
async fn read_msg<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let kind = match r.read_u8().await {
        Ok(k) => k,
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = r.read_u32().await?;
    if len > MAX_LEN {
        return Err(bad(format!("attach message of {len} bytes")));
    }
    let mut body = vec![0; len as usize];
    r.read_exact(&mut body).await?;
    Ok(Some((kind, body)))
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

fn i32_of(b: &[u8]) -> io::Result<i32> {
    let a: [u8; 4] = b.try_into().map_err(|_| bad("bad i32".into()))?;
    Ok(i32::from_be_bytes(a))
}

impl ServerMsg {
    /// 쓴다.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, w: &mut W) -> io::Result<()> {
        match self {
            ServerMsg::Hello { stdin, tty } => {
                write_msg(w, 1, &[u8::from(*stdin) | (u8::from(*tty) << 1)]).await
            }
            ServerMsg::Stdout(b) => write_msg(w, 2, b).await,
            ServerMsg::Stderr(b) => write_msg(w, 3, b).await,
            ServerMsg::Exit(c) => write_msg(w, 4, &c.to_be_bytes()).await,
        }
    }

    /// 읽는다. 닫혔으면 `None`.
    pub async fn read_from<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Self>> {
        let Some((kind, b)) = read_msg(r).await? else {
            return Ok(None);
        };
        Ok(Some(match (kind, b.as_slice()) {
            (1, [flags]) => ServerMsg::Hello {
                stdin: flags & 1 != 0,
                tty: flags & 2 != 0,
            },
            (2, _) => ServerMsg::Stdout(b),
            (3, _) => ServerMsg::Stderr(b),
            (4, _) => ServerMsg::Exit(i32_of(&b)?),
            (k, _) => return Err(bad(format!("unknown attach message {k}"))),
        }))
    }
}

impl ClientMsg {
    /// 쓴다.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, w: &mut W) -> io::Result<()> {
        match self {
            ClientMsg::Stdin(b) => write_msg(w, 1, b).await,
            ClientMsg::Resize(rows, cols) => {
                let mut b = rows.to_be_bytes().to_vec();
                b.extend_from_slice(&cols.to_be_bytes());
                write_msg(w, 2, &b).await
            }
            ClientMsg::Signal(n) => write_msg(w, 3, &n.to_be_bytes()).await,
        }
    }

    /// 읽는다. 닫혔으면 `None`.
    pub async fn read_from<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Self>> {
        let Some((kind, b)) = read_msg(r).await? else {
            return Ok(None);
        };
        Ok(Some(match (kind, b.as_slice()) {
            (1, _) => ClientMsg::Stdin(b),
            (2, [r0, r1, c0, c1]) => ClientMsg::Resize(
                u16::from_be_bytes([*r0, *r1]),
                u16::from_be_bytes([*c0, *c1]),
            ),
            (3, _) => ClientMsg::Signal(i32_of(&b)?),
            (k, _) => return Err(bad(format!("unknown attach message {k}"))),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = [
                ServerMsg::Hello {
                    stdin: true,
                    tty: false,
                },
                ServerMsg::Hello {
                    stdin: false,
                    tty: true,
                },
                ServerMsg::Stdout(b"out".to_vec()),
                ServerMsg::Stderr(vec![]),
                ServerMsg::Exit(-3),
            ];
            let mut buf = Vec::new();
            for m in &server {
                m.write_to(&mut buf).await.unwrap();
            }
            let mut r = buf.as_slice();
            for m in &server {
                assert_eq!(
                    ServerMsg::read_from(&mut r).await.unwrap().as_ref(),
                    Some(m)
                );
            }
            assert_eq!(ServerMsg::read_from(&mut r).await.unwrap(), None);

            let client = [
                ClientMsg::Stdin(b"in".to_vec()),
                ClientMsg::Resize(24, 80),
                ClientMsg::Signal(15),
            ];
            let mut buf = Vec::new();
            for m in &client {
                m.write_to(&mut buf).await.unwrap();
            }
            let mut r = buf.as_slice();
            for m in &client {
                assert_eq!(
                    ClientMsg::read_from(&mut r).await.unwrap().as_ref(),
                    Some(m)
                );
            }
            // 중간에 잘리면 오류.
            let mut r = &buf[..3];
            assert!(ClientMsg::read_from(&mut r).await.is_err());
        });
    }
}
