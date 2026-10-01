//! t12 — what a turn carries besides its words, through the wire evo really answers.
//!
//! ```sh
//! EVO_AGENT_BIN=…/build/evo-agent EVO_AGENT_REPO=… \
//!   CARGO_TARGET_DIR=target/proofs cargo test -p proofs \
//!   --test t12_attachments -- --nocapture
//! ```
//!
//! The unit tests say what the client *builds*; this says what a server built from
//! evo's own main *takes*: an image file by path (evo reads it, sniffs its type and
//! journals it), pasted bytes as base64, and a file that is embedded nowhere and
//! named in the message's text. Then it reads the session back — the row a reader
//! sees is the server's own item — and fetches one image's bytes from `GET /media`,
//! which is what the transcript's image rows are drawn from.
//!
//! The half that is easy to get wrong is the refusal: evo refuses an image it cannot
//! read with `invalid_args`, and that is the message the tab puts above the composer.

use std::path::PathBuf;
use std::time::Instant;

use proofs::fixture::{Fixture, NOTE};
use proofs::watch::{op, snapshot, try_op};
use serde_json::{json, Value};
use session::attachments::Attached;
use store::launch::Program;

/// A 1×1 PNG, red, and the same in blue: real images — evo's own reader sniffs the
/// bytes, so a made-up one would be refused for the wrong reason — and different
/// enough that fetching the wrong one is a failure rather than a coincidence.
const RED: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];
const BLUE: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x60, 0xf8, 0x0f,
    0x00, 0x01, 0x03, 0x01, 0x00, 0x08, 0x89, 0xc2, 0xec, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The user item the server publishes for the turn, newest last.
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
fn t12_attachments() {
    let started = Instant::now();
    let fixture = Fixture::new("t12");
    fixture.enter();
    let server = fixture.spawn(&fixture.spec(Program::Agent, 0));
    let client = server.client().clone();

    // Two files in the folder a swarm would run in: one a picture, one a table.
    // They are named by the paths the client itself would hand over — resolved, so
    // the proof asserts on the same form the op carries (this filesystem has `/var`
    // under `/private`, and only one of the two is what the agent's tools open).
    std::fs::write(fixture.folder.join("shot.png"), RED).expect("the picture");
    std::fs::write(fixture.folder.join("notes.csv"), b"name,total\nada,1\n").expect("the table");
    let shot: PathBuf =
        std::fs::canonicalize(fixture.folder.join("shot.png")).expect("the picture's path");
    let table: PathBuf =
        std::fs::canonicalize(fixture.folder.join("notes.csv")).expect("the table's path");

    // --- one turn: a path image, pasted bytes, and a file --------------------------
    let attached = vec![
        Attached::ImageFile(shot.clone()),
        Attached::ImageBytes {
            name: "pasted.png".to_string(),
            media_type: "image/png".to_string(),
            bytes: BLUE.to_vec(),
        },
        Attached::File(table.clone()),
    ];
    let (text, images) = session::attachment_turn("two pictures and a table", &attached);
    println!("{NOTE} the turn's text: {text:?}");
    println!("{NOTE} the turn's images: {}", json!(images));
    assert_eq!(images.len(), 2, "one entry per image, and only per image");
    assert_eq!(images[0], json!({ "path": shot }));
    assert_eq!(images[1]["name"], json!("pasted.png"));
    assert_eq!(images[1]["media_type"], json!("image/png"));

    // The client's own op, exactly as the tab sends it.
    let request =
        session::OpRequest::input_send(&text, images, session::Queue::Now, Some("session"));
    let reply = op(&client, &request.op, request.args.clone());
    println!("{NOTE} input.send answered {reply}");
    let id = reply["item_id"].as_str().expect("the item id").to_string();
    assert_eq!(
        reply["queued"],
        json!(false),
        "an idle coordinator runs it now"
    );

    // --- the row the reader sees, which is the server's own ------------------------
    let session = snapshot(&client, &["session"], 50);
    let user = last_user(&session);
    println!("{NOTE} the user item: {user}");
    assert_eq!(
        user["id"],
        json!(id),
        "the row keeps the identity the reply named"
    );
    assert_eq!(
        user["text"],
        json!(text),
        "the words travel as they were written, file block and all"
    );
    assert!(
        user["text"].as_str().expect("text").contains(&format!(
            "{}\n- {}",
            session::FILES_HEADING,
            table.display()
        )),
        "the table is named by its absolute path: {user}"
    );

    let item_images = user["images"].as_array().expect("images").clone();
    assert_eq!(item_images.len(), 2, "the item carries both pictures");
    for (n, image) in item_images.iter().enumerate() {
        println!("{NOTE} image {n}: {image}");
        assert_eq!(image["media_type"], json!("image/png"), "{image}");
        assert!(image["bytes"].as_u64().unwrap_or_default() > 0, "{image}");
        assert_eq!(
            image["href"],
            json!(format!("/media/{id}/{n}")),
            "where its bytes are fetched from: {image}"
        );
    }

    // --- and the bytes themselves, which is what an image row draws ----------------
    let (path_image, content_type) = client.media("session", &id, 0).expect("image 0");
    assert_eq!(path_image, RED, "the file on disk, as evo read it");
    assert_eq!(content_type, "image/png");
    let (pasted, content_type) = client.media("session", &id, 1).expect("image 1");
    assert_eq!(
        pasted, BLUE,
        "the bytes that were pasted, as they were sent"
    );
    assert_eq!(content_type, "image/png");
    println!(
        "{NOTE} media 0 and 1 fetched: {} / {} bytes",
        path_image.len(),
        pasted.len()
    );

    // --- an image evo cannot read is refused, and the turn does not run -----------
    let refused = try_op(
        &client,
        "input.send",
        json!({
            "text": "this one is gone",
            "images": [{"path": "/nowhere/at/all/gone.png"}],
            "queue": "now",
        }),
    );
    println!("{NOTE} an unreadable image: {refused}");
    assert_eq!(refused["ok"], json!(false), "{refused}");
    let error = &refused["error"];
    assert_eq!(error["code"], json!("invalid_args"), "{refused}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be read"),
        "the server says what it was, which is what the tab shows above the composer: \
         {refused}"
    );

    // A refused turn leaves nothing behind: no row, and no second user item.
    let after = snapshot(&client, &["session"], 50);
    let users = after["topics"]["session"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == json!("user"))
        .count();
    assert_eq!(users, 1, "the refusal added no row: {after}");

    println!("{NOTE} t12 done in {:?}", started.elapsed());
}
