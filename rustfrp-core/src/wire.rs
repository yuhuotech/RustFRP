//! 线协议：控制流上的「4 字节大端长度 + JSON」帧，以及数据流的起始消息。

use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::TunnelError;

/// 单帧最大长度（控制消息很小，限制可防止恶意对端撑爆内存）。
pub const MAX_FRAME: usize = 64 * 1024;
/// 协议版本。
pub const PROTOCOL_VERSION: u32 = 1;

/// 登录请求（Client → Edge，控制流第一条）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// 协议版本。
    pub version: u32,
    /// frp 的 `user`：代理名前缀。
    pub user: String,
    /// 客户端标识。
    pub client_id: String,
    /// Unix 秒。
    pub timestamp: i64,
    /// `auth_key(token, timestamp)`。
    pub auth: String,
    /// 元数据。
    #[serde(default)]
    pub metas: BTreeMap<String, String>,
}

/// 代理注册信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySpec {
    /// 代理名（已带 user 前缀）。
    pub name: String,
    /// `tcp` / `udp`。
    pub kind: String,
    /// 远程端口，0 = 由服务端从允许范围内分配。
    pub remote_port: u16,
}

/// Client → Edge。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 登录。
    Login(Login),
    /// 注册代理。
    NewProxy(ProxySpec),
    /// 心跳。
    Ping,
}

/// Edge → Client。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// 登录结果。
    LoginResp {
        /// 成功与否。
        ok: bool,
        /// 失败原因。
        error: String,
        /// 本次会话标识。
        run_id: String,
    },
    /// 代理注册结果。
    NewProxyResp {
        /// 代理名。
        name: String,
        /// 实际监听端口（失败为 0）。
        remote_port: u16,
        /// 失败原因，空表示成功。
        error: String,
    },
    /// 心跳应答。
    Pong,
}

/// 数据流起始消息（Edge → Client，每条新流的第一帧）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartWork {
    /// 代理名。
    pub proxy: String,
    /// 访问者地址。
    pub src: String,
}

/// 写一帧。
pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), TunnelError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(msg).map_err(|e| TunnelError::Wire(e.to_string()))?;
    if body.len() > MAX_FRAME {
        return Err(TunnelError::Wire(format!(
            "frame too large: {}",
            body.len()
        )));
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// 读一帧。
pub async fn read_frame<R, T>(r: &mut R) -> Result<T, TunnelError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(TunnelError::Wire(format!("frame too large: {len}")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|e| TunnelError::Wire(e.to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn login() -> Login {
        Login {
            version: PROTOCOL_VERSION,
            user: "u".into(),
            client_id: "c".into(),
            timestamp: 1,
            auth: "a".into(),
            metas: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn roundtrip_all_messages() {
        let (mut a, mut b) = duplex(4096);
        let c = [
            ClientMsg::Login(login()),
            ClientMsg::NewProxy(ProxySpec {
                name: "n".into(),
                kind: "tcp".into(),
                remote_port: 6000,
            }),
            ClientMsg::Ping,
        ];
        for m in &c {
            write_frame(&mut a, m).await.unwrap();
            assert_eq!(&read_frame::<_, ClientMsg>(&mut b).await.unwrap(), m);
        }
        let s = [
            ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
            },
            ServerMsg::NewProxyResp {
                name: "n".into(),
                remote_port: 1,
                error: String::new(),
            },
            ServerMsg::Pong,
        ];
        for m in &s {
            write_frame(&mut a, m).await.unwrap();
            assert_eq!(&read_frame::<_, ServerMsg>(&mut b).await.unwrap(), m);
        }
        let w = StartWork {
            proxy: "p".into(),
            src: "1.2.3.4:5".into(),
        };
        write_frame(&mut a, &w).await.unwrap();
        assert_eq!(read_frame::<_, StartWork>(&mut b).await.unwrap(), w);
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected_both_ways() {
        let (mut a, mut b) = duplex(1 << 20);
        let big = StartWork {
            proxy: "x".repeat(MAX_FRAME),
            src: String::new(),
        };
        assert!(matches!(
            write_frame(&mut a, &big).await,
            Err(TunnelError::Wire(_))
        ));
        a.write_all(&((MAX_FRAME as u32 + 1).to_be_bytes()))
            .await
            .unwrap();
        assert!(matches!(
            read_frame::<_, StartWork>(&mut b).await,
            Err(TunnelError::Wire(_))
        ));
    }

    #[tokio::test]
    async fn malformed_and_truncated_input() {
        let (mut a, mut b) = duplex(4096);
        a.write_all(&3u32.to_be_bytes()).await.unwrap();
        a.write_all(b"{{{").await.unwrap();
        assert!(matches!(
            read_frame::<_, StartWork>(&mut b).await,
            Err(TunnelError::Wire(_))
        ));
        a.write_all(&10u32.to_be_bytes()).await.unwrap();
        a.write_all(b"ab").await.unwrap();
        drop(a);
        assert!(matches!(
            read_frame::<_, StartWork>(&mut b).await,
            Err(TunnelError::Io(_))
        ));
    }

    #[tokio::test]
    async fn write_to_closed_peer_fails() {
        let (mut a, b) = duplex(8);
        drop(b);
        assert!(matches!(
            write_frame(&mut a, &ClientMsg::Ping).await,
            Err(TunnelError::Io(_))
        ));
    }
}
