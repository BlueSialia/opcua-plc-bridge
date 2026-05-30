//! Write request type for the FINS driver.

use core_model::TagValue;
use tokio::sync::oneshot;

/// A queued write request with an optional oneshot reply channel for confirmation.
///
/// On success the driver sends Ok(()); on failure it sends Err(String).
#[derive(Debug)]
pub struct WriteRequest {
    /// Stable core-model tag identifier.
    pub tag_id: String,

    /// Desired value to write.
    pub value: TagValue,

    /// Optional reply channel for driver-level confirmation.
    pub reply: Option<oneshot::Sender<Result<(), String>>>,
}

impl WriteRequest {
    /// Create a new write request without a reply channel.
    pub fn new(tag_id: impl Into<String>, value: TagValue) -> Self {
        Self {
            tag_id: tag_id.into(),
            value,
            reply: None,
        }
    }

    /// Attach a reply channel to the request, returning the modified request.
    pub fn with_reply(mut self, tx: oneshot::Sender<Result<(), String>>) -> Self {
        self.reply = Some(tx);
        self
    }

    /// Consume the request and return its reply channel (if any).
    /// Useful for drivers that want to take ownership of the channel before
    /// performing asynchronous operations.
    pub fn take_reply(&mut self) -> Option<oneshot::Sender<Result<(), String>>> {
        self.reply.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::TagValue as CMTagValue;

    /// #feature DRV-FINS
    #[test]
    fn create_and_attach_reply() {
        let req = WriteRequest::new("PLC::Tag1", CMTagValue::UInt16(42));
        assert_eq!(req.tag_id, "PLC::Tag1");

        let (tx, _rx) = oneshot::channel::<Result<(), String>>();
        let req2 = req.with_reply(tx);
        assert!(req2.reply.is_some());
    }

    /// #feature DRV-FINS
    #[test]
    fn take_reply_consumes_channel() {
        let mut req = WriteRequest::new("t", CMTagValue::Bool(true));
        let (tx, _rx) = oneshot::channel::<Result<(), String>>();
        req = req.with_reply(tx);
        assert!(req.reply.is_some());
        let _ = req.take_reply();
        assert!(req.reply.is_none());
    }
}
