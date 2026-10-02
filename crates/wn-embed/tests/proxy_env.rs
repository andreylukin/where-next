//! Model downloads respect the caller's proxy settings without mutating the test process env.
#![cfg(feature = "remote")]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

const BODY: &[u8] = b"verified by the model store after download";
const CHILD: &str = "WN_PROXY_TEST_SOURCE";
const PROXY_ENV: &[&str] = &[
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
    "HF_TOKEN",
];

#[test]
fn download_in_child() {
    let Ok(source) = std::env::var(CHILD) else {
        return;
    };
    let dest = std::env::var("WN_PROXY_TEST_DEST").unwrap();
    let result = wn_embed::remote::download(
        &wn_embed::source::ModelSource::parse(&source).unwrap(),
        "model.onnx",
        std::path::Path::new(&dest),
    );
    if std::env::var_os("WN_PROXY_TEST_ERROR").is_some() {
        let error = result.expect_err("download should fail").to_string();
        assert!(
            !error.contains("private-proxy-password"),
            "proxy credentials leaked"
        );
    } else {
        result.unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), BODY);
    }
}

fn run(source: &str, vars: &[(&str, &str)], error: bool) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "download_in_child", "--nocapture"])
        .env(CHILD, source)
        .env("WN_PROXY_TEST_DEST", dir.path().join("model.onnx"))
        .env_remove("WN_PROXY_TEST_ERROR");
    for key in PROXY_ENV {
        child.env_remove(key);
    }
    for (key, value) in vars {
        child.env(key, value);
    }
    if error {
        child.env("WN_PROXY_TEST_ERROR", "1");
    }
    child.output().unwrap()
}

fn request(reader: &mut BufReader<TcpStream>) -> String {
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
    first.trim().to_string()
}

fn server(https_proxy: bool) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let start = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > Duration::from_secs(3) {
                        return Vec::new();
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let seen = vec![request(&mut reader)];
        if https_proxy {
            stream
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            return seen;
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            BODY.len()
        )
        .unwrap();
        stream.write_all(BODY).unwrap();
        seen
    });
    (address, worker)
}

fn success(out: Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn http_model_download_uses_the_configured_proxy() {
    for name in ["http_proxy", "all_proxy", "ALL_PROXY"] {
        let (proxy, worker) = server(false);
        let out = run("http://model.invalid/pinned", &[(name, &proxy)], false);
        let seen = worker.join().unwrap();
        success(out);
        assert_eq!(
            seen,
            ["GET http://model.invalid/pinned/model.onnx HTTP/1.1"],
            "{name}"
        );
    }
}

#[test]
fn https_proxy_takes_precedence_over_all_proxy() {
    for name in ["https_proxy", "HTTPS_PROXY"] {
        let (proxy, worker) = server(true);
        let out = run(
            "https://model.invalid/pinned",
            &[(name, &proxy), ("all_proxy", "http://127.0.0.1:1")],
            true,
        );
        let seen = worker.join().unwrap();
        success(out);
        assert_eq!(seen, ["CONNECT model.invalid:443 HTTP/1.1"], "{name}");
    }
}

#[test]
fn lowercase_proxy_takes_precedence_over_uppercase() {
    let (proxy, worker) = server(true);
    let out = run(
        "https://model.invalid/pinned",
        &[
            ("https_proxy", &proxy),
            ("HTTPS_PROXY", "http://127.0.0.1:1"),
        ],
        true,
    );
    let seen = worker.join().unwrap();
    success(out);
    assert_eq!(seen, ["CONNECT model.invalid:443 HTTP/1.1"]);
}

#[test]
fn no_proxy_and_direct_downloads_keep_local_sources_working() {
    for vars in [
        vec![],
        // HTTP_PROXY is unsafe in CGI environments and must not be used.
        vec![("HTTP_PROXY", "http://127.0.0.1:1")],
        vec![
            ("http_proxy", "http://127.0.0.1:1"),
            ("no_proxy", "127.0.0.1"),
        ],
        vec![
            ("http_proxy", "http://127.0.0.1:1"),
            ("NO_PROXY", "127.0.0.1"),
        ],
        vec![("http_proxy", "http://127.0.0.1:1"), ("NO_PROXY", "*")],
        vec![
            ("http_proxy", "http://127.0.0.1:1"),
            ("no_proxy", "127.0.0.1"),
            ("NO_PROXY", "unrelated.invalid"),
        ],
        vec![("http_proxy", ""), ("all_proxy", "http://127.0.0.1:1")],
    ] {
        let (source, worker) = server(false);
        let out = run(&format!("{source}/pinned"), &vars, false);
        let seen = worker.join().unwrap();
        success(out);
        assert_eq!(seen, ["GET /pinned/model.onnx HTTP/1.1"]);
    }
}

#[test]
fn invalid_proxy_errors_do_not_expose_credentials() {
    success(run(
        "http://model.invalid/pinned",
        &[(
            "http_proxy",
            "unsupported://user:private-proxy-password@proxy.invalid",
        )],
        true,
    ));
}
