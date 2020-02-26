use futures_util::stream::SplitStream;
use futures::StreamExt;
use tokio_tungstenite::{connect_async, WebSocketStream};
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use url;

#[cfg(test)]
mod tests;

struct ConnectionPool {
    connections: HashMap<String, ConnectionHandler>,
}

impl ConnectionPool {
    fn new() -> ConnectionPool {
        return ConnectionPool { connections: HashMap::new() };
    }

    fn get_connection(&mut self, client: &Client, host: &str) -> &ConnectionHandler {
        if !self.connections.contains_key(host) {
            // TODO The connection buffer is hardcoded
            let (connection, connection_handler) = Connection::new(host, 5).unwrap();
            client.schedule_task(connection.open());

            self.connections.insert(host.to_string(), connection_handler);
        }
        return self.connections.get(host).unwrap();
    }
}

struct ConnectionHandler {
    tx: mpsc::Sender<Message>,
}


struct Connection {
    url: url::Url,
    rx: mpsc::Receiver<Message>,
}

impl Connection {
    fn new(host: &str, buffer_size: usize) -> Result<(Connection, ConnectionHandler), ConnectionError> {
        let url = url::Url::parse(&host)?;
        let (tx, rx) = mpsc::channel(buffer_size);

        return Ok((Connection { url, rx }, ConnectionHandler { tx }));
    }

    async fn open(self) -> Result<(), ConnectionError> {
        let (ws_stream, _) = tokio::spawn(connect_async(self.url)).await??;
        let (write_stream, read_stream) = ws_stream.split();

        let receive = Connection::receive_messages(read_stream);
        let send = self.rx.map(Ok).forward(write_stream);

        tokio::spawn(send);
        tokio::spawn(receive);
        return Ok(());
    }

    async fn receive_messages(read_stream: SplitStream<WebSocketStream<TcpStream>>) {
        read_stream.for_each_concurrent(10, |message| async {
            if let Ok(m) = message {
                // TODO Add a real callback
                println!("{}", m);
            }
        }).await;
    }

    async fn send_message(mut tx: mpsc::Sender<Message>, message: String) -> Result<(), ConnectionError> {
        tx.send(Message::text(message)).await?;
        return Ok(());
    }
}

#[derive(Debug, Clone)]
pub enum ConnectionError
{
    ParseError,
    ConnectError,
    SendMessageError,

}

impl Error for ConnectionError {}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ConnectionError::ParseError => write!(f, "Parse error!"),
            ConnectionError::ConnectError => write!(f, "Connect error!"),
            ConnectionError::SendMessageError => write!(f, "Send message error!")
        }
    }
}

impl From<url::ParseError, > for ConnectionError {
    fn from(_: url::ParseError) -> Self {
        ConnectionError::ParseError
    }
}

impl From<tokio::task::JoinError, > for ConnectionError {
    fn from(_: tokio::task::JoinError) -> Self {
        ConnectionError::ConnectError
    }
}

impl From<tungstenite::error::Error, > for ConnectionError {
    fn from(_: tungstenite::error::Error) -> Self {
        ConnectionError::ConnectError
    }
}

impl From<tokio::sync::mpsc::error::SendError<Message>, > for ConnectionError {
    fn from(_: tokio::sync::mpsc::error::SendError<Message>) -> Self {
        ConnectionError::SendMessageError
    }
}


struct Client {
    rt: tokio::runtime::Runtime,
}


impl Client {
    fn new() -> Client {
        let rt = tokio::runtime::Runtime::new().unwrap();
        return Client { rt };
    }

    fn schedule_task<F>(&self, task: impl Future<Output=Result<F, ConnectionError>> + Send + 'static)
        where F: Send + 'static {
        &self.rt.spawn(async move { task.await }.inspect(|response| {
            match response {
                Err(e) => println!("{}", e),
                Ok(_) => ()
            }
        }));
    }
}