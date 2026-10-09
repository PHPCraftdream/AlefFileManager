// SPDX-License-Identifier: MIT OR Apache-2.0
#[tokio::test]
async fn delivery_targets_live_windows_and_rejects_batches_without_partial_emission() {
    tokio::time::timeout(Duration::from_secs(3), async {
        use alef_core::{
            protocol::call::Limits,
            session::{session::SessionManager, TokenSource},
        };
        let tokens: TokenSource = Arc::new(|| "token".into());
        let manager = SessionManager::new(tokens, Limits::default());
        let main = manager.begin_document(1).await;
        let tool = manager.begin_document(2).await;
        let host = Arc::new(Recorder::default());
        let links = DeepLinks::empty(vec!["alef".into()]);
        links
            .receive(&["alef:startup".into()], host.as_ref())
            .unwrap();
        assert_eq!(host.count(), 0);
        links.intercept(&main, true, host.as_ref());
        links.intercept(&main, true, host.as_ref());
        links.intercept(&tool, true, host.as_ref());
        assert_eq!(host.count(), 1);
        links.receive(&["alef:live".into()], host.as_ref()).unwrap();
        assert_eq!(host.count(), 3);
        let windows: Vec<_> = host.events.lock().unwrap()[1..]
            .iter()
            .map(|event| event.0)
            .collect();
        assert_eq!(windows, [Some(1), Some(2)]);
        assert!(links
            .receive(&["alef:good".into(), "other:bad".into()], host.as_ref())
            .is_err());
        assert_eq!(host.count(), 3);
        links.intercept(&main, false, host.as_ref());
        links.intercept(&tool, false, host.as_ref());
        links
            .receive(&["alef:queued".into()], host.as_ref())
            .unwrap();
        assert_eq!(host.count(), 3);
        links.intercept(&tool, true, host.as_ref());
        assert_eq!(
            host.events.lock().unwrap()[3],
            (
                Some(2),
                "app.open-url".into(),
                json!({"url": "alef:queued"})
            )
        );
        let boundary = format!("alef:{}", "x".repeat(8192 - 5));
        links.receive(&[boundary], host.as_ref()).unwrap();
        assert_eq!(host.count(), 5);
    })
    .await
    .expect("bounded live-window delivery test");
}

#[tokio::test]
async fn hello_urls_are_validated_atomically_and_old_hello_remains_compatible() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let host = Arc::new(Recorder::default());
        let links = Arc::new(DeepLinks::empty(vec!["alef".into()]));
        for urls in [
            vec!["alef:valid", "other:invalid"],
            vec!["alef:bad\n"],
            vec!["alef://[bad"],
            vec!["alef:x"; 33],
        ] {
            let (mut client, server) = tokio::io::duplex(65536);
            let message = json!({"args": ParsedArgs::default(), "cwd": "/work", "urls": urls});
            client
                .write_all(format!("{message}\n").as_bytes())
                .await
                .unwrap();
            assert!(serve(Box::new(server), host.clone(), links.clone())
                .await
                .is_err());
            assert_eq!(host.count(), 0);
        }
        let (mut client, server) = tokio::io::duplex(65536);
        let message = json!({"args": ParsedArgs::default(), "cwd": "/old"});
        client
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        serve(Box::new(server), host.clone(), links).await.unwrap();
        assert_eq!(host.count(), 1);
        assert_eq!(host.events.lock().unwrap()[0].2["cwd"], "/old");
    })
    .await
    .expect("bounded Hello test");
}


#[tokio::test]
async fn hello_byte_limit_requires_a_complete_newline_frame() {
    tokio::time::timeout(Duration::from_secs(2), async {
        assert_eq!(MAX_MESSAGE, 256 * 1024);
        let host = Arc::new(Recorder::default());
        let links = Arc::new(DeepLinks::empty(Vec::new()));
        let mut hello = Hello {
            args: ParsedArgs::default(),
            cwd: String::new(),
            urls: Vec::new(),
        };
        let base = serde_json::to_vec(&hello).unwrap().len();
        hello.cwd = "x".repeat(MAX_MESSAGE as usize - base - 1);
        let mut boundary = serde_json::to_vec(&hello).unwrap();
        boundary.push(b'\n');
        assert_eq!(boundary.len(), MAX_MESSAGE as usize);
        let mut truncated = boundary.clone();
        truncated.pop();
        let mut over_limit = truncated.clone();
        over_limit.push(b' '); // valid JSON exactly at the cap, but no terminating newline
        over_limit.push(b'\n');
        for (message, valid) in [(truncated, false), (over_limit, false), (boundary, true)] {
            let (mut client, server) = tokio::io::duplex(MAX_MESSAGE as usize + 1);
            client.write_all(&message).await.unwrap();
            client.shutdown().await.unwrap();
            let result = serve(Box::new(server), host.clone(), links.clone()).await;
            if valid {
                result.unwrap();
                let mut reply = [0; 3];
                client.read_exact(&mut reply).await.unwrap();
                assert_eq!(&reply, b"ok\n");
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
                assert_eq!(host.count(), 0);
            }
        }
        assert_eq!(host.count(), 1);
        let (_directory, endpoint) = endpoint("oversized"); // no listener: must reject before connect
        hello.cwd.push('x');
        assert_eq!(
            deliver(&endpoint, &hello).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    })
    .await
    .expect("bounded byte-contract regression");
}
