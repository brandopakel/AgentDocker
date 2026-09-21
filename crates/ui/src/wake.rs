//! Worker notifications are coalesced; idle windows do not poll terminal output.
use std::sync::{Arc, OnceLock};
#[derive(Clone)]
pub struct Wake(Arc<tokio::sync::Notify>);
impl Default for Wake {
    fn default() -> Self {
        static NOTIFY: OnceLock<Arc<tokio::sync::Notify>> = OnceLock::new();
        Self(NOTIFY.get_or_init(Default::default).clone())
    }
}
impl Wake {
    pub fn request_repaint(&self) {
        self.0.notify_one();
    }
    pub async fn notified(&self) {
        self.0.notified().await;
    }
}
