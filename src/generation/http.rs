use super::{GenerationError, GenerationObserver, ProviderReply, ProviderUsage};
use std::{sync::Arc, time::Instant};

#[derive(Default)]
pub(crate) struct ResponseMetadata {
    pub answer_text: Option<Arc<str>>,
    pub finish_reason: Option<Arc<str>>,
    pub usage: Option<ProviderUsage>,
}

pub(crate) async fn send_json(
    request: reqwest::RequestBuilder,
    observer: &mut dyn GenerationObserver,
    metadata: impl Fn(&[u8]) -> ResponseMetadata,
) -> Result<Vec<u8>, GenerationError> {
    let started = Instant::now();
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let error = GenerationError::Transport(error.without_url().to_string());
            observer.failed(&error);
            return Err(error);
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(chunk) => chunk,
            Err(error) => {
                let error = GenerationError::Transport(error.without_url().to_string());
                observer.response_received(ProviderReply {
                    status: Some(status.as_u16()),
                    raw_body: String::from_utf8_lossy(&bytes).into_owned().as_str().into(),
                    answer_text: None,
                    complete: false,
                    finish_reason: None,
                    usage: None,
                    elapsed: started.elapsed(),
                });
                observer.failed(&error);
                return Err(error);
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
            let remaining = (8usize * 1024 * 1024).saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
            observer.response_received(ProviderReply {
                status: Some(status.as_u16()),
                raw_body: String::from_utf8_lossy(&bytes).into_owned().as_str().into(),
                answer_text: None,
                complete: false,
                finish_reason: None,
                usage: None,
                elapsed: started.elapsed(),
            });
            let error = GenerationError::Response("response exceeds 8 MiB".into());
            observer.failed(&error);
            return Err(error);
        }
        bytes.extend_from_slice(&chunk);
    }
    let raw_body = String::from_utf8_lossy(&bytes).into_owned();
    let observation = metadata(&bytes);
    observer.response_received(ProviderReply {
        status: Some(status.as_u16()),
        raw_body: raw_body.as_str().into(),
        answer_text: observation.answer_text,
        complete: true,
        finish_reason: observation.finish_reason,
        usage: observation.usage,
        elapsed: started.elapsed(),
    });
    if !status.is_success() {
        let error = GenerationError::Http(status.as_u16());
        observer.failed(&error);
        return Err(error);
    }
    Ok(bytes)
}
