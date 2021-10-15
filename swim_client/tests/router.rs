// Copyright 2015-2021 SWIM.AI inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod tests {
    use swim_async_runtime::time::timeout::timeout;
    use swim_client::configuration::router::{ConnectionPoolParams, RouterParamBuilder};
    use swim_client::connections::factory::tungstenite::TungsteniteWsFactory;
    use swim_client::connections::SwimConnPool;
    use swim_client::router::{Router, RouterEvent, SwimRouter};
    use swim_model::path::AbsolutePath;
    use swim_warp::envelope::Envelope;
    use test_server::build_server;
    use tokio::time::Duration;

    #[tokio::test]
    #[ignore]
    async fn secure() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );

        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "/unit/foo", "info");
        let (sink, mut stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync()
            .node_uri("/unit/foo")
            .lane_uri("info")
            .done();

        sink.send(sync).await.unwrap();

        eprintln!("message = {:#?}", stream.recv().await);

        let _ = router.close().await;
        handle.stop();
    }

    #[tokio::test]
    async fn normal_receive() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );

        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "/unit/foo", "info");
        let (sink, mut stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync()
            .node_uri("/unit/foo")
            .lane_uri("info")
            .done();

        sink.send(sync).await.unwrap();

        let expected = RouterEvent::Message(
            Envelope::linked()
                .node_uri("/unit/foo")
                .lane_uri("info")
                .done()
                .into_response()
                .unwrap(),
        );
        assert_eq!(stream.recv().await.unwrap(), expected);

        let expected = RouterEvent::Message(
            Envelope::event()
                .node_uri("/unit/foo")
                .lane_uri("info")
                .body("")
                .done()
                .into_response()
                .unwrap(),
        );
        assert_eq!(stream.recv().await.unwrap(), expected);

        let expected = RouterEvent::Message(
            Envelope::synced()
                .node_uri("/unit/foo")
                .lane_uri("info")
                .done()
                .into_response()
                .unwrap(),
        );
        assert_eq!(stream.recv().await.unwrap(), expected);

        let _ = router.close().await;
        handle.stop();
    }

    #[tokio::test]
    async fn node_not_found_receive() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(
            url::Url::parse(&host).unwrap(),
            "non_existent",
            "non_existent",
        );
        let (sink, mut stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::link()
            .node_uri("non_existent")
            .lane_uri("non_existent")
            .done();

        sink.send(sync).await.unwrap();

        let expected = RouterEvent::Message(
            Envelope::node_not_found("non_existent", "non_existent")
                .into_response()
                .unwrap(),
        );
        assert_eq!(stream.recv().await.unwrap(), expected);

        let _ = router.close().await;
        handle.stop();
    }

    #[tokio::test]
    async fn lane_not_found_receive() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "/unit/foo", "non_existent");
        let (sink, mut stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::link()
            .node_uri("/unit/foo")
            .lane_uri("non_existent")
            .done();

        sink.send(sync).await.unwrap();

        let expected = RouterEvent::Message(
            Envelope::lane_not_found("/unit/foo", "non_existent")
                .into_response()
                .unwrap(),
        );
        assert_eq!(stream.recv().await.unwrap(), expected);

        let _ = router.close().await;
        handle.stop();
    }

    #[tokio::test]
    async fn not_interested_receive() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "foo", "bar");
        let (sink, mut stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync()
            .node_uri("/unit/foo")
            .lane_uri("info")
            .done();

        sink.send(sync).await.unwrap();

        let result = timeout(Duration::from_secs(5), stream.recv()).await;
        assert!(result.is_err());

        let _ = router.close().await;
        handle.stop();
    }

    #[tokio::test]
    async fn not_found_receive() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(
            url::Url::parse(&host).unwrap(),
            "non_existent",
            "non_existent",
        );
        let (sink, mut stream) = router.connection_for(&path).await.unwrap();

        let command = Envelope::command()
            .node_uri("non_existent")
            .lane_uri("non_existent")
            .done();

        sink.send(command).await.unwrap();

        let result = timeout(Duration::from_secs(5), stream.recv()).await;
        assert!(result.is_err());

        let _ = router.close().await;
        handle.stop();
    }

    #[tokio::test]
    async fn send_commands() {
        let (server, mut handle) = build_server().await;
        tokio::spawn(server.run());
        let port = handle.address().await.unwrap().port();

        let host = format!("ws://127.0.0.1:{}", port);
        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let url = url::Url::parse(&host).unwrap();

        let first_message = Envelope::command()
            .node_uri("/unit/foo")
            .lane_uri("publishInfo")
            .body("Hello, World!")
            .done();

        let second_message = Envelope::command()
            .node_uri("/unit/foo")
            .lane_uri("publishInfo")
            .body("Test message")
            .done();

        let third_message = Envelope::command()
            .node_uri("/unit/foo")
            .lane_uri("publishInfo")
            .body("Bye, World!")
            .done();

        let router_sink = router.general_sink();

        let result = router_sink.send((url.clone(), first_message)).await;
        assert!(result.is_ok());

        let result = router_sink.send((url.clone(), second_message)).await;
        assert!(result.is_ok());

        let result = router_sink.send((url, third_message)).await;
        assert!(result.is_ok());

        let _ = router.close().await;
        handle.stop();
    }
}
