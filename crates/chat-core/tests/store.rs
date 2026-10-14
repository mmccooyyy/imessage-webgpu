//! Native store tests with an injected clock (spec art_d3wTgg6t §5): seq
//! ordering, cmid dedupe, typing expiry, ack delivery, reconnect flow, and
//! scroll stickiness.

use std::time::Duration;

use chat_core::{ConnState, Effect, Event, Store, TYPING_TIMEOUT_MS};
use chat_protocol::{ClientFrame, Msg, Peer, ServerFrame};
use uuid::Uuid;

/// Deterministic cmid — the caller supplies the uuid in production too.
fn cmid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A peer broadcast at the given seq.
fn peer_msg(seq: u64, text: &str) -> ServerFrame {
    ServerFrame::Msg {
        id: format!("m_{seq}"),
        from: "u_2".into(),
        name: "Sam".into(),
        text: text.into(),
        ts: 1_760_000_000 + seq as i64,
        seq,
    }
}

/// A history entry from an arbitrary sender.
fn history_msg(seq: u64, from: &str, name: &str, text: &str) -> Msg {
    Msg {
        id: format!("m_{seq}"),
        from: from.into(),
        name: name.into(),
        text: text.into(),
        ts: 1_760_000_000 + seq as i64,
        seq,
    }
}

/// `welcome` joining us as `u_me` with peer Sam present.
fn welcome(seq: u64, history: Vec<Msg>) -> Event {
    Event::Frame(ServerFrame::Welcome {
        you: "u_me".into(),
        users: vec![Peer {
            id: "u_2".into(),
            name: "Sam".into(),
        }],
        history,
        seq,
    })
}

/// A store that has joined as u_me, cursor at 5, one own history message.
fn joined() -> Store {
    let mut store = Store::new();
    store.apply(Event::SocketOpened, 0);
    store.apply(welcome(5, vec![history_msg(1, "u_me", "Matt", "first")]), 0);
    store
}

#[test]
fn seq_gap_requests_resume_and_never_renders_out_of_order() {
    let mut store = joined();

    // A frame two past the cursor must not render.
    let effects = store.apply(Event::Frame(peer_msg(7, "early")), 10);
    assert!(effects.contains(&Effect::Resume { last_seq: 5 }));
    assert!(!effects.contains(&Effect::Render));
    assert_eq!(store.messages().len(), 1);
    assert_eq!(store.last_seq(), 5);

    // The hole fills first; the skipped frame then lands in order.
    assert!(store
        .apply(Event::Frame(peer_msg(6, "fill")), 11)
        .contains(&Effect::Render));
    assert!(store
        .apply(Event::Frame(peer_msg(7, "early")), 12)
        .contains(&Effect::Render));
    let seqs: Vec<u64> = store.messages().iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![1, 6, 7]);
}

#[test]
fn duplicate_cmid_is_a_no_op() {
    let mut store = joined();
    let id = cmid(1);

    let effects = store.send("one".into(), id, 10);
    assert!(effects.contains(&Effect::SendMsg(ClientFrame::Msg {
        cmid: id,
        text: "one".into(),
    })));
    assert_eq!(store.pending().len(), 1);

    // Same cmid again: no new pending, no effects.
    assert!(store.send("two".into(), id, 11).is_empty());
    assert_eq!(store.pending().len(), 1);
    assert_eq!(store.pending()[0].text, "one");

    // First ack delivers; a replayed ack is a no-op.
    assert!(store.on_ack(id, "m_6".into(), 6).contains(&Effect::Render));
    assert_eq!(store.pending().len(), 0);
    assert_eq!(store.messages().len(), 2);
    assert!(store.on_ack(id, "m_6".into(), 6).is_empty());
    assert_eq!(store.messages().len(), 2);
}

#[test]
fn typing_expires_after_about_three_seconds() {
    let mut store = joined();
    let typing = ServerFrame::Typing {
        user: "u_2".into(),
        on: true,
    };

    store.apply(Event::Frame(typing.clone()), 1_000);
    assert_eq!(store.typing_users(1_000), vec!["u_2".to_string()]);
    assert_eq!(
        store.typing_users(1_000 + TYPING_TIMEOUT_MS - 1),
        vec!["u_2".to_string()]
    );

    // A refresh pushes the deadline out past the original expiry.
    store.apply(Event::Frame(typing.clone()), 2_000);
    assert_eq!(
        store.typing_users(1_000 + TYPING_TIMEOUT_MS),
        vec!["u_2".to_string()]
    );

    // tick at the (refreshed) deadline expires it.
    store.tick(2_000 + TYPING_TIMEOUT_MS);
    assert!(store.typing_users(2_000 + TYPING_TIMEOUT_MS).is_empty());

    // An explicit `on: false` clears immediately.
    store.apply(Event::Frame(typing), 6_000);
    store.apply(
        Event::Frame(ServerFrame::Typing {
            user: "u_2".into(),
            on: false,
        }),
        6_100,
    );
    assert!(store.typing_users(6_200).is_empty());
}

#[test]
fn ack_flips_pending_to_delivered() {
    let mut store = joined();
    let id = cmid(2);
    store.send("hi".into(), id, 10);
    assert_eq!(store.pending().len(), 1);
    assert_eq!(store.last_seq(), 5);

    let effects = store.on_ack(id, "m_6".into(), 6);
    assert!(effects.contains(&Effect::Render));
    assert_eq!(store.pending().len(), 0);
    let delivered = store.messages().last().expect("message delivered");
    assert_eq!(delivered.seq, 6);
    assert_eq!(delivered.text, "hi");
    assert!(delivered.mine);
    assert_eq!(store.last_seq(), 6);
}

#[test]
fn ack_gap_requests_resume_and_replay_dedupes() {
    let mut store = joined();
    let id = cmid(3);
    store.send("hi".into(), id, 10);

    // Our ack skipped two missed peer broadcasts: file the message, request
    // the hole.
    let effects = store.on_ack(id, "m_8".into(), 8);
    assert!(effects.contains(&Effect::Resume { last_seq: 5 }));
    assert_eq!(store.pending().len(), 0);
    assert_eq!(store.messages().last().expect("own msg filed").seq, 8);
    assert_eq!(store.last_seq(), 5);

    // Replay fills the hole; the echo of our own message dedupes silently.
    store.apply(Event::Frame(peer_msg(6, "a")), 11);
    store.apply(Event::Frame(peer_msg(7, "b")), 12);
    assert!(store
        .apply(Event::Frame(peer_msg(8, "echo")), 13)
        .is_empty());
    assert_eq!(store.last_seq(), 8);
    let seqs: Vec<u64> = store.messages().iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![1, 6, 7, 8]);
}

#[test]
fn reconnect_schedules_backoff_requeues_pending_and_resumes() {
    let mut store = joined();

    // Drop of a live session: attempt 0 arms the 500 ms base (jitter 0.5).
    let effects = store.apply(Event::SocketClosed { jitter: 0.5 }, 1_000);
    assert_eq!(
        store.conn(),
        &ConnState::Reconnecting {
            attempt: 0,
            next_retry: 1_500,
        }
    );
    assert!(effects.contains(&Effect::RetryAfter(Duration::from_millis(500))));

    // Sends still queue while reconnecting (same cmid for later retries).
    let id = cmid(4);
    assert!(store
        .send("queued".into(), id, 1_100)
        .contains(&Effect::SendMsg(ClientFrame::Msg {
            cmid: id,
            text: "queued".into(),
        })));

    // tick before the deadline does nothing; at the deadline it fires Connect.
    assert!(store.tick(1_499).is_empty());
    let effects = store.tick(1_500);
    assert!(effects.contains(&Effect::Connect));
    assert_eq!(store.conn(), &ConnState::Connecting);

    // Socket opens and welcome lands: resumed state, replay requested from
    // the old cursor, pending re-sent with the same cmid.
    store.apply(Event::SocketOpened, 1_600);
    let effects = store.apply(
        welcome(9, vec![history_msg(6, "u_2", "Sam", "catching up")]),
        1_700,
    );
    assert_eq!(store.conn(), &ConnState::OfflineResumed);
    assert!(effects.contains(&Effect::Resume { last_seq: 5 }));
    assert!(effects.contains(&Effect::SendMsg(ClientFrame::Msg {
        cmid: id,
        text: "queued".into(),
    })));
}

#[test]
fn first_contact_failure_parks_until_manual_retry() {
    let mut store = Store::new();
    store.apply(Event::SocketOpened, 0);
    let effects = store.apply(Event::SocketClosed { jitter: 0.5 }, 100);
    assert_eq!(store.conn(), &ConnState::ConnectingFailed);
    assert!(effects.contains(&Effect::Render));

    // Input inert while failed.
    assert!(store.send("nope".into(), cmid(5), 200).is_empty());

    // Manual retry joins cleanly (no resume — nothing was ever connected).
    store.apply(Event::SocketOpened, 300);
    assert_eq!(store.conn(), &ConnState::Connecting);
    let effects = store.apply(welcome(0, vec![]), 400);
    assert_eq!(store.conn(), &ConnState::Connected);
    assert!(store.is_live());
    assert!(!effects.contains(&Effect::Resume { last_seq: 0 }));
}

#[test]
fn scroll_stickiness_follows_viewport() {
    let mut store = joined();
    assert!(store.stick_to_bottom());

    // User scrolls up: new messages stop emitting Scroll.
    store.apply(Event::ViewportScrolled { at_bottom: false }, 10);
    assert!(!store.stick_to_bottom());
    let effects = store.apply(Event::Frame(peer_msg(6, "above the fold")), 11);
    assert!(!effects.contains(&Effect::Scroll));

    // Scrolling back to bottom re-arms stickiness.
    store.apply(Event::ViewportScrolled { at_bottom: true }, 12);
    let effects = store.apply(Event::Frame(peer_msg(7, "back down")), 13);
    assert!(effects.contains(&Effect::Scroll));
}
