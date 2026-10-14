//! Two-client WebSocket integration tests over a real axum server (spec
//! verification matrix, chat-server row: "native integration tests with two
//! in-process ws clients").
//!
//! The server binds an OS-assigned loopback port — nothing is exposed beyond
//! the test process, and no port is fixed, so tests can run in parallel.

use std::time::Duration;

use chat_server::router;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Boot the router on an OS-assigned loopback port and return the port.
async fn spawn_server() -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        axum::serve(listener, router()).await.expect("server runs");
    });
    port
}

async fn connect(port: u16) -> Ws {
    let (ws, _) = connect_async(format!("ws://127.0.0.1:{port}/ws"))
        .await
        .expect("connect /ws");
    ws
}

async fn send_json(ws: &mut Ws, value: &serde_json::Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .expect("send frame");
}

/// Next JSON text frame, or `None` once the server closes the socket.
/// Control frames (pings) are skipped.
async fn recv_json(ws: &mut Ws) -> Option<serde_json::Value> {
    loop {
        let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("server frame within 5s");
        match next {
            Some(Ok(Message::Text(text))) => {
                return Some(serde_json::from_str(&text).expect("server sends valid json"))
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(other)) => panic!("unexpected non-text frame: {other:?}"),
            Some(Err(err)) => panic!("websocket error: {err}"),
        }
    }
}

/// The server has nothing more to send: no frame arrives within 300 ms.
async fn assert_quiet(ws: &mut Ws) {
    let next = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    match next {
        Err(_elapsed) => {} // quiet
        Ok(None) => panic!("server closed the socket unexpectedly"),
        Ok(Some(Ok(frame))) => panic!("expected silence, got {frame:?}"),
        Ok(Some(Err(err))) => panic!("websocket error: {err}"),
    }
}

async fn hello(ws: &mut Ws, name: &str) {
    send_json(ws, &serde_json::json!({ "t": "hello", "name": name })).await;
}

async fn send_msg(ws: &mut Ws, cmid: Uuid, text: &str) {
    send_json(
        ws,
        &serde_json::json!({ "t": "msg", "cmid": cmid.to_string(), "text": text }),
    )
    .await;
}

fn cmid() -> Uuid {
    Uuid::new_v4()
}

/// Two joined clients: Matt (u_1) and Sam (u_2). Leaves c1 having consumed
/// u_2's presence join, and c2 having consumed nothing (no self-echo).
async fn join_two(port: u16) -> (Ws, Ws) {
    let mut c1 = connect(port).await;
    hello(&mut c1, "Matt").await;
    let w1 = recv_json(&mut c1).await.expect("welcome for u_1");
    assert_eq!(w1["t"], "welcome");
    assert_eq!(w1["you"], "u_1");

    let mut c2 = connect(port).await;
    hello(&mut c2, "Sam").await;
    let w2 = recv_json(&mut c2).await.expect("welcome for u_2");
    assert_eq!(w2["you"], "u_2");
    assert_eq!(
        w2["users"],
        serde_json::json!([{ "id": "u_1", "name": "Matt" }])
    );

    // c1 learns about u_2 via presence; c2 gets no self-echo.
    let join = recv_json(&mut c1).await.expect("presence join for u_2");
    assert_eq!(join["t"], "presence");
    assert_eq!(join["join"]["id"], "u_2");
    assert_eq!(join["join"]["name"], "Sam");
    assert!(join["leave"].is_null());

    (c1, c2)
}

#[tokio::test]
async fn hello_gets_welcome_with_history() {
    let port = spawn_server().await;

    let mut c1 = connect(port).await;
    hello(&mut c1, "Matt").await;
    let w1 = recv_json(&mut c1).await.expect("welcome for first client");
    assert_eq!(w1["t"], "welcome");
    assert_eq!(w1["you"], "u_1");
    assert_eq!(w1["users"], serde_json::json!([]));
    assert_eq!(w1["history"], serde_json::json!([]));
    assert_eq!(w1["seq"], 0);

    // A message from u_1 lands in the room...
    let cmid1 = cmid();
    send_msg(&mut c1, cmid1, "hi").await;
    let ack = recv_json(&mut c1).await.expect("ack");
    assert_eq!(ack["t"], "ack");
    assert_eq!(ack["cmid"], serde_json::json!(cmid1.to_string()));
    assert_eq!(ack["id"], "m_1");
    assert_eq!(ack["seq"], 1);

    // ...and a later joiner receives it as history (this is the room's
    // second user, so u_2).
    let mut c3 = connect(port).await;
    hello(&mut c3, "Pat").await;
    let w3 = recv_json(&mut c3).await.expect("welcome with history");
    assert_eq!(w3["t"], "welcome");
    assert_eq!(w3["you"], "u_2");
    assert_eq!(
        w3["users"],
        serde_json::json!([{ "id": "u_1", "name": "Matt" }])
    );
    assert_eq!(
        w3["history"],
        serde_json::json!([{
            "id": "m_1", "from": "u_1", "name": "Matt", "text": "hi",
            "ts": w3["history"][0]["ts"], "seq": 1,
        }])
    );
    assert_eq!(w3["seq"], 1);
}

#[tokio::test]
async fn msg_acks_sender_and_broadcasts_to_peer_in_order() {
    let port = spawn_server().await;
    let (mut c1, mut c2) = join_two(port).await;

    // Three messages, alternating senders: each side's own stream must read
    // back in server-seq order — acks for own sends, broadcasts for the peer's.
    let cmid1 = cmid();
    send_msg(&mut c1, cmid1, "first").await;
    let c1_next = recv_json(&mut c1).await.expect("ack for first");
    assert_eq!(c1_next["t"], "ack");
    assert_eq!(c1_next["cmid"], serde_json::json!(cmid1.to_string()));
    assert_eq!(c1_next["id"], "m_1");
    assert_eq!(c1_next["seq"], 1);
    let c2_next = recv_json(&mut c2).await.expect("broadcast of first");
    assert_eq!(c2_next["t"], "msg");
    assert_eq!(c2_next["id"], "m_1");
    assert_eq!(c2_next["from"], "u_1");
    assert_eq!(c2_next["name"], "Matt");
    assert_eq!(c2_next["text"], "first");
    assert_eq!(c2_next["seq"], 1);

    let cmid2 = cmid();
    send_msg(&mut c2, cmid2, "second").await;
    let c2_next = recv_json(&mut c2).await.expect("ack for second");
    assert_eq!(c2_next["t"], "ack");
    assert_eq!(c2_next["seq"], 2);
    let c1_next = recv_json(&mut c1).await.expect("broadcast of second");
    assert_eq!(c1_next["t"], "msg");
    assert_eq!(c1_next["from"], "u_2");
    assert_eq!(c1_next["seq"], 2);

    let cmid3 = cmid();
    send_msg(&mut c1, cmid3, "third").await;
    let c1_next = recv_json(&mut c1).await.expect("ack for third");
    assert_eq!(c1_next["t"], "ack");
    assert_eq!(c1_next["seq"], 3);
    let c2_next = recv_json(&mut c2).await.expect("broadcast of third");
    assert_eq!(c2_next["t"], "msg");
    assert_eq!(c2_next["seq"], 3);
}

#[tokio::test]
async fn resume_replays_exactly_the_missed_range() {
    let port = spawn_server().await;

    // u_1 sends three messages, then the conversation continues with a fresh
    // connection that resumes from a cursor of its choosing.
    let mut c1 = connect(port).await;
    hello(&mut c1, "Matt").await;
    recv_json(&mut c1).await.expect("welcome for u_1");
    for i in 1..=3 {
        send_msg(&mut c1, cmid(), &format!("m{i}")).await;
        recv_json(&mut c1).await.expect("ack");
    }

    let mut c2 = connect(port).await;
    hello(&mut c2, "Sam").await;
    let welcome = recv_json(&mut c2).await.expect("welcome for u_2");
    assert_eq!(welcome["t"], "welcome");
    assert_eq!(welcome["seq"], 3);
    assert_eq!(
        welcome["history"].as_array().expect("history array").len(),
        3
    );

    send_json(
        &mut c2,
        &serde_json::json!({ "t": "resume", "last_seq": 1 }),
    )
    .await;
    let first = recv_json(&mut c2).await.expect("first replayed message");
    assert_eq!(first["t"], "msg");
    assert_eq!(first["seq"], 2);
    let second = recv_json(&mut c2).await.expect("second replayed message");
    assert_eq!(second["t"], "msg");
    assert_eq!(second["seq"], 3);
    // Exactly the missed range: no duplicates, no gaps.
    assert_quiet(&mut c2).await;

    // A cursor at the room's frontier replays nothing.
    send_json(
        &mut c2,
        &serde_json::json!({ "t": "resume", "last_seq": 3 }),
    )
    .await;
    assert_quiet(&mut c2).await;
}

#[tokio::test]
async fn hello_must_be_the_first_frame() {
    let port = spawn_server().await;

    for first_frame in [
        serde_json::json!({ "t": "typing", "on": true }),
        serde_json::json!({ "t": "msg", "cmid": cmid().to_string(), "text": "hi" }),
        serde_json::json!({ "t": "nonsense" }),
    ] {
        let mut ws = connect(port).await;
        send_json(&mut ws, &first_frame).await;
        let err = recv_json(&mut ws).await.expect("error frame before close");
        assert_eq!(err["t"], "error");
        assert_eq!(err["code"], "no_hello");
        assert!(
            recv_json(&mut ws).await.is_none(),
            "server must close after no_hello"
        );
    }

    // Raw garbage that is not JSON at all gets the same treatment.
    let mut ws = connect(port).await;
    ws.send(Message::Text("not json at all".into()))
        .await
        .expect("send garbage");
    let err = recv_json(&mut ws).await.expect("error frame for garbage");
    assert_eq!(err["t"], "error");
    assert_eq!(err["code"], "no_hello");
    assert!(recv_json(&mut ws).await.is_none());
}

#[tokio::test]
async fn history_cap_holds_at_200_and_evicts_oldest() {
    let port = spawn_server().await;

    let mut c1 = connect(port).await;
    hello(&mut c1, "Matt").await;
    recv_json(&mut c1).await.expect("welcome for u_1");

    for i in 1..=210u64 {
        send_msg(&mut c1, cmid(), &format!("message {i}")).await;
        let ack = recv_json(&mut c1).await.expect("ack");
        assert_eq!(ack["seq"], i, "ack for message {i}");
    }

    // A new joiner sees exactly the 200 newest messages.
    let mut c2 = connect(port).await;
    hello(&mut c2, "Sam").await;
    let welcome = recv_json(&mut c2)
        .await
        .expect("welcome with capped history");
    let history = welcome["history"].as_array().expect("history array");
    assert_eq!(history.len(), 200);
    assert_eq!(history[0]["seq"], 11, "oldest messages evicted");
    assert_eq!(history[0]["text"], "message 11");
    assert_eq!(history[199]["seq"], 210);
    assert_eq!(welcome["seq"], 210);
}

#[tokio::test]
async fn duplicate_cmid_reacks_without_duplicate_broadcast() {
    let port = spawn_server().await;
    let (mut c1, mut c2) = join_two(port).await;

    let cmid1 = cmid();
    send_msg(&mut c1, cmid1, "once").await;
    let ack1 = recv_json(&mut c1).await.expect("ack for first send");
    assert_eq!(ack1["seq"], 1);
    assert_eq!(recv_json(&mut c2).await.expect("broadcast")["seq"], 1);

    // Exact retry (same cmid, as a reconnecting client would send):
    // re-acked against the original id/seq...
    send_msg(&mut c1, cmid1, "once").await;
    let ack2 = recv_json(&mut c1).await.expect("ack for retry");
    assert_eq!(ack2["t"], "ack");
    assert_eq!(ack2["id"], ack1["id"]);
    assert_eq!(ack2["seq"], 1);
    // ...and the peer receives nothing new.
    assert_quiet(&mut c2).await;

    // The retry consumed no seq: the next message gets the next one.
    send_msg(&mut c1, cmid(), "next").await;
    assert_eq!(recv_json(&mut c1).await.expect("ack for next")["seq"], 2);
    assert_eq!(
        recv_json(&mut c2).await.expect("broadcast of next")["seq"],
        2
    );
}

#[tokio::test]
async fn typing_is_relayed_to_peers() {
    let port = spawn_server().await;
    let (mut c1, mut c2) = join_two(port).await;

    send_json(&mut c1, &serde_json::json!({ "t": "typing", "on": true })).await;
    let on = recv_json(&mut c2).await.expect("typing on relay");
    assert_eq!(on["t"], "typing");
    assert_eq!(on["user"], "u_1");
    assert_eq!(on["on"], true);
    // The sender never sees its own typing relay.
    assert_quiet(&mut c1).await;

    send_json(&mut c1, &serde_json::json!({ "t": "typing", "on": false })).await;
    let off = recv_json(&mut c2).await.expect("typing off relay");
    assert_eq!(off["t"], "typing");
    assert_eq!(off["user"], "u_1");
    assert_eq!(off["on"], false);
}

#[tokio::test]
async fn disconnect_broadcasts_presence_leave() {
    let port = spawn_server().await;
    let (mut c1, c2) = join_two(port).await;

    drop(c2);

    let leave = recv_json(&mut c1).await.expect("presence leave");
    assert_eq!(leave["t"], "presence");
    assert!(leave["join"].is_null());
    assert_eq!(leave["leave"], "u_2");
}
