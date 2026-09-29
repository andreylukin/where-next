//! `ModelStore` against a local HTTP server: a remote model is downloaded, verified against its
//! manifest and becomes Loaded; tampered files, missing files and unsafe manifests never do.
#![cfg(feature = "remote")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use wn_embed::lifecycle::ModelState;
use wn_embed::source::ModelSource;
use wn_embed::store::{ModelStore, StoreError};
use wn_embed::verify::{Manifest, MANIFEST_FILE};

/// Serves `files` (name → body) over HTTP/1.1 on 127.0.0.1; unknown paths get 404. Returns the
/// base URL and a log of requested paths.
fn serve(files: BTreeMap<String, Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/model", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&log);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            // Drain headers.
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok() && header != "\r\n" && !header.is_empty() {
                header.clear();
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            seen.lock().unwrap().push(path.clone());
            let name = path.strip_prefix("/model/").unwrap_or("");
            match files.get(name) {
                Some(body) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(body);
                }
                None => {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });
    (base, log)
}

fn model_files() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    files.insert("wn-model.json".to_string(), br#"{"name":"t"}"#.to_vec());
    files.insert("model.onnx".to_string(), vec![7u8; 100_000]);
    files
}

fn with_manifest(mut files: BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in &files {
        std::fs::write(dir.path().join(name), body).unwrap();
    }
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    let manifest = Manifest::compute(dir.path(), &names).unwrap();
    files.insert(
        MANIFEST_FILE.to_string(),
        serde_json::to_vec(&manifest).unwrap(),
    );
    files
}

fn store(base: &str, cache: &Path) -> ModelStore {
    ModelStore::new(cache.join("m"), ModelSource::parse(base).unwrap())
}

#[test]
fn remote_model_downloads_and_verifies() {
    let (base, log) = serve(with_manifest(model_files()));
    let cache = tempfile::tempdir().unwrap();
    let mut s = store(&base, cache.path());
    assert_eq!(s.ensure().unwrap(), ModelState::Loaded);
    assert!(s.manifest().is_some());
    assert_eq!(
        std::fs::read(cache.path().join("m/model.onnx"))
            .unwrap()
            .len(),
        100_000
    );
    assert!(!cache.path().join("m.download").exists());
    // The manifest is fetched first.
    assert_eq!(log.lock().unwrap()[0], format!("/model/{MANIFEST_FILE}"));
}

#[test]
fn tampered_remote_file_is_corrupt_and_never_loaded() {
    let mut files = with_manifest(model_files());
    files.insert("model.onnx".to_string(), vec![8u8; 100_000]);
    let (base, _) = serve(files);
    let cache = tempfile::tempdir().unwrap();
    let mut s = store(&base, cache.path());
    assert!(matches!(s.ensure(), Err(StoreError::Verify(_))));
    assert_eq!(s.state(), ModelState::Corrupt);
    assert!(s.manifest().is_none());
}

#[test]
fn missing_remote_file_fails_back_to_missing_without_a_partial_dir() {
    let mut files = with_manifest(model_files());
    files.remove("model.onnx");
    let (base, _) = serve(files);
    let cache = tempfile::tempdir().unwrap();
    let mut s = store(&base, cache.path());
    let err = s.ensure().unwrap_err();
    assert!(err.to_string().contains("HTTP 404"), "{err}");
    assert_eq!(s.state(), ModelState::Missing);
    assert!(!cache.path().join("m").exists());
    assert!(!cache.path().join("m.download").exists());
}

#[test]
fn remote_manifest_with_traversal_is_refused() {
    let mut files = BTreeMap::new();
    let mut listed = BTreeMap::new();
    listed.insert("../../escape".to_string(), "00".repeat(32));
    files.insert(
        MANIFEST_FILE.to_string(),
        serde_json::to_vec(&Manifest { files: listed }).unwrap(),
    );
    let (base, log) = serve(files);
    let cache = tempfile::tempdir().unwrap();
    let mut s = store(&base, cache.path());
    let err = s.ensure().unwrap_err();
    assert!(err.to_string().contains("unsafe file name"), "{err}");
    assert_eq!(s.state(), ModelState::Missing);
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "nothing but the manifest was requested"
    );
}
