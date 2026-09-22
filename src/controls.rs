use crate::{metrics::Summary, video};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
#[derive(Default)]
pub struct CallView {
    pub state: Mutex<String>,
    pub summary: Mutex<Option<Summary>>,
    pub video: Arc<video::View>,
    pub ended: AtomicBool,
}
impl CallView {
    pub fn state(&self, text: impl Into<String>) {
        *self.state.lock().unwrap() = text.into();
    }
}
