use super::{SequenceNumber, UdpConnectionVars, UdpPacket};
use log::*;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Debug, PartialEq)]
pub(super) enum SendEvent {
    Send(UdpPacket),
    Resend(UdpPacket),
    AckUpdate,
    WindowUpdate,
    Close,
}

pub(super) struct SendEventReceiver {
    receiver: UnboundedReceiver<SendEvent>,
}

impl SendEventReceiver {
    pub(super) fn new(receiver: UnboundedReceiver<SendEvent>) -> Self {
        Self { receiver }
    }

    pub(super) async fn wait_for_next_packet(
        &mut self,
        con: Arc<Mutex<UdpConnectionVars>>,
    ) -> Option<UdpPacket> {
        let mut event = match self.receiver.recv().await {
            Some(event) => event,
            None => return None,
        };

        // In the case of ack or window update attempt to drain the channel of all subsequent update events
        // to avoid sending unnecessary empty packets if they are not required
        loop {
            match event {
                SendEvent::AckUpdate | SendEvent::WindowUpdate => {}
                SendEvent::Send(packet) | SendEvent::Resend(packet) => return Some(packet),
                SendEvent::Close => {
                    let mut con = con.lock().unwrap();
                    return Some(con.create_close_packet());
                }
            }

            event = match self.receiver.try_recv() {
                Ok(event) => event,
                Err(_) => break,
            };
        }

        // If there are still no pending packet to be sent, we send an empty packet
        let mut con = con.lock().unwrap();

        debug!(
            "sending window/ack update [ack: {}, window: {}]",
            con.ack_number,
            con.calculate_recv_window()
        );
        Some(con.create_data_packet(&[]))
    }
}

impl UdpConnectionVars {
    pub(super) fn create_data_packet(&mut self, payload: &[u8]) -> UdpPacket {
        // Only increase the sequence number if the packet contains data
        // This allows ACK/window updates not to incur recursive ACK's from the peer
        let sequence_number = if payload.len() == 0 {
            self.sequence_number
        } else {
            self.sequence_number + SequenceNumber(1)
        };

        let packet = UdpPacket::data(
            sequence_number,
            self.ack_number,
            self.calculate_recv_window(),
            payload,
        );

        self.update_sequence_number(&packet);

        packet
    }

    pub(super) fn create_open_packet(&mut self) -> UdpPacket {
        let packet = UdpPacket::open(
            self.sequence_number + SequenceNumber(1),
            self.ack_number,
            self.calculate_recv_window(),
        );

        self.update_sequence_number(&packet);

        packet
    }

    pub(super) fn create_close_packet(&mut self) -> UdpPacket {
        let packet = UdpPacket::close(self.sequence_number + SequenceNumber(1), self.ack_number);

        self.update_sequence_number(&packet);

        packet
    }

    fn update_sequence_number(&mut self, packet: &UdpPacket) {
        self.sequence_number = packet.end_sequence_number();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{UdpConnectionConfig, UdpPacketType};
    use super::*;
    use tokio::runtime::Runtime;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn test_create_data_packet() {
        let mut con = UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));

        con.sequence_number = SequenceNumber(10);
        con.ack_number = SequenceNumber(20);

        let packet = con.create_data_packet(&[1]);

        assert_eq!(packet.packet_type, UdpPacketType::Data);
        assert_eq!(packet.sequence_number, SequenceNumber(11));
        assert_eq!(packet.ack_number, SequenceNumber(20));
        assert_eq!(packet.window, 1000);
        assert_eq!(packet.payload, vec![1u8]);

        assert_eq!(con.sequence_number, SequenceNumber(12));
    }

    #[test]
    fn test_create_data_packet_empty() {
        let mut con = UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));

        con.sequence_number = SequenceNumber(10);
        con.ack_number = SequenceNumber(20);

        let packet = con.create_data_packet(&[]);

        assert_eq!(packet.packet_type, UdpPacketType::Data);
        assert_eq!(packet.sequence_number, SequenceNumber(10));
        assert_eq!(packet.ack_number, SequenceNumber(20));
        assert_eq!(packet.window, 1000);
        assert_eq!(packet.payload, Vec::<u8>::new());

        assert_eq!(con.sequence_number, SequenceNumber(10));
    }

    #[test]
    fn test_create_open_packet() {
        let mut con = UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));

        con.sequence_number = SequenceNumber(10);
        con.ack_number = SequenceNumber(20);

        let packet = con.create_open_packet();

        assert_eq!(packet.packet_type, UdpPacketType::Open);
        assert_eq!(packet.sequence_number, SequenceNumber(11));
        assert_eq!(packet.ack_number, SequenceNumber(20));
        assert_eq!(packet.window, 1000);
        assert_eq!(packet.payload, Vec::<u8>::new());

        assert_eq!(con.sequence_number, SequenceNumber(11));
    }

    #[test]
    fn test_create_close_packet() {
        let mut con = UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));

        con.sequence_number = SequenceNumber(10);
        con.ack_number = SequenceNumber(20);

        let packet = con.create_close_packet();

        assert_eq!(packet.packet_type, UdpPacketType::Close);
        assert_eq!(packet.sequence_number, SequenceNumber(11));
        assert_eq!(packet.ack_number, SequenceNumber(20));
        assert_eq!(packet.window, 0);
        assert_eq!(packet.payload, Vec::<u8>::new());

        assert_eq!(con.sequence_number, SequenceNumber(11));
    }

    #[test]
    fn test_wait_for_next_packet_send_event() {
        Runtime::new().unwrap().block_on(async {
            let con = UdpConnectionVars::new(UdpConnectionConfig::default());
            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            let sent_packet = UdpPacket::data(SequenceNumber(1), SequenceNumber(1), 0, &[]);

            tx.send(SendEvent::Send(sent_packet.clone())).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(packet, sent_packet);
        });
    }

    #[test]
    fn test_wait_for_next_packet_resend_event() {
        Runtime::new().unwrap().block_on(async {
            let con = UdpConnectionVars::new(UdpConnectionConfig::default());
            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            let sent_packet = UdpPacket::data(SequenceNumber(1), SequenceNumber(1), 0, &[]);

            tx.send(SendEvent::Resend(sent_packet.clone())).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(packet, sent_packet);
        });
    }

    #[test]
    fn test_wait_for_next_packet_close_event() {
        Runtime::new().unwrap().block_on(async {
            let mut con =
                UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));
            con.sequence_number = SequenceNumber(10);

            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            tx.send(SendEvent::Close).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(
                packet,
                UdpPacket::close(SequenceNumber(11), SequenceNumber(0))
            );
        });
    }

    #[test]
    fn test_wait_for_next_packet_dropped_sender() {
        Runtime::new().unwrap().block_on(async {
            let con = UdpConnectionVars::new(UdpConnectionConfig::default());
            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();
            drop(tx);

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            let option = task.await.unwrap();

            assert_eq!(option, None);
        });
    }

    #[test]
    fn test_wait_for_next_packet_ack_update_event() {
        Runtime::new().unwrap().block_on(async {
            let mut con =
                UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));
            con.sequence_number = SequenceNumber(10);

            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            tx.send(SendEvent::AckUpdate).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(
                packet,
                UdpPacket::data(SequenceNumber(10), SequenceNumber(0), 1000, &[])
            );
        });
    }

    #[test]
    fn test_wait_for_next_packet_window_update_event() {
        Runtime::new().unwrap().block_on(async {
            let mut con =
                UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));
            con.sequence_number = SequenceNumber(10);

            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            tx.send(SendEvent::WindowUpdate).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(
                packet,
                UdpPacket::data(SequenceNumber(10), SequenceNumber(0), 1000, &[])
            );
        });
    }

    #[test]
    fn test_wait_for_next_packet_ack_window_updates_followed_by_send() {
        Runtime::new().unwrap().block_on(async {
            let con = UdpConnectionVars::new(UdpConnectionConfig::default());
            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            let sent_packet = UdpPacket::data(SequenceNumber(1), SequenceNumber(1), 0, &[]);

            tx.send(SendEvent::AckUpdate).unwrap();
            tx.send(SendEvent::WindowUpdate).unwrap();
            tx.send(SendEvent::Send(sent_packet.clone())).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(packet, sent_packet);
        });
    }

    #[test]
    fn test_wait_for_next_packet_ack_window_updates_followed_by_close() {
        Runtime::new().unwrap().block_on(async {
            let mut con =
                UdpConnectionVars::new(UdpConnectionConfig::default().with_recv_window(1000));
            con.sequence_number = SequenceNumber(10);

            let con = Arc::new(Mutex::new(con));

            let (tx, rx) = unbounded_channel();

            let mut receiver = SendEventReceiver::new(rx);

            let task =
                tokio::spawn(async move { receiver.wait_for_next_packet(Arc::clone(&con)).await });

            tx.send(SendEvent::AckUpdate).unwrap();
            tx.send(SendEvent::WindowUpdate).unwrap();
            tx.send(SendEvent::Close).unwrap();

            let packet = task.await.unwrap().unwrap();

            assert_eq!(
                packet,
                UdpPacket::close(SequenceNumber(11), SequenceNumber(0))
            );
        });
    }
}