use crate::router::{Router, SwimRouter};
use common::sink::item::ItemSink;
use common::warp::envelope::Envelope;
use common::warp::path::AbsolutePath;
use std::{thread, time};

#[tokio::test(core_threads = 2)]
async fn foo() {
    let mut router = SwimRouter::new(5).await;

    let path = AbsolutePath::new("ws://224.223.233.1:9001", "foo", "bar");
    let (mut sink, _stream) = router.connection_for(&path).await;

    let sync = Envelope::sync(String::from("node_uri"), String::from("lane_uri"));

    // thread::sleep(time::Duration::from_secs(5));
    sink.send_item(sync).await.unwrap();

    // loop {
    //     println!("{:?}", stream.recv().await.unwrap());
    // }

    thread::sleep(time::Duration::from_secs(5));
}
