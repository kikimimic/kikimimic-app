use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use kikimimic_core::scheduler::{StepOutput, Utterance};
use kikimimic_core::Recognizer;
use serde::Deserialize;
use serde_json::json;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

const URL: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
const FINISH_TIMEOUT: Duration = Duration::from_secs(10);

pub fn api_key() -> Option<String> {
    std::env::var("SONIOX_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| {
            std::env::var("API_KEY")
                .ok()
                .filter(|key| !key.trim().is_empty())
        })
}

enum Command {
    Audio(Vec<u8>),
    Finish,
}

enum WorkerEvent {
    Response(Response),
    Error(String),
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    tokens: Vec<Token>,
    #[serde(default)]
    finished: bool,
    error_code: Option<u16>,
    error_type: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Token {
    text: String,
    #[serde(default)]
    is_final: bool,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    language: Option<String>,
}

pub struct SonioxRecognizer {
    tx: Sender<Command>,
    rx: Receiver<WorkerEvent>,
    reducer: Reducer,
    send_error: Option<String>,
}

impl SonioxRecognizer {
    pub fn new(api_key: String, spoken: Vec<String>, terms: Vec<String>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Err(error) = run_worker(&api_key, &spoken, &terms, cmd_rx, &event_tx) {
                let _ = event_tx.send(WorkerEvent::Error(format!("{error:#}")));
            }
        });
        Self {
            tx: cmd_tx,
            rx: event_rx,
            reducer: Reducer::default(),
            send_error: None,
        }
    }

    fn collect(&mut self) -> Result<()> {
        if let Some(error) = &self.send_error {
            anyhow::bail!(error.clone());
        }
        while let Ok(event) = self.rx.try_recv() {
            if let Err(error) = self.apply(event) {
                self.send_error = Some(format!("{error:#}"));
                break;
            }
        }
        if let Some(error) = &self.send_error {
            anyhow::bail!(error.clone());
        }
        Ok(())
    }

    fn apply(&mut self, event: WorkerEvent) -> Result<()> {
        match event {
            WorkerEvent::Response(response) => {
                if let Some(code) = response.error_code {
                    anyhow::bail!(
                        "Soniox {code} {}: {}",
                        response.error_type.as_deref().unwrap_or("error"),
                        response
                            .error_message
                            .as_deref()
                            .unwrap_or("request failed")
                    );
                }
                self.reducer.ingest(response);
                Ok(())
            }
            WorkerEvent::Error(error) => anyhow::bail!("Soniox connection failed: {error}"),
        }
    }
}

impl Recognizer for SonioxRecognizer {
    fn push_audio(&mut self, samples: &[f32]) {
        if samples.is_empty() || self.send_error.is_some() {
            return;
        }
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            pcm.extend_from_slice(&value.to_le_bytes());
        }
        if self.tx.send(Command::Audio(pcm)).is_err() {
            self.send_error = Some("Soniox audio worker stopped".into());
        }
    }

    fn step(&mut self, _lang: Option<&str>) -> Result<Option<StepOutput>> {
        self.collect()?;
        Ok(self.reducer.next_output())
    }

    fn finish(&mut self, _lang: Option<&str>) -> Result<Option<StepOutput>> {
        self.tx
            .send(Command::Finish)
            .context("Soniox audio worker stopped before finish")?;
        let deadline = Instant::now() + FINISH_TIMEOUT;
        while !self.reducer.finished && Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => self.apply(event)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if !self.reducer.finished {
            anyhow::bail!("Soniox did not finish within 10 seconds");
        }
        self.reducer.finalize();
        Ok(self.reducer.next_output())
    }
}

impl Drop for SonioxRecognizer {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Finish);
    }
}

fn run_worker(
    api_key: &str,
    spoken: &[String],
    terms: &[String],
    commands: Receiver<Command>,
    events: &Sender<WorkerEvent>,
) -> Result<()> {
    let (mut socket, _) = connect(URL).context("connect websocket")?;
    set_read_timeout(&mut socket, Duration::from_millis(25))?;
    let mut config = json!({
        "api_key": api_key,
        "model": "stt-rt-v5",
        "audio_format": "pcm_s16le",
        "sample_rate": 16000,
        "num_channels": 1,
        "language_hints": spoken,
        "enable_language_identification": true,
        "enable_endpoint_detection": true,
        "max_endpoint_delay_ms": 1500
    });
    if !terms.is_empty() {
        config["context"] = json!({ "terms": terms });
    }
    socket.send(Message::Text(config.to_string().into()))?;

    let mut finishing = false;
    loop {
        while !finishing {
            match commands.try_recv() {
                Ok(Command::Audio(bytes)) => socket.send(Message::Binary(bytes.into()))?,
                Ok(Command::Finish) | Err(mpsc::TryRecvError::Disconnected) => {
                    socket.send(Message::Text("".into()))?;
                    finishing = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                let response: Response = serde_json::from_str(&text).context("parse response")?;
                let done = response.finished || response.error_code.is_some();
                events.send(WorkerEvent::Response(response)).ok();
                if done {
                    return Ok(());
                }
            }
            Ok(Message::Close(_)) => anyhow::bail!("websocket closed before the finished response"),
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(tungstenite::Error::ConnectionClosed) => {
                anyhow::bail!("websocket closed before the finished response")
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn set_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Duration,
) -> Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(timeout))?,
        MaybeTlsStream::NativeTls(stream) => stream.get_ref().set_read_timeout(Some(timeout))?,
        _ => anyhow::bail!("unsupported Soniox TLS stream"),
    }
    Ok(())
}

#[derive(Default)]
struct Reducer {
    utterance_text: String,
    utterance_start_ms: Option<u64>,
    utterance_end_ms: u64,
    utterance_lang: Option<String>,
    committed_delta: String,
    volatile: String,
    volatile_dirty: bool,
    outputs: VecDeque<StepOutput>,
    finished: bool,
}

impl Reducer {
    fn ingest(&mut self, response: Response) {
        if response.tokens.is_empty() && !response.finished {
            return;
        }
        let mut volatile = String::new();
        for token in response.tokens {
            if token.is_final && token.text == "<end>" {
                self.finalize();
                continue;
            }
            if token.is_final {
                self.utterance_start_ms = self.utterance_start_ms.or(token.start_ms);
                self.utterance_end_ms = token.end_ms.unwrap_or(self.utterance_end_ms);
                if token.language.is_some() {
                    self.utterance_lang = token.language;
                }
                self.utterance_text.push_str(&token.text);
                self.committed_delta.push_str(&token.text);
            } else {
                volatile.push_str(&token.text);
            }
        }
        self.volatile_dirty |= self.volatile != volatile;
        self.volatile = volatile;
        if response.finished {
            self.finished = true;
            self.finalize();
        }
    }

    fn finalize(&mut self) {
        let text = self.utterance_text.trim();
        if text.is_empty() {
            self.volatile.clear();
            self.volatile_dirty = true;
            return;
        }
        let utterance = Utterance {
            text: text.to_string(),
            start_ms: self.utterance_start_ms.unwrap_or(0),
            end_ms: self.utterance_end_ms,
            lang: self.utterance_lang.take(),
        };
        self.outputs.push_back(StepOutput {
            committed_delta: std::mem::take(&mut self.committed_delta),
            volatile: String::new(),
            utterance_final: Some(utterance),
        });
        self.utterance_text.clear();
        self.utterance_start_ms = None;
        self.utterance_end_ms = 0;
        self.volatile.clear();
        self.volatile_dirty = false;
    }

    fn next_output(&mut self) -> Option<StepOutput> {
        if let Some(output) = self.outputs.pop_front() {
            return Some(output);
        }
        if self.committed_delta.is_empty() && !self.volatile_dirty {
            return None;
        }
        self.volatile_dirty = false;
        Some(StepOutput {
            committed_delta: std::mem::take(&mut self.committed_delta),
            volatile: self.volatile.clone(),
            utterance_final: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a Soniox API key and network access"]
    fn live_connection_accepts_audio_and_context_terms() {
        let _ = dotenvy::dotenv();
        let key = api_key().expect("SONIOX_API_KEY or API_KEY");
        let mut recognizer =
            SonioxRecognizer::new(key, vec!["ja".into()], vec!["Claude Code".into()]);
        let wav = std::env::var("KKM_SONIOX_TEST_WAV").ok();
        let samples = if let Some(path) = &wav {
            hound::WavReader::open(path)
                .expect("test WAV")
                .samples::<i16>()
                .map(|sample| sample.expect("PCM sample") as f32 / i16::MAX as f32)
                .collect::<Vec<_>>()
        } else {
            vec![0.0; 16_000]
        };
        let mut transcript = String::new();
        for chunk in samples.chunks(1_600) {
            recognizer.push_audio(chunk);
            std::thread::sleep(Duration::from_millis(100));
            if let Some(out) = recognizer.step(None).expect("Soniox step") {
                if let Some(utterance) = out.utterance_final {
                    transcript.push_str(&utterance.text);
                }
            }
        }
        if let Some(out) = recognizer.finish(None).expect("Soniox finished response") {
            if let Some(utterance) = out.utterance_final {
                transcript.push_str(&utterance.text);
            }
        }
        while let Some(out) = recognizer.step(None).expect("Soniox remaining output") {
            if let Some(utterance) = out.utterance_final {
                transcript.push_str(&utterance.text);
            }
        }
        if wav.is_some() {
            assert!(!transcript.is_empty(), "Soniox returned no speech");
            eprintln!("Soniox smoke transcript: {transcript}");
        }
    }

    fn response(tokens: Vec<Token>, finished: bool) -> Response {
        Response {
            tokens,
            finished,
            error_code: None,
            error_type: None,
            error_message: None,
        }
    }

    fn token(text: &str, is_final: bool, start_ms: u64, end_ms: u64) -> Token {
        Token {
            text: text.into(),
            is_final,
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
            language: Some("ja".into()),
        }
    }

    #[test]
    fn endpoint_turns_final_tokens_into_an_utterance() {
        let mut reducer = Reducer::default();
        reducer.ingest(response(
            vec![
                token("こんにちは", true, 100, 600),
                token("<end>", true, 600, 600),
            ],
            false,
        ));
        let output = reducer.next_output().expect("endpoint output");
        assert_eq!(output.committed_delta, "こんにちは");
        assert_eq!(
            output.utterance_final.expect("utterance").text,
            "こんにちは"
        );
    }

    #[test]
    fn non_final_tokens_replace_the_volatile_tail() {
        let mut reducer = Reducer::default();
        reducer.ingest(response(vec![token("こん", false, 0, 100)], false));
        assert_eq!(reducer.next_output().expect("first").volatile, "こん");
        reducer.ingest(response(vec![token("こんにちは", false, 0, 300)], false));
        assert_eq!(
            reducer.next_output().expect("revision").volatile,
            "こんにちは"
        );
    }

    #[test]
    fn empty_progress_response_does_not_clear_the_volatile_tail() {
        let mut reducer = Reducer::default();
        reducer.ingest(response(vec![token("クロ", false, 0, 100)], false));
        reducer.next_output();
        reducer.ingest(response(vec![], false));
        assert!(reducer.next_output().is_none());
        reducer.ingest(response(
            vec![
                token("クロード", true, 0, 500),
                token("<end>", true, 500, 500),
            ],
            false,
        ));
        assert_eq!(
            reducer.next_output().unwrap().utterance_final.unwrap().text,
            "クロード"
        );
    }
}
