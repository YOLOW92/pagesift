//! Internal early Markdown notification for opt-in streaming responses.
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct MarkdownPreview {
    pub url: String,
    pub markdown: String,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct MarkdownProgress(pub broadcast::Sender<MarkdownPreview>);

impl PartialEq for MarkdownProgress {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
}
