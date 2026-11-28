use crate::{Client, Message, Result};
use dim_agent_types::native_files::{Header, ImportedFile, READY, UPLOAD};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

impl Client {
    pub async fn upload_session_file(
        &mut self,
        header: Header,
        mut source: impl AsyncRead + Unpin,
    ) -> Result<ImportedFile> {
        if !self.supports(&[UPLOAD]) {
            return Err("The target does not support attachment uploads".into());
        }
        let mut channel = Self::connect(self.connection_config()).await?;
        if channel.instance_id() != self.instance_id() || !channel.supports(&[UPLOAD]) {
            return Err("The target changed before the file could be uploaded".into());
        }
        if header.byte_length == 0 && source.read(&mut [0; 1]).await? != 0 {
            return Err("The source file changed during upload".into());
        }
        let id = channel.send(UPLOAD, serde_json::to_value(&header)?).await?;
        loop {
            match channel.next().await? {
                Message::Notice { method, .. } if method == READY => break,
                Message::Notice { .. } => continue,
                Message::Response {
                    id: received,
                    result: Err(error),
                } if id == received => return Err(error.into()),
                _ => return Err("Invalid upload admission reply".into()),
            }
        }
        let send = async {
            let mut remaining = header.byte_length;
            let mut buffer = vec![0u8; 64 * 1024];
            while remaining > 0 {
                let take = remaining.min(buffer.len() as u64) as usize;
                source.read_exact(&mut buffer[..take]).await?;
                remaining -= take as u64;
                if remaining == 0 && source.read(&mut [0u8; 1]).await? != 0 {
                    return Err("The source file changed during upload".into());
                }
                channel.write.write_all(&buffer[..take]).await?;
            }
            channel.write.flush().await?;
            Ok::<_, crate::Error>(())
        };
        let receipt = async {
            loop {
                match Self::read_message(&mut channel.read, &mut channel.frame)
                    .await
                    .map_err(|e| {
                        format!("Upload receipt unavailable: {e}; the file may already be stored")
                    })? {
                    Message::Notice { .. } => {}
                    Message::Response {
                        id: received,
                        result,
                    } if received == id => return result.map_err(crate::Error::from),
                    _ => return Err("Invalid upload receipt".into()),
                }
            }
        };
        tokio::pin!(receipt);
        let reply = tokio::select! {
            biased;
            result = &mut receipt => result?,
            result = send => {result?; receipt.await?}
        };
        Ok(serde_json::from_value(reply)?)
    }
}
