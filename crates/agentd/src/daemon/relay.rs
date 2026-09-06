//! Bounded, opaque restricted-protocol relay over the engine CLI's attached streams.
use agentdocker_core::AgentRecord;
use agentdocker_host::containers::ContainerError;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};

const FRAME: usize = 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Envelope {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    close: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    ready: bool,
}
async fn line(reader: &mut (impl AsyncBufRead + Unpin), max: usize) -> io::Result<String> {
    let mut line = String::new();
    (&mut *reader)
        .take((max + 1) as u64)
        .read_line(&mut line)
        .await?;
    if line.len() > max || !line.ends_with('\n') {
        return Err(io::Error::other("invalid relay frame length"));
    }
    Ok(line)
}
// Keep partial bytes outside the select future: other ready branches may cancel it.
async fn envelope_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    buffer: &mut Vec<u8>,
) -> io::Result<String> {
    let max = 8 * FRAME;
    (&mut *reader)
        .take((max + 1 - buffer.len()) as u64)
        .read_until(b'\n', buffer)
        .await?;
    if buffer.len() > max || !buffer.ends_with(b"\n") {
        return Err(io::Error::other("invalid relay envelope length"));
    }
    String::from_utf8(std::mem::take(buffer)).map_err(io::Error::other)
}
pub(super) fn needs_cleanup(record: &AgentRecord) -> bool {
    record
        .container
        .as_ref()
        .and_then(|c| c.workspace.as_ref())
        .and_then(|w| w.access.as_ref())
        .and_then(|a| a.relay.as_ref())
        .is_some_and(|r| !r.retired)
}
pub(super) async fn start(
    record: AgentRecord,
    target: PathBuf,
) -> Result<JoinHandle<()>, ContainerError> {
    let prepared = record.clone();
    let args = tokio::task::spawn_blocking(move || agentdocker_host::relay::start_args(&prepared))
        .await
        .map_err(|e| ContainerError(e.to_string()))??;
    let mut child = tokio::process::Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ContainerError(e.to_string()))?;
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let ready = tokio::time::timeout(Duration::from_secs(15), line(&mut stdout, 1024))
        .await
        .map_err(|_| ContainerError("engine socket relay did not become ready".into()))?
        .map_err(|_| ContainerError("engine socket relay exited before readiness".into()))?;
    let ready: Envelope = serde_json::from_str(&ready)
        .map_err(|_| ContainerError("invalid engine relay readiness response".into()))?;
    if !ready.ready || !ready.id.is_empty() || ready.frame.is_some() {
        return Err(ContainerError(
            "invalid engine relay readiness response".into(),
        ));
    }
    tokio::task::spawn_blocking(move || agentdocker_host::relay::verify_helper(&record))
        .await
        .map_err(|e| ContainerError(e.to_string()))??;
    let mut stdin = child.stdin.take().unwrap();
    Ok(tokio::spawn(async move {
        let (output, mut responses) = mpsc::channel::<Envelope>(32);
        let mut peers: HashMap<String, mpsc::Sender<String>> = HashMap::new();
        let mut tasks = JoinSet::new();
        let mut pending_line = Vec::new();
        loop {
            tokio::select! {
                raw=envelope_line(&mut stdout,&mut pending_line)=> {
                    let Ok(raw)=raw else {tracing::warn!("relay CLI stream ended");break;};
                    let Ok(message)=serde_json::from_str::<Envelope>(&raw) else {tracing::warn!("invalid relay envelope");break;};
                    if message.id.len()!=32 || !message.id.bytes().all(|b|b.is_ascii_hexdigit()) || message.ready {break;}

                    if message.close {peers.remove(&message.id);continue;}
                    let Some(frame)=message.frame.filter(|f|f.len()<=FRAME && f.ends_with('\n')) else {break;};
                    if !peers.contains_key(&message.id) {
                        if peers.len()>=32 {break;}
                        let (input,mut requests)=mpsc::channel::<String>(2);
                        peers.insert(message.id.clone(),input);
                        let id=message.id.clone();let target=target.clone();let output=output.clone();
                        tasks.spawn(async move {
                            let outcome=tokio::time::timeout(Duration::from_secs(30),async {
                                let mut upstream=BufReader::new(UnixStream::connect(target).await?);
                                for _ in 0..2 {
                                    let Some(frame)=requests.recv().await else {return Ok::<_,io::Error>(());};
                                    upstream.get_mut().write_all(frame.as_bytes()).await?;
                                    let response=line(&mut upstream,FRAME).await?;
                                    output.send(Envelope {id:id.clone(),frame:Some(response),close:false,ready:false}).await.map_err(io::Error::other)?;
                                }
                                Ok(())
                            }).await;
                            if !matches!(outcome,Ok(Ok(()))) {
                                let _=output.send(Envelope {id:id.clone(),frame:None,close:true,ready:false}).await;
                            }
                            id
                        });
                    }
                    if peers.get(&message.id).unwrap().try_send(frame).is_err() {break;}
                }
                Some(response)=responses.recv()=> {

                    let Ok(mut frame)=serde_json::to_vec(&response) else {break;};frame.push(b'\n');
                    if !matches!(tokio::time::timeout(Duration::from_secs(30),stdin.write_all(&frame)).await,Ok(Ok(()))) {break;}
                }
                Some(done)=tasks.join_next(),if !tasks.is_empty()=> {
                    if let Ok(id)=done {peers.remove(&id);}
                }
                status=child.wait()=>{tracing::warn!(?status,"relay CLI exited");break;},
            }
        }
        tasks.abort_all();
        // Closing the attached stream makes the helper exit. Recovery additionally
        // verifies and replaces an orphan by its exact owned engine identity.
        drop(stdin);
        let _ = child.kill().await;
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_envelope_read_keeps_partial_bytes_and_checks_the_complete_limit() {
        let (mut writer, reader) = tokio::io::duplex(16384);
        let mut reader = BufReader::new(reader);
        let mut pending = Vec::new();
        writer.write_all(b"{\"id\":\"partial").await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                envelope_line(&mut reader, &mut pending)
            )
            .await
            .is_err()
        );
        assert!(!pending.is_empty());
        writer.write_all(b"\"}\n").await.unwrap();
        assert_eq!(
            envelope_line(&mut reader, &mut pending).await.unwrap(),
            "{\"id\":\"partial\"}\n"
        );
        assert!(pending.is_empty());
        pending = vec![b'x'; 8 * FRAME];
        writer.write_all(b"x\n").await.unwrap();
        assert!(envelope_line(&mut reader, &mut pending).await.is_err());
    }
}
