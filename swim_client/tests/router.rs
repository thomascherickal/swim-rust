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

#[cfg(feature = "test_server")]
mod tests {
    use swim_client::configuration::router::{ConnectionPoolParams, RouterParamBuilder};
    use swim_client::connections::factory::tungstenite::TungsteniteWsFactory;
    use swim_client::connections::SwimConnPool;
    use swim_client::router::{Router, SwimRouter};
    use swim_common::model::Value;
    use swim_common::warp::envelope::Envelope;
    use swim_common::warp::path::AbsolutePath;
    use test_server::clients::Cli;
    use test_server::Docker;
    use test_server::SwimTestServer;
    use tokio::time::Duration;
    use utilities::trace;

    fn init_trace() {
        trace::init_trace(vec!["client::router=trace"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn secure() {
        init_trace();

        let docker = Cli::default();
        let container = docker.run(SwimTestServer);
        let port = container.get_host_port(9001).unwrap();
        let host = format!("wss://127.0.0.1:{}", port);

        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );

        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "/unit/foo", "info");
        let (sink, _stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync(String::from("/unit/foo"), String::from("info"));

        sink.send(sync).await.unwrap();

        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = router.close().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_receive() {
        init_trace();

        let docker = Cli::default();
        let container = docker.run(SwimTestServer);
        let port = container.get_host_port(9001).unwrap();
        let host = format!("ws://127.0.0.1:{}", port);

        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );

        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "/unit/foo", "info");
        let (sink, _stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync(String::from("/unit/foo"), String::from("info"));

        sink.send(sync).await.unwrap();

        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = router.close().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn not_interested_receive() {
        init_trace();

        let docker = Cli::default();
        let container = docker.run(SwimTestServer);
        let port = container.get_host_port(9001).unwrap();
        let host = format!("ws://127.0.0.1:{}", port);

        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "foo", "bar");
        let (sink, _stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync(String::from("/unit/foo"), String::from("info"));

        sink.send(sync).await.unwrap();

        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = router.close().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn not_found_receive() {
        init_trace();

        let docker = Cli::default();
        let container = docker.run(SwimTestServer);
        let port = container.get_host_port(9001).unwrap();
        let host = format!("ws://127.0.0.1:{}", port);

        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "foo", "bar");
        let (sink, _stream) = router.connection_for(&path).await.unwrap();

        let sync = Envelope::sync(String::from("non_existent"), String::from("non_existent"));

        sink.send(sync).await.unwrap();

        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = router.close().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_commands() {
        init_trace();

        let docker = Cli::default();
        let container = docker.run(SwimTestServer);
        let port = container.get_host_port(9001).unwrap();
        let host = format!("ws://127.0.0.1:{}", port);

        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let url = url::Url::parse(&host).unwrap();

        let first_message = Envelope::make_command(
            String::from("/unit/foo"),
            String::from("publishInfo"),
            Some(Value::text("Hello, World!")),
        );

        let second_message = Envelope::make_command(
            String::from("/unit/foo"),
            String::from("publishInfo"),
            Some(Value::text("Test message")),
        );

        let third_message = Envelope::make_command(
            String::from("/unit/foo"),
            String::from("publishInfo"),
            Some(Value::text("Bye, World!")),
        );

        let router_sink = router.general_sink();

        router_sink
            .send((url.clone(), first_message))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        router_sink
            .send((url.clone(), second_message))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        router_sink.send((url, third_message)).await.unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = router.close().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    pub async fn server_stops_between_requests() {
        init_trace();

        let docker = Cli::default();
        let container = docker.run(SwimTestServer);
        let port = container.get_host_port(9001).unwrap();
        let host = format!("ws://127.0.0.1:{}", port);

        let config = RouterParamBuilder::default().build();
        let pool = SwimConnPool::new(
            ConnectionPoolParams::default(),
            TungsteniteWsFactory::new(5).await,
        );
        let mut router = SwimRouter::new(config, pool);

        let path = AbsolutePath::new(url::Url::parse(&host).unwrap(), "/unit/foo", "info");
        let (sink, _stream) = router.connection_for(&path).await.unwrap();
        let sync = Envelope::sync(String::from("/unit/foo"), String::from("info"));

        println!("Sending item");
        sink.send(sync).await.unwrap();
        println!("Sent item");

        docker.stop(container.id());

        let sync = Envelope::sync(String::from("/unit/foo"), String::from("info"));

        println!("Sending second item");
        let _ = sink.send(sync).await;
        println!("Sent second item");
        tokio::time::sleep(Duration::from_secs(10)).await;
        let _ = router.close().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
