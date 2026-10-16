//! The toolhost wire: one big-endian u32 length and a JSON body each way, one call per connection.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroize;

/// The largest frame either side reads.
pub const MAX_FRAME: usize = 64 << 20;

/// A tool invocation.
#[derive(Debug, Deserialize, Serialize)]
pub struct Call {
    #[serde(default)]
    pub agent_id: String,
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

/// A tool result; `image_b64` carries a screenshot when the tool produced one, and `secrets`
/// the card fields it typed from a file, as (field, value).
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Output {
    pub text: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<(String, String)>,
}

impl Output {
    /// A successful text result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// A failed result carrying `text` as the reason.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            ..Self::default()
        }
    }
}

/// Writes `msg` as a big-endian u32 length followed by JSON.
pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut body = serde_json::to_vec(msg).map_err(|e| format!("encode frame: {e}"))?;
    let len = u32::try_from(body.len()).map_err(|_| "frame too large".to_owned())?;
    let written = async {
        w.write_all(&len.to_be_bytes()).await?;
        w.write_all(&body).await?;
        w.flush().await
    }
    .await;
    body.zeroize();
    written.map_err(|e| format!("write frame: {e}"))
}

/// Reads one frame written by [`write_frame`].
pub async fn read_frame<R, T>(r: &mut R) -> Result<T, String>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    r.read_exact(&mut len)
        .await
        .map_err(|e| format!("read frame length: {e}"))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(format!("frame of {len} bytes exceeds {MAX_FRAME}"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)
        .await
        .map_err(|e| format!("read frame body: {e}"))?;
    let msg = serde_json::from_slice(&body);
    body.zeroize();
    msg.map_err(|e| format!("decode frame: {e}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn a_frame_round_trips() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let call = Call {
            agent_id: "desk".into(),
            tool: "desk_windows".into(),
            args: json!({"pid": 7}),
        };
        write_frame(&mut a, &call).await.unwrap();
        let got: Call = read_frame(&mut b).await.unwrap();
        assert_eq!(got.tool, "desk_windows");
        assert_eq!(got.args["pid"], 7);
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        assert!(read_frame::<_, Output>(&mut b).await.is_err());
    }

    #[test]
    fn the_wire_keeps_its_json_shape() {
        assert_eq!(
            serde_json::to_value(Output::text("[]")).unwrap(),
            json!({"text": "[]", "is_error": false})
        );
        let call: Call =
            serde_json::from_value(json!({"tool": "desk_snapshot", "args": {"text": true}}))
                .unwrap();
        assert_eq!(
            (call.agent_id.as_str(), call.args["text"].as_bool()),
            ("", Some(true))
        );
        let out: Output = serde_json::from_value(json!({"text": "x"})).unwrap();
        assert!(!out.is_error && out.image_b64.is_none() && out.secrets.is_empty());
    }
}
