use std::io::{Error, ErrorKind::InvalidData};

pub(crate) fn invalid_data<Message: Into<Box<dyn std::error::Error + Send + Sync>>>(
    message: Message
) -> anyhow::Error {
    Error::new(
        InvalidData, 
        message
    ).into()
}