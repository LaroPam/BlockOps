use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{collections::VecDeque, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, oneshot, watch},
};

#[derive(Clone, Debug, Serialize)]
pub struct ProcessStatus {
    pub phase: String,
    pub logs: VecDeque<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}
impl Default for ProcessStatus {
    fn default() -> Self {
        Self {
            phase: "starting".into(),
            logs: VecDeque::new(),
            exit_code: None,
            error: None,
        }
    }
}
pub struct Process {
    pub generation: String,
    pub status: watch::Receiver<ProcessStatus>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    console: Console,
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
#[derive(Clone)]
pub struct Console(mpsc::Sender<(String, oneshot::Sender<Result<(), String>>)>);
impl Console {
    pub async fn send(&self, input: &str) -> Result<()> {
        let command = input.trim();
        ensure!(
            !command.is_empty() && command.len() <= 1024 && !command.chars().any(char::is_control),
            "Команда должна быть одной строкой длиной до 1024 байт"
        );
        let command = command.strip_prefix('/').unwrap_or(command);
        let first = command.split_whitespace().next().unwrap_or("");
        let first = first.strip_prefix("minecraft:").unwrap_or(first);
        ensure!(
            !first.eq_ignore_ascii_case("stop"),
            "Для остановки используйте кнопку «Остановить»"
        );
        let (reply, response) = oneshot::channel();
        self.0
            .try_send((command.to_owned(), reply))
            .map_err(|_| anyhow::anyhow!("Консоль недоступна или очередь команд заполнена"))?;
        tokio::time::timeout(Duration::from_secs(3), response)
            .await
            .context("Не удалось подтвердить доставку команды вовремя; проверьте журнал")??
            .map_err(anyhow::Error::msg)
    }
}
impl Process {
    pub fn console(&self) -> Console {
        self.console.clone()
    }
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
async fn lines(mut reader: impl AsyncRead + Unpin, tx: mpsc::Sender<String>) {
    let mut pending = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for byte in &chunk[..n] {
            if *byte == b'\n' || pending.len() >= 8192 {
                if tx
                    .send(
                        String::from_utf8_lossy(&pending)
                            .trim_end_matches('\r')
                            .to_owned(),
                    )
                    .await
                    .is_err()
                {
                    return;
                }
                pending.clear();
            }
            if *byte != b'\n' {
                pending.push(*byte);
            }
        }
    }
    if !pending.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&pending).into()).await;
    }
}
/// Owns the process, drains both pipes, bounds logs and performs graceful stop before forced termination.
pub fn spawn(mut command: Command, lock: std::fs::File, stop_timeout: Duration) -> Result<Process> {
    ensure!(stop_timeout.as_secs() > 0, "Invalid stop timeout");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command.spawn().context("Cannot start Java process")?;
    let mut stdin = child.stdin.take().context("Missing stdin")?;
    let stdout = child.stdout.take().context("Missing stdout")?;
    let stderr = child.stderr.take().context("Missing stderr")?;
    let (status_tx, status) = watch::channel(ProcessStatus::default());
    let (stop, mut stopping) = oneshot::channel();
    let (console_tx, mut console_rx) =
        mpsc::channel::<(String, oneshot::Sender<Result<(), String>>)>(16);
    let task = tokio::spawn(async move {
        let _lock = lock;
        let (lines_tx, mut lines_rx) = mpsc::channel(64);
        let mut readers = tokio::task::JoinSet::new();
        readers.spawn(lines(stdout, lines_tx.clone()));
        readers.spawn(lines(stderr, lines_tx));
        let mut deadline = None;
        loop {
            tokio::select! {
                biased;
                _ = &mut stopping, if deadline.is_none() => {
                    status_tx.send_modify(|s| s.phase = "stopping".into());
                    let _ = tokio::time::timeout(Duration::from_secs(2), stdin.write_all(b"stop\n")).await;
                    deadline = Some(tokio::time::Instant::now() + stop_timeout);
                }
                _ = async { if let Some(d) = deadline { tokio::time::sleep_until(d).await; } else { std::future::pending::<()>().await; } } => {
                    let _ = child.kill().await;
                    status_tx.send_modify(|s| { s.phase = "failed".into(); s.error = Some("Graceful shutdown timed out; server was terminated. Check world integrity.".into()); });
                    break;
                }
                Some((command, reply)) = console_rx.recv(), if deadline.is_none() => {
                    if reply.is_closed() { continue; }
                    let result = tokio::time::timeout(Duration::from_secs(2), stdin.write_all(format!("{command}\n").as_bytes())).await;
                    let response = match result { Ok(Ok(())) => Ok(()), Ok(Err(e)) => Err(format!("Ошибка записи в консоль: {e}")), Err(_) => Err("Время записи команды истекло".into()) };
                    let failed = response.is_err();
                    let _ = reply.send(response);
                    if failed {
                        let _ = child.kill().await;
                        status_tx.send_modify(|s| { s.phase = "failed".into(); s.error = Some("Console input failed; server was terminated. Check world integrity.".into()); });
                        break;
                    }
                }
                result = child.wait() => {
                    status_tx.send_modify(|s| match result {
                        Ok(exit) => { s.exit_code = exit.code(); s.phase = if exit.success() { "stopped" } else { "failed" }.into(); if !exit.success() { s.error = Some(format!("Java exited: {exit}")); } },
                        Err(e) => { s.phase = "failed".into(); s.error = Some(e.to_string()); }
                    });
                    break;
                }
                Some(line) = lines_rx.recv() => status_tx.send_modify(|s| {
                    if s.phase == "starting" && line.contains("Done (") && line.contains("For help, type") { s.phase = "running".into(); }
                    if line.contains("Failed to request yggdrasil public key") {
                        s.error = Some("Minecraft cannot fetch authentication keys. Check direct access to api.minecraftservices.com; online-mode remains enabled.".into());
                    }
                    if s.logs.len() == 200 { s.logs.pop_front(); }
                    s.logs.push_back(line);
                }),
            }
        }
        // Drain the finite remaining output, retaining the exit state.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(line) = lines_rx.recv().await {
                status_tx.send_modify(|s| {
                    if s.logs.len() == 200 {
                        s.logs.pop_front();
                    }
                    s.logs.push_back(line);
                });
            }
        })
        .await;
        readers.abort_all();
        while readers.join_next().await.is_some() {}
    });
    Ok(Process {
        generation: uuid::Uuid::new_v4().to_string(),
        status,
        stop: Some(stop),
        task: Some(task),
        console: Console(console_tx),
    })
}
