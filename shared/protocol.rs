use std::io::Cursor;

use anyhow::{Context, Result};
use async_std::prelude::*;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use log::debug;

use crate::message::*;

pub use crate::platform::socket::*;

/// Convenience wrapper around send_bytes.
/// Deserialize a message and feed the bytes into send_bytes.
pub async fn send_message(message: Message, socket: &mut Socket) -> Result<()> {
    debug!("Sending message: {:?}", message);
    // Prepare command for transfer and determine message byte size
    let payload = bincode::serialize(&message).expect("Failed to serialize message.");

    send_bytes(&payload, socket).await
}

/// Send a Vec of bytes. Before the actual bytes are send, the size of the message
/// is transmitted in an header of fixed size (u64).
pub async fn send_bytes(payload: &[u8], socket: &mut Socket) -> Result<()> {
    let message_size = payload.len() as u64;

    let mut header = vec![];
    header.write_u64::<BigEndian>(message_size).unwrap();

    // Send the request size header first.
    // Afterwards send the request.
    socket.write_all(&header).await?;

    // Split the payload into 1.5kbyte chunks (MUT for TCP)
    for chunk in payload.chunks(1500) {
        socket.write_all(chunk).await?;
    }

    Ok(())
}

/// Receive a byte stream depending on a given header.
/// This is the basic protocol beneath all pueue communication.
pub async fn receive_bytes(socket: &mut Socket) -> Result<Vec<u8>> {
    // Receive the header with the overall message size
    let mut header = vec![0; 8];
    socket.read(&mut header).await?;
    let mut header = Cursor::new(header);
    let message_size = header.read_u64::<BigEndian>()? as usize;

    // Buffer for the whole payload
    let mut payload_bytes = Vec::with_capacity(message_size);

    // Receive chunks until we reached the expected message size
    while payload_bytes.len() < message_size {
        // Calculate the amount of bytes left
        // By default try a buffer size of 1024 bytes
        let mut chunk_size = message_size - payload_bytes.len();
        if chunk_size > 1024 {
            chunk_size = 1024;
        }

        // Read data and get the amount of received bytes
        let mut chunk = vec![0; chunk_size];
        let received_bytes = socket.read(&mut chunk).await?;

        // If we received less bytes than the chunk buffer size,
        // split the unneeded bytes, since they are filled with zeros
        if received_bytes < chunk_size {
            let _ = chunk.split_off(received_bytes);
        }

        payload_bytes.append(&mut chunk);
    }

    Ok(payload_bytes)
}

/// Convenience wrapper that receives a message and converts it into a Message.
pub async fn receive_message(socket: &mut Socket) -> Result<Message> {
    let payload_bytes = receive_bytes(socket).await?;
    debug!("Received {} bytes", payload_bytes.len());

    // Deserialize the message.
    let message: Message = bincode::deserialize(&payload_bytes).context(
        "In case you updated Pueue, try restarting the daemon. Otherwise please report this",
    )?;
    debug!("Received message: {:?}", message);

    Ok(message)
}

#[cfg(test)]
mod test {
    use super::*;

    use async_std::net::{TcpListener, TcpStream};
    use async_std::task;

    #[async_std::test]
    async fn test_single_huge_payload() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        // The message that should be sent
        let payload = "a".repeat(100_000);
        let message = create_success_message(payload);
        let original_bytes = bincode::serialize(&message).expect("Failed to serialize message.");

        let listener: Box<dyn GenericListener> = Box::new(listener);

        // Spawn a sub thread that:
        // 1. Accepts a new connection
        // 2. Reads a message
        // 3. Sends the same message back
        task::spawn(async move {
            let mut socket = listener.accept().await.unwrap();
            let message_bytes = receive_bytes(&mut socket).await.unwrap();

            let message: Message = bincode::deserialize(&message_bytes).unwrap();

            send_message(message, &mut socket).await.unwrap();
        });

        let mut client: Socket = Box::new(TcpStream::connect(&addr).await?);

        // Create a client that sends a message and instantly receives it
        send_message(message, &mut client).await?;
        let response_bytes = receive_bytes(&mut client).await?;
        let _message: Message = bincode::deserialize(&response_bytes)?;

        assert_eq!(response_bytes, original_bytes);

        Ok(())
    }
}
