use oh_swarm::types::{Message, MessageKind, TeammateId};
use oh_swarm::Mailbox;
use std::time::SystemTime;

fn make_msg(from: &str, to: &str, body: &str) -> Message {
    Message {
        from: TeammateId::new(from),
        to: TeammateId::new(to),
        kind: MessageKind::UserTurn,
        body: serde_json::json!({ "text": body }),
        sent_at: SystemTime::now(),
    }
}

#[tokio::test]
async fn mailbox_send_recv_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let agent = TeammateId::new("alice");
    let mb = Mailbox::for_agent(dir.path(), &agent);

    let msg = make_msg("bob", "alice", "hello");
    mb.send(&msg).await.unwrap();

    let got = mb
        .recv_one()
        .await
        .unwrap()
        .expect("message should be present");
    assert_eq!(got.body, serde_json::json!({ "text": "hello" }));

    // Inbox should now be empty.
    assert!(mb.recv_one().await.unwrap().is_none());
}

#[tokio::test]
async fn mailbox_fifo_order() {
    let dir = tempfile::tempdir().unwrap();
    let agent = TeammateId::new("fifo_agent");
    let mb = Mailbox::for_agent(dir.path(), &agent);

    for i in 0u32..5 {
        let mut msg = make_msg("sender", "fifo_agent", &i.to_string());
        // Force distinct nanosecond timestamps by nudging SystemTime.
        msg.sent_at =
            SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1000 + u64::from(i) * 100);
        mb.send(&msg).await.unwrap();
    }

    for expected in 0u32..5 {
        let got = mb.recv_one().await.unwrap().expect("expected message");
        let body_text = got.body["text"].as_str().unwrap();
        assert_eq!(
            body_text,
            expected.to_string(),
            "FIFO order violated at index {expected}"
        );
    }
}

#[tokio::test]
async fn mailbox_peek_all_non_destructive() {
    let dir = tempfile::tempdir().unwrap();
    let agent = TeammateId::new("peeker");
    let mb = Mailbox::for_agent(dir.path(), &agent);

    mb.send(&make_msg("a", "peeker", "x")).await.unwrap();
    mb.send(&make_msg("b", "peeker", "y")).await.unwrap();

    // peek_all should return 2 messages without consuming them.
    let peeked = mb.peek_all().await.unwrap();
    assert_eq!(peeked.len(), 2);

    // recv_one should still work.
    let first = mb.recv_one().await.unwrap().unwrap();
    assert_eq!(first.body["text"].as_str().unwrap(), "x");
}
