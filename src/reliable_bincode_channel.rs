use std::{marker::PhantomData, u16};

use byteorder::{ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::reliable_channel::{self, ReliableChannel};

#[derive(Debug, Error)]
pub enum Error {
    #[error("reliable channel error: {0}")]
    ReliableChannelError(#[from] reliable_channel::Error),
    #[error("message has exceeded the configured max message length")]
    MessageTooLarge,
    #[error("an error has been encountered that has caused the stream to shutdown")]
    Shutdown,
    #[error("bincode serialization error: {0}")]
    BincodeError(#[from] bincode::Error),
}

/// The maximum supported length for a message sent over a `ReliableBincodeChannel`.
pub const MAX_MESSAGE_LEN: usize = u16::MAX as usize;

/// Wraps a `ReliableChannel` together with an internal buffer to allow easily sending message types
/// serialized with `bincode`.
///
/// Messages are guaranteed to arrive, and are guaranteed to be in order.  Messages have a maximum
/// length, but this maximum size can be much larger than the size of an individual packet (up to
/// MAX_MESSAGE_LEN large).
pub struct ReliableBincodeChannel {
    channel: ReliableChannel,
    bincode_config: bincode::Config,
    max_message_len: u16,
    write_buffer: Vec<u8>,
    write_pos: usize,
    read_buffer: Vec<u8>,
    read_pos: usize,
}

impl ReliableBincodeChannel {
    /// Create a new `ReliableBincodeChannel` with a maximum message size of `max_message_len`.
    pub fn new(channel: ReliableChannel, max_message_len: usize) -> Self {
        assert!(max_message_len <= MAX_MESSAGE_LEN);
        let mut bincode_config = bincode::config();
        bincode_config.limit(max_message_len as u64);
        ReliableBincodeChannel {
            channel,
            bincode_config,
            max_message_len: max_message_len as u16,
            write_buffer: Vec::new(),
            write_pos: 0,
            read_buffer: Vec::new(),
            read_pos: 0,
        }
    }

    /// Write the given message to the reliable channel.
    ///
    /// In order to ensure that messages are sent in a timely manner, `flush` must be called after
    /// calling this method.  Without calling `flush`, any pending writes will not be sent until the
    /// next automatic sender task wakeup.
    pub async fn send<T: Serialize>(&mut self, msg: &T) -> Result<(), Error> {
        self.finish_write().await?;
        self.write_pos = 0;
        self.write_buffer.resize(2, 0);
        match self
            .bincode_config
            .serialize_into(&mut self.write_buffer, msg)
        {
            Ok(()) => {
                let message_len = self.write_buffer.len() as u16 - 2;
                LittleEndian::write_u16(&mut self.write_buffer[0..2], message_len);
                self.finish_write().await?;
                Ok(())
            }
            Err(err) => {
                self.write_buffer.clear();
                self.write_pos = 0;
                Err(err.into())
            }
        }
    }

    /// Ensure that any previously sent messages are sent as soon as possible.
    pub async fn flush(&mut self) -> Result<(), Error> {
        self.finish_write().await?;
        Ok(self.channel.flush().await?)
    }

    /// Read the next available incoming message.
    pub async fn recv<'a, T: Deserialize<'a>>(&'a mut self) -> Result<T, Error> {
        if self.read_pos < 2 {
            self.read_buffer.resize(2, 0);
        }
        self.finish_read().await?;

        let message_len = LittleEndian::read_u16(&self.read_buffer[0..2]);
        if message_len > self.max_message_len {
            return Err(Error::MessageTooLarge);
        }
        self.read_buffer.resize(message_len as usize + 2, 0);
        self.finish_read().await?;

        self.read_pos = 0;
        Ok(self.bincode_config.deserialize(&self.read_buffer[2..])?)
    }

    async fn finish_write(&mut self) -> Result<(), Error> {
        while self.write_pos < self.write_buffer.len() {
            let len = self
                .channel
                .write(&self.write_buffer[self.write_pos..])
                .await?;
            self.write_pos += len;
        }
        Ok(())
    }

    async fn finish_read(&mut self) -> Result<(), Error> {
        while self.read_pos < self.read_buffer.len() {
            let len = self
                .channel
                .read(&mut self.read_buffer[self.read_pos..])
                .await?;
            self.read_pos += len;
        }
        Ok(())
    }
}

/// Wrapper over an `ReliableBincodeChannel` that only allows a single message type.
pub struct ReliableTypedChannel<T> {
    channel: ReliableBincodeChannel,
    _phantom: PhantomData<T>,
}

impl<T> ReliableTypedChannel<T> {
    pub fn new(channel: ReliableBincodeChannel) -> Self {
        ReliableTypedChannel {
            channel,
            _phantom: PhantomData,
        }
    }

    pub async fn flush(&mut self) -> Result<(), Error> {
        self.channel.flush().await
    }
}

impl<T: Serialize> ReliableTypedChannel<T> {
    pub async fn send(&mut self, msg: &T) -> Result<(), Error> {
        self.channel.send(msg).await
    }
}

impl<'a, T: Deserialize<'a>> ReliableTypedChannel<T> {
    pub async fn recv(&'a mut self) -> Result<T, Error> {
        self.channel.recv().await
    }
}
