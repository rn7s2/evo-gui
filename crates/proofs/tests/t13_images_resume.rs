//! t13 — a picture in the history: the turn a session was stopped on, read back
//! after `--resume`, with its image still there and still fetchable.
//!
//! ```sh
//! EVO_SWARM_BIN=…/build/evo-swarm EVO_AGENT_BIN=…/build/evo-agent EVO_AGENT_REPO=… \
//!   CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t13_images_resume
//! ```
//!
//! t12 (`t12_attachments.rs`) proves the *sending* half against a plain agent: an
//! image file by path arrives as an item whose `images[]` names `GET /media/<id>/0`,
//! and those bytes are the file's. This is the other half, and the one the reader
//! meets on reopening a session: an image travels by value into the journal
//! (evo-agent's `docs/serve.md` §5, `src/media/media.lisp` — "images travel by
//! value, base64 in the block, and therefore into the journal"), so a server started
//! with `--resume <journal>` must rebuild the item *and* the bytes from the session
//! file alone, with nothing left on disk to read.
//!
//! So the picture's file is deleted before the resume: what `/media` answers can
//! only have come out of the journal. That is the difference between "the transcript
//! can render images" and "the transcript can render images from history".
//!
//! A swarm is what a tab runs, and its coordinator's session is where a person's
//! images go (a lane topic takes no input at all — asserted at the end), so this
//! proof resumes the swarm itself rather than a lone agent.

use std::path::Path;
use std::time::Instant;

use proofs::fixture::{same_path, Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, op, snapshot, try_op, wait_for};
use serde_json::{json, Value};
use session::attachments::Attached;
use store::launch::Program;

/// A 1×1 PNG, red: a real image — evo's own reader sniffs the bytes, so a made-up
/// one would be refused for the wrong reason.
const RED: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A tab's server is a swarm, and one lane is enough to be a swarm.
const PROGRAM: Program = Program::Swarm;
const WORKERS: u16 = 1;

const TURN: &str = "t13 the turn with the picture";

/// The user item of a session snapshot, newest last.
fn last_user(session: &Value) -> Value {
    session["topics"]["session"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .rfind(|item| item["kind"] == json!("user"))
        .cloned()
        .expect("a user item")
}

#[test]
fn t13_images_resume() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t13");
    let server = fixture.spawn(&fixture.spec(PROGRAM, WORKERS));
    let path = server.ready().session.path.clone();
    let client = server.client().clone();

    // --- a turn with a picture, the way the tab sends one -------------------------
    std::fs::write(fixture.folder.join("shot.png"), RED).expect("the picture");
    let picture =
        std::fs::canonicalize(fixture.folder.join("shot.png")).expect("the picture's path");
    let (text, images) = session::attachment_turn(TURN, &[Attached::ImageFile(picture.clone())]);
    let request =
        session::OpRequest::input_send(&text, images, session::Queue::Now, Some("session"));
    let reply = op(&client, &request.op, request.args.clone());
    let id = reply["item_id"].as_str().expect("the item id").to_string();
    println!("{NOTE} sent {id} carrying {picture:?}");

    // The row's descriptor, and the bytes behind it: what the sending half already
    // proves, asserted here too so a failure below is about the resume alone.
    let before = snapshot(&client, &["session"], 50);
    let user = last_user(&before);
    assert_eq!(user["id"], json!(id), "{user}");
    assert_eq!(
        user["images"].as_array().map(Vec::len),
        Some(1),
        "the turn carries its picture: {user}"
    );
    let href = user["images"][0]["href"].clone();
    assert_eq!(href, json!(format!("/media/{id}/0")), "{user}");
    assert_eq!(
        client.media("session", &id, 0).expect("image 0").0,
        RED,
        "the bytes are the file that was attached"
    );

    // --- stopped, and the file taken away -----------------------------------------
    let mut server = server;
    let stopped = server.shutdown().expect("the ladder ran");
    std::fs::remove_file(&picture).expect("the picture leaves the disk");
    println!(
        "{NOTE} stopped {path} ({stopped:?}) and deleted the picture; \
         nothing is left to read it from"
    );

    // --- resumed: the item, and the bytes, come out of the journal ----------------
    let mut resumed = fixture.spawn(&fixture.resume_spec(PROGRAM, Path::new(&path)));
    assert!(
        same_path(Path::new(&resumed.ready().session.path), Path::new(&path)),
        "the resumed server serves the session it was given: {:?}",
        resumed.ready().session
    );
    let client = resumed.client().clone();
    // The read a tab does first: one snapshot, which is what builds the transcript's
    // rows — an item that came back without its images draws no image row at all.
    let after = wait_for(deadline, "the resumed session's own item", || {
        let session = snapshot(&client, &["session"], 50);
        let user = last_user(&session);
        (user["id"] == json!(id)).then_some((session, user))
    });
    let (session, user) = after.clone();
    println!("{NOTE} resumed: {user}");
    assert_eq!(
        user["text"].as_str().unwrap_or_default().contains(TURN),
        true,
        "the turn's words are back: {user}"
    );
    let resumed_images = user["images"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        resumed_images.len(),
        1,
        "the resumed item still carries its picture: {user}"
    );
    assert_eq!(resumed_images[0]["href"], href, "{user}");
    assert_eq!(
        resumed_images[0]["media_type"],
        json!("image/png"),
        "{user}"
    );
    assert_eq!(
        resumed_images[0]["bytes"].as_u64(),
        Some(RED.len() as u64),
        "and the size the descriptor names: {user}"
    );

    // The bytes the resumed server serves are the journaled ones — the file they
    // came from is gone.
    let (bytes, content_type) = client
        .media("session", &id, 0)
        .expect("GET /media after the resume");
    assert_eq!(content_type, "image/png");
    assert_eq!(bytes, RED, "the picture, out of the session file alone");
    assert!(
        client.media("session", &id, 1).is_err(),
        "one image means image 1 is still a 404"
    );
    // And the row's own item, whole (`GET /items/<id>`), agrees: a reader who opens
    // the row gets the same descriptor.
    let item = client.item("session", &id).expect("GET /items/<id>");
    let item = item.get("item").cloned().unwrap_or(item);
    assert_eq!(
        item["images"][0]["href"], href,
        "the whole item names the same media: {item}"
    );
    println!(
        "{NOTE} the session's {} item(s) came back: {session}",
        session["topics"]["session"]["items"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
    );

    // A lane can never be handed one: `input.send` writes to the session only, and
    // it says so rather than dropping the picture somewhere no row will read it.
    let refused = try_op(
        &client,
        "input.send",
        json!({
            "topic": "lane:1",
            "text": "a picture for a lane",
            "images": [{ "path": picture.display().to_string() }],
        }),
    );
    assert_eq!(refused["error"]["code"], json!("invalid_args"), "{refused}");

    let stopped = resumed.shutdown().expect("the ladder ran");
    println!(
        "{NOTE} resumed, fetched and stopped {stopped:?} (t13 in {:?})",
        started.elapsed()
    );
}
