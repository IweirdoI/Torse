use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use torse_net::*;
use torse_types::*;
fn limits() -> TransportLimits {
    TransportLimits {
        frame: 256,
        queued_frames: 2,
        queued_bytes: 1024,
        poll_bytes: 3,
    }
}
#[test]
fn memory_backpressure_and_control_close_drop_all_unsent_frames() {
    let (mut a, mut b) = MemoryTransport::pair(limits()).unwrap();
    a.send(vec![1; 128]).unwrap();
    a.send(vec![2; 128]).unwrap();
    assert_eq!(a.send(vec![3]).unwrap_err().code, FaultCode::ResourceLimit);
    assert_eq!(b.receive().unwrap().unwrap(), vec![1; 128]);
    a.send(vec![3]).unwrap();
    a.close();
    assert!(b.receive().is_err());
    assert!(b.send(vec![4]).is_err());
}
#[test]
fn tcp_partial_prefixes_payloads_and_writes_reassemble_exactly() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    let mut a = TcpTransport::new(stream, limits()).unwrap();
    let mut b = TcpTransport::new(peer, limits()).unwrap();
    let expected: Vec<_> = (0..200).collect();
    a.send(expected.clone()).unwrap();
    let mut received = None;
    for _ in 0..300 {
        a.poll().unwrap();
        b.poll().unwrap();
        if let Some(frame) = b.receive().unwrap() {
            received = Some(frame);
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(received, Some(expected));
    assert!(b.receive().unwrap().is_none());
}
#[test]
fn tcp_oversized_prefix_is_rejected_before_payload_allocation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut raw = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    let mut transport = TcpTransport::new(peer, limits()).unwrap();
    raw.write_all(&u32::MAX.to_le_bytes()).unwrap();
    let mut rejected = false;
    for _ in 0..50 {
        if transport.poll().is_err() {
            rejected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(rejected);
    assert!(transport.receive().is_err());
}
