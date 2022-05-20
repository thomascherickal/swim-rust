// Copyright 2015-2021 Swim Inc.
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

use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::agent::task::links::TriggerUnlink;
use crate::agent::task::write_fut::SpecialAction;
use crate::compat::{Operation, RawRequestMessageDecoder, RequestMessage};
use crate::error::InvalidKey;
use crate::routing::RoutingAddr;

use self::links::Links;
use self::prune::PruneRemotes;
use self::remotes::{LaneRegistry, RemoteSender, RemoteTracker, UplinkResponse};
use self::write_fut::{WriteResult, WriteTask};

use super::{
    AgentAttachmentRequest, AgentRuntimeConfig, AgentRuntimeRequest, DisconnectionReason, Io,
};
use bytes::{Bytes, BytesMut};
use futures::ready;
use futures::stream::FuturesUnordered;
use futures::{
    future::{join, select as fselect, Either},
    stream::{select as sselect, SelectAll},
    SinkExt, Stream, StreamExt,
};
use pin_utils::pin_mut;
use swim_api::protocol::agent::{LaneResponseKind, MapLaneResponse, ValueLaneResponse};
use swim_api::{
    agent::UplinkKind,
    error::AgentRuntimeError,
    protocol::{
        agent::{
            LaneRequest, LaneRequestEncoder, MapLaneResponseDecoder, ValueLaneResponseDecoder,
        },
        map::{extract_header, MapMessage, MapMessageEncoder, RawMapOperationEncoder},
        WithLengthBytesCodec,
    },
};
use swim_model::path::RelativePath;
use swim_model::Text;
use swim_recon::parser::MessageExtractError;
use swim_utilities::future::{immediate_or_join, StopAfterError, SwimStreamExt};
use swim_utilities::io::byte_channel::{byte_channel, ByteReader, ByteWriter};
use swim_utilities::trigger::{self, promise};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Instant, Sleep};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{Encoder, FramedRead, FramedWrite};
use uuid::Uuid;

use tracing::{debug, error, info, info_span, trace, warn};
use tracing_futures::Instrument;

mod init;
mod links;
mod prune;
mod remotes;
mod timeout_coord;
mod write_fut;

pub use init::{AgentInitTask, NoLanes};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct LaneEndpoint<T> {
    name: Text,
    kind: UplinkKind,
    io: T,
}

impl<T> LaneEndpoint<T> {
    fn new(name: Text, kind: UplinkKind, io: T) -> Self {
        LaneEndpoint { name, kind, io }
    }
}

impl LaneEndpoint<Io> {
    fn split(self) -> (LaneEndpoint<ByteWriter>, LaneEndpoint<ByteReader>) {
        let LaneEndpoint {
            name,
            kind,
            io: (tx, rx),
        } = self;

        let read = LaneEndpoint::new(name.clone(), kind, rx);

        let write = LaneEndpoint::new(name, kind, tx);

        (write, read)
    }
}

impl LaneEndpoint<ByteReader> {
    fn into_lane_stream(self, registry: &mut LaneRegistry) -> LaneStream {
        let LaneEndpoint {
            name,
            kind,
            io: reader,
        } = self;
        let id = registry.add_endpoint(name);
        match kind {
            UplinkKind::Value => {
                let receiver = LaneReceiver::value(id, reader);
                Either::Left(receiver).stop_after_error()
            }
            UplinkKind::Map => {
                let receiver = LaneReceiver::map(id, reader);
                Either::Right(receiver).stop_after_error()
            }
        }
    }
}

#[derive(Debug)]
pub struct InitialEndpoints {
    rx: mpsc::Receiver<AgentRuntimeRequest>,
    endpoints: Vec<LaneEndpoint<Io>>,
}

impl InitialEndpoints {
    fn new(rx: mpsc::Receiver<AgentRuntimeRequest>, endpoints: Vec<LaneEndpoint<Io>>) -> Self {
        InitialEndpoints { rx, endpoints }
    }

    pub fn make_runtime_task(
        self,
        identity: RoutingAddr,
        node_uri: Text,
        attachment_rx: mpsc::Receiver<AgentAttachmentRequest>,
        config: AgentRuntimeConfig,
        stopping: trigger::Receiver,
    ) -> AgentRuntimeTask {
        AgentRuntimeTask::new(identity, node_uri, self, attachment_rx, stopping, config)
    }
}

#[derive(Debug)]
pub struct AgentRuntimeTask {
    identity: RoutingAddr,
    node_uri: Text,
    init: InitialEndpoints,
    attachment_rx: mpsc::Receiver<AgentAttachmentRequest>,
    stopping: trigger::Receiver,
    config: AgentRuntimeConfig,
}

type ValueLaneEncoder = LaneRequestEncoder<WithLengthBytesCodec>;
type MapLaneEncoder = LaneRequestEncoder<MapMessageEncoder<RawMapOperationEncoder>>;

/// Sender to communicate with a lane.
#[derive(Debug)]
enum LaneSender {
    Value {
        sender: FramedWrite<ByteWriter, ValueLaneEncoder>,
    },
    Map {
        sender: FramedWrite<ByteWriter, MapLaneEncoder>,
    },
}

#[derive(Debug, Clone)]
enum RwCoorindationMessage {
    UnknownLane {
        origin: Uuid,
        path: RelativePath,
    },
    BadEnvelope {
        origin: Uuid,
        lane: Text,
        error: MessageExtractError,
    },
    Link {
        origin: Uuid,
        lane: Text,
    },
    Unlink {
        origin: Uuid,
        lane: Text,
    },
}

#[derive(Debug, Error)]
enum LaneSendError {
    #[error("Sending lane message failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Interpreting lane message failed: {0}")]
    Extraction(#[from] MessageExtractError),
}

impl LaneSender {
    fn new(tx: ByteWriter, kind: UplinkKind) -> Self {
        match kind {
            UplinkKind::Value => LaneSender::Value {
                sender: FramedWrite::new(tx, LaneRequestEncoder::value()),
            },
            UplinkKind::Map => LaneSender::Map {
                sender: FramedWrite::new(tx, LaneRequestEncoder::map()),
            },
        }
    }

    async fn start_sync(&mut self, id: Uuid) -> Result<(), std::io::Error> {
        match self {
            LaneSender::Value { sender } => {
                let req: LaneRequest<Bytes> = LaneRequest::Sync(id);
                sender.send(req).await
            }
            LaneSender::Map { sender } => {
                let req: LaneRequest<MapMessage<Bytes, Bytes>> = LaneRequest::Sync(id);
                sender.send(req).await
            }
        }
    }

    async fn feed_frame(&mut self, data: Bytes) -> Result<(), LaneSendError> {
        match self {
            LaneSender::Value { sender } => {
                sender.feed(LaneRequest::Command(data)).await?;
            }
            LaneSender::Map { sender } => {
                let message = extract_header(&data)?;
                sender.send(LaneRequest::Command(message)).await?;
            }
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), std::io::Error> {
        match self {
            LaneSender::Value { sender } => flush_sender_val(sender).await,
            LaneSender::Map { sender } => flush_sender_map(sender).await,
        }
    }
}

async fn flush_sender_val<T>(sender: &mut FramedWrite<ByteWriter, T>) -> Result<(), T::Error>
where
    T: Encoder<LaneRequest<Bytes>>,
{
    sender.flush().await
}

async fn flush_sender_map<T>(sender: &mut FramedWrite<ByteWriter, T>) -> Result<(), T::Error>
where
    T: Encoder<LaneRequest<MapMessage<Bytes, Bytes>>>,
{
    sender.flush().await
}

#[derive(Debug)]
struct LaneReceiver<D> {
    lane_id: u64,
    receiver: FramedRead<ByteReader, D>,
}

type ValueLaneReceiver = LaneReceiver<ValueLaneResponseDecoder>;
type MapLaneReceiver = LaneReceiver<MapLaneResponseDecoder>;

#[derive(Debug)]
struct RawLaneResponse {
    target: Option<Uuid>,
    response: UplinkResponse,
}

impl RawLaneResponse {
    pub fn targetted(id: Uuid, response: UplinkResponse) -> Self {
        RawLaneResponse {
            target: Some(id),
            response,
        }
    }

    pub fn broadcast(response: UplinkResponse) -> Self {
        RawLaneResponse {
            target: None,
            response,
        }
    }
}

impl From<ValueLaneResponse<Bytes>> for RawLaneResponse {
    fn from(resp: ValueLaneResponse<Bytes>) -> Self {
        let ValueLaneResponse { kind, value } = resp;
        match kind {
            LaneResponseKind::StandardEvent => {
                RawLaneResponse::broadcast(UplinkResponse::Value(value))
            }
            LaneResponseKind::SyncEvent(id) => {
                RawLaneResponse::targetted(id, UplinkResponse::SyncedValue(value))
            }
        }
    }
}

impl From<MapLaneResponse<Bytes, Bytes>> for RawLaneResponse {
    fn from(resp: MapLaneResponse<Bytes, Bytes>) -> Self {
        match resp {
            MapLaneResponse::Event { kind, operation } => match kind {
                LaneResponseKind::StandardEvent => {
                    RawLaneResponse::broadcast(UplinkResponse::Map(operation))
                }
                LaneResponseKind::SyncEvent(id) => {
                    RawLaneResponse::targetted(id, UplinkResponse::Map(operation))
                }
            },
            MapLaneResponse::SyncComplete(id) => {
                RawLaneResponse::targetted(id, UplinkResponse::SyncedMap)
            }
        }
    }
}

impl LaneReceiver<ValueLaneResponseDecoder> {
    fn value(lane_id: u64, reader: ByteReader) -> Self {
        LaneReceiver {
            lane_id,
            receiver: FramedRead::new(reader, Default::default()),
        }
    }
}

impl LaneReceiver<MapLaneResponseDecoder> {
    fn map(lane_id: u64, reader: ByteReader) -> Self {
        LaneReceiver {
            lane_id,
            receiver: FramedRead::new(reader, Default::default()),
        }
    }
}

#[derive(Debug)]
struct Failed(u64);

impl Stream for ValueLaneReceiver {
    type Item = Result<(u64, RawLaneResponse), Failed>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let maybe_result = ready!(this.receiver.poll_next_unpin(cx));
        let id = this.lane_id;
        Poll::Ready(
            maybe_result.map(|result| result.map(|r| (id, r.into())).map_err(|_| Failed(id))),
        )
    }
}

impl Stream for MapLaneReceiver {
    type Item = Result<(u64, RawLaneResponse), Failed>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let maybe_result = ready!(this.receiver.poll_next_unpin(cx));
        let id = this.lane_id;
        Poll::Ready(
            maybe_result.map(|result| result.map(|r| (id, r.into())).map_err(|_| Failed(id))),
        )
    }
}

impl AgentRuntimeTask {
    fn new(
        identity: RoutingAddr,
        node_uri: Text,
        init: InitialEndpoints,
        attachment_rx: mpsc::Receiver<AgentAttachmentRequest>,
        stopping: trigger::Receiver,
        config: AgentRuntimeConfig,
    ) -> Self {
        AgentRuntimeTask {
            identity,
            node_uri,
            init,
            attachment_rx,
            stopping,
            config,
        }
    }

    pub async fn run(self) {
        let AgentRuntimeTask {
            identity,
            node_uri,
            init: InitialEndpoints { rx, endpoints },
            attachment_rx,
            stopping,
            config,
        } = self;

        let (write_endpoints, read_endpoints): (Vec<_>, Vec<_>) =
            endpoints.into_iter().map(LaneEndpoint::split).unzip();

        let (read_tx, read_rx) = mpsc::channel(config.attachment_queue_size.get());
        let (write_tx, write_rx) = mpsc::channel(config.attachment_queue_size.get());
        let (read_vote, write_vote, vote_waiter) = timeout_coord::timeout_coordinator();

        let (kill_switch_tx, kill_switch_rx) = trigger::trigger();

        let combined_stop = fselect(fselect(stopping.clone(), kill_switch_rx), vote_waiter);
        let att = attachment_task(rx, attachment_rx, read_tx, write_tx.clone(), combined_stop)
            .instrument(info_span!("Agent Runtime Attachment Task", %identity, %node_uri));
        let read = read_task(
            config,
            write_endpoints,
            read_rx,
            write_tx,
            read_vote,
            stopping.clone(),
        )
        .instrument(info_span!("Agent Runtime Read Task", %identity, %node_uri));
        let write = write_task(
            WriteTaskConfiguration::new(identity, node_uri.clone(), config),
            read_endpoints,
            write_rx,
            write_vote,
            stopping,
        )
        .instrument(info_span!("Agent Runtime Write Task", %identity, %node_uri));

        let io = await_io_tasks(read, write, kill_switch_tx);
        join(att, io).await;
    }
}

enum ReadTaskRegistration {
    Lane {
        name: Text,
        sender: LaneSender,
    },
    Remote {
        reader: ByteReader,
        on_attached: Option<trigger::Sender>,
    },
}

#[derive(Debug)]
enum WriteTaskMessage {
    Lane(LaneEndpoint<ByteReader>),
    Remote {
        id: Uuid,
        writer: ByteWriter,
        completion: promise::Sender<DisconnectionReason>,
        on_attached: Option<trigger::Sender>,
    },
    Coord(RwCoorindationMessage),
}

impl WriteTaskMessage {
    fn generates_activity(&self) -> bool {
        matches!(self, WriteTaskMessage::Coord(_))
    }
}

const BAD_LANE_REG: &str = "Agent failed to receive lane registration result.";

async fn attachment_task<F>(
    runtime: mpsc::Receiver<AgentRuntimeRequest>,
    attachment: mpsc::Receiver<AgentAttachmentRequest>,
    read_tx: mpsc::Sender<ReadTaskRegistration>,
    write_tx: mpsc::Sender<WriteTaskMessage>,
    combined_stop: F,
) where
    F: Future + Unpin,
{
    let mut stream = sselect(
        ReceiverStream::new(runtime).map(Either::Left),
        ReceiverStream::new(attachment).map(Either::Right),
    )
    .take_until(combined_stop);

    let mut attachments = FuturesUnordered::new();

    loop {
        tokio::select! {
            biased;
            _ = attachments.next(), if !attachments.is_empty() => {},
            maybe_event = stream.next() => {
                if let Some(event) = maybe_event {
                    match event {
                        Either::Left(AgentRuntimeRequest::AddLane {
                            name,
                            kind,
                            config,
                            promise,
                        }) => {
                            info!("Registering a new {} lane with name {}.", kind, name);
                            let lane_config = config.unwrap_or_default();
                            let (in_tx, in_rx) = byte_channel(lane_config.input_buffer_size);
                            let (out_tx, out_rx) = byte_channel(lane_config.output_buffer_size);
                            let sender = LaneSender::new(in_tx, kind);
                            let read_permit = match read_tx.reserve().await {
                                Err(_) => {
                                    warn!("Read task stopped while attempting to register a new {} lane named '{}'.", kind, name);
                                    if promise.send(Err(AgentRuntimeError::Terminated)).is_err() {
                                        error!(BAD_LANE_REG);
                                    }
                                    break;
                                }
                                Ok(permit) => permit,
                            };
                            let write_permit = match write_tx.reserve().await {
                                Err(_) => {
                                    warn!("Write task stopped while attempting to register a new {} lane named '{}'.", kind, name);
                                    if promise.send(Err(AgentRuntimeError::Terminated)).is_err() {
                                        error!(BAD_LANE_REG);
                                    }
                                    break;
                                }
                                Ok(permit) => permit,
                            };
                            read_permit.send(ReadTaskRegistration::Lane {
                                name: name.clone(),
                                sender,
                            });
                            write_permit.send(WriteTaskMessage::Lane(LaneEndpoint::new(
                                name, kind, out_rx,
                            )));
                            if promise.send(Ok((out_tx, in_rx))).is_err() {
                                error!(BAD_LANE_REG);
                            }
                        }
                        Either::Left(_) => todo!("Opening downlinks form agents not implemented."),
                        Either::Right(AgentAttachmentRequest { id, io: (tx, rx), completion, on_attached }) => {
                            info!(
                                "Attaching a new remote endpoint with ID {id} to the agent.",
                                id = id
                            );
                            let read_permit = match read_tx.reserve().await {
                                Err(_) => {
                                    warn!("Read task stopped while attempting to attach a remote endpoint.");
                                    break;
                                }
                                Ok(permit) => permit,
                            };
                            let write_permit = match write_tx.reserve().await {
                                Err(_) => {
                                    warn!("Write task stopped while attempting to attach a remote endpoint.");
                                    break;
                                }
                                Ok(permit) => permit,
                            };
                            let (read_on_attached, write_on_attached) = if let Some(on_attached) = on_attached {
                                let (read_tx, read_rx) = trigger::trigger();
                                let (write_tx, write_rx) = trigger::trigger();
                                attachments.push(async move {
                                    if matches!(join(read_rx, write_rx).await, (Ok(_), Ok(_))) {
                                        on_attached.trigger();
                                    }
                                });
                                (Some(read_tx), Some(write_tx))
                            } else {
                                (None, None)
                            };
                            read_permit.send(ReadTaskRegistration::Remote { reader: rx, on_attached: read_on_attached });
                            write_permit.send(WriteTaskMessage::Remote { id, writer: tx, completion, on_attached: write_on_attached });
                        }
                    }
                } else {
                    break;
                }
            }
        }
    }
}

type RemoteReceiver = FramedRead<ByteReader, RawRequestMessageDecoder>;

fn remote_receiver(reader: ByteReader) -> RemoteReceiver {
    RemoteReceiver::new(reader, Default::default())
}

const TASK_COORD_ERR: &str = "Stopping after communcating with the write task failed.";
const STOP_VOTED: &str = "Stopping as read and write tasks have both voted to do so.";

enum ReadTaskEvent {
    Registration(ReadTaskRegistration),
    Envelope(RequestMessage<Bytes>),
    Timeout,
}

async fn read_task(
    config: AgentRuntimeConfig,
    initial_endpoints: Vec<LaneEndpoint<ByteWriter>>,
    reg_rx: mpsc::Receiver<ReadTaskRegistration>,
    write_tx: mpsc::Sender<WriteTaskMessage>,
    stop_vote: timeout_coord::Voter,
    stopping: trigger::Receiver,
) {
    let mut remotes = SelectAll::<StopAfterError<RemoteReceiver>>::new();

    let mut reg_stream = ReceiverStream::new(reg_rx).take_until(stopping);

    let mut counter: u64 = 0;

    let mut next_id = move || {
        let id = counter;
        counter += 1;
        id
    };

    let mut name_mapping = HashMap::new();
    let mut lanes = HashMap::new();
    let mut needs_flush = None;
    let mut voted = false;

    for LaneEndpoint { name, kind, io } in initial_endpoints.into_iter() {
        let i = next_id();
        name_mapping.insert(name, i);
        lanes.insert(i, LaneSender::new(io, kind));
    }

    loop {
        let flush = flush_lane(&mut lanes, &mut needs_flush);
        let next = if remotes.is_empty() {
            match immediate_or_join(timeout(config.inactive_timeout, reg_stream.next()), flush)
                .await
            {
                (Ok(Some(reg)), _) => ReadTaskEvent::Registration(reg),
                (Err(_), _) => ReadTaskEvent::Timeout,
                _ => {
                    break;
                }
            }
        } else {
            let select_next = timeout(
                config.inactive_timeout,
                fselect(reg_stream.next(), remotes.next()),
            );
            let (result, _) = immediate_or_join(select_next, flush).await;
            match result {
                Ok(Either::Left((Some(reg), _))) => ReadTaskEvent::Registration(reg),
                Ok(Either::Left((_, _))) => {
                    info!("Terminating after registration task stopped.");
                    break;
                }
                Ok(Either::Right((Some(Ok(envelope)), _))) => ReadTaskEvent::Envelope(envelope),
                Ok(Either::Right((Some(Err(error)), _))) => {
                    error!(error = ?error, "Failed reading from lane: {}", error);
                    continue;
                }
                Ok(Either::Right((_, _))) => {
                    continue;
                }
                Err(_) => ReadTaskEvent::Timeout,
            }
        };
        match next {
            ReadTaskEvent::Registration(reg) => match reg {
                ReadTaskRegistration::Lane { name, sender } => {
                    let id = next_id();
                    info!(
                        "Reading from new lane named '{}'. Assigned ID is {}.",
                        name, id
                    );
                    name_mapping.insert(name, id);
                    lanes.insert(id, sender);
                }
                ReadTaskRegistration::Remote {
                    reader,
                    on_attached,
                } => {
                    info!("Reading from new remote endpoint.");
                    let rx = remote_receiver(reader).stop_after_error();
                    remotes.push(rx);
                    if let Some(on_attached) = on_attached {
                        on_attached.trigger();
                    }
                }
            },
            ReadTaskEvent::Envelope(msg) => {
                if voted {
                    trace!("Attempting to rescind stop vote.");
                    if stop_vote.rescind() {
                        info!(STOP_VOTED);
                        break;
                    } else {
                        info!("Vote to stop rescinded.");
                        voted = false;
                    }
                }
                debug!(message = ?msg, "Processing envelope.");
                let RequestMessage {
                    path,
                    origin,
                    envelope,
                } = msg;

                if let Some(id) = name_mapping.get(&path.lane) {
                    if matches!(&needs_flush, Some(i) if i != id) {
                        trace!(
                            "Flushing lane '{name}' (id = {id})",
                            name = path.lane,
                            id = id
                        );
                        flush_lane(&mut lanes, &mut needs_flush).await;
                    }
                    if let Some(lane_tx) = lanes.get_mut(id) {
                        let RelativePath { lane, .. } = path;
                        let origin: Uuid = origin.into();
                        match envelope {
                            Operation::Link => {
                                debug!(
                                    "Attempting to set up link to {} from lane '{}'.",
                                    origin, lane
                                );
                                if write_tx
                                    .send(WriteTaskMessage::Coord(RwCoorindationMessage::Link {
                                        origin,
                                        lane,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    error!(TASK_COORD_ERR);
                                    break;
                                }
                            }
                            Operation::Sync => {
                                debug!(
                                    "Attempting to synchronize {} with lane '{}'.",
                                    origin, lane
                                );
                                if lane_tx.start_sync(origin).await.is_err() {
                                    error!(
                                        "Failed to communicate with lane '{}'. Removing handle.",
                                        lane
                                    );
                                    if let Some(id) = name_mapping.remove(&lane) {
                                        lanes.remove(&id);
                                    }
                                };
                            }
                            Operation::Command(body) => {
                                trace!(body = ?body, "Dispatching command envelope from {} to lane '{}'.", origin, lane);
                                match lane_tx.feed_frame(body).await {
                                    Err(LaneSendError::Io(_)) => {
                                        error!("Failed to communicate with lane '{}'. Removing handle.", lane);
                                        if let Some(id) = name_mapping.remove(&lane) {
                                            lanes.remove(&id);
                                        }
                                    }
                                    Err(LaneSendError::Extraction(error)) => {
                                        error!(error = ?error, "Received invalid envelope from {} for lane '{}'", origin, lane);
                                        if write_tx
                                            .send(WriteTaskMessage::Coord(
                                                RwCoorindationMessage::BadEnvelope {
                                                    origin,
                                                    lane,
                                                    error,
                                                },
                                            ))
                                            .await
                                            .is_err()
                                        {
                                            error!(TASK_COORD_ERR);
                                            break;
                                        }
                                    }
                                    _ => {
                                        let _ = lane_tx.flush().await;
                                        needs_flush = Some(*id);
                                    }
                                }
                            }
                            Operation::Unlink => {
                                debug!(
                                    "Attempting to stop the link to {} from lane '{}'.",
                                    origin, lane
                                );
                                if write_tx
                                    .send(WriteTaskMessage::Coord(RwCoorindationMessage::Unlink {
                                        origin,
                                        lane,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    error!(TASK_COORD_ERR);
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    info!("Recevied envelope for non-existent lane '{}'.", path.lane);
                    let flush = flush_lane(&mut lanes, &mut needs_flush);
                    let send_err = write_tx.send(WriteTaskMessage::Coord(
                        RwCoorindationMessage::UnknownLane {
                            origin: origin.into(),
                            path,
                        },
                    ));
                    let (_, result) = join(flush, send_err).await;
                    if result.is_err() {
                        error!(TASK_COORD_ERR);
                        break;
                    }
                }
            }
            ReadTaskEvent::Timeout => {
                info!(
                    "No envelopes received within {:?}. Voting to stop.",
                    config.inactive_timeout
                );
                if stop_vote.vote() {
                    info!(STOP_VOTED);
                    break;
                }
                voted = true;
            }
        }
    }
}

async fn flush_lane(lanes: &mut HashMap<u64, LaneSender>, needs_flush: &mut Option<u64>) {
    if let Some(id) = needs_flush.take() {
        if let Some(tx) = lanes.get_mut(&id) {
            if tx.flush().await.is_err() {
                lanes.remove(&id);
            }
        }
    }
}

#[derive(Debug)]
enum WriteTaskEvent {
    Message(WriteTaskMessage),
    Event { id: u64, response: RawLaneResponse },
    WriteDone(WriteResult),
    LaneFailed(u64),
    PruneRemote(Uuid),
    Timeout,
    Stop,
}

type LaneStream = StopAfterError<Either<ValueLaneReceiver, MapLaneReceiver>>;

#[derive(Debug)]
struct WriteTaskConfiguration {
    identity: RoutingAddr,
    node_uri: Text,
    runtime_config: AgentRuntimeConfig,
}

impl WriteTaskConfiguration {
    fn new(identity: RoutingAddr, node_uri: Text, runtime_config: AgentRuntimeConfig) -> Self {
        WriteTaskConfiguration {
            identity,
            node_uri,
            runtime_config,
        }
    }
}

#[derive(Debug)]
struct InactiveTimeout<'a> {
    timeout: Duration,
    timeout_delay: Pin<&'a mut Sleep>,
    enabled: bool,
}

#[derive(Debug)]
struct WriteTaskEvents<'a, S, W> {
    inactive_timeout: InactiveTimeout<'a>,
    remote_timeout: Duration,
    prune_remotes: PruneRemotes<'a>,
    message_stream: S,
    lanes: SelectAll<LaneStream>,
    pending_writes: FuturesUnordered<W>,
}

impl<'a, S, W> WriteTaskEvents<'a, S, W> {
    fn new(
        inactive_timeout: Duration,
        remote_timeout: Duration,
        timeout_delay: Pin<&'a mut Sleep>,
        prune_delay: Pin<&'a mut Sleep>,
        message_stream: S,
    ) -> Self {
        WriteTaskEvents {
            inactive_timeout: InactiveTimeout {
                timeout: inactive_timeout,
                timeout_delay,
                enabled: true,
            },
            remote_timeout,
            prune_remotes: PruneRemotes::new(prune_delay),
            message_stream,
            lanes: Default::default(),
            pending_writes: Default::default(),
        }
    }

    fn add_lane(&mut self, lane: LaneStream) {
        self.lanes.push(lane);
    }

    fn clear_lanes(&mut self) {
        self.lanes.clear()
    }

    fn schedule_write(&mut self, write: W) {
        self.pending_writes.push(write);
    }

    fn schedule_prune(&mut self, remote_id: Uuid) {
        let WriteTaskEvents {
            remote_timeout,
            prune_remotes,
            ..
        } = self;
        prune_remotes.push(remote_id, *remote_timeout);
    }

    fn disable_timeout(&mut self) {
        self.inactive_timeout.enabled = false;
    }

    fn enable_timeout(&mut self) {
        self.inactive_timeout.enabled = true;
    }
}

impl<'a, S, W> WriteTaskEvents<'a, S, W>
where
    S: Stream<Item = WriteTaskMessage> + Unpin,
    W: Future<Output = WriteResult> + Send + 'static,
{
    async fn select_next(&mut self) -> WriteTaskEvent {
        let WriteTaskEvents {
            inactive_timeout,
            message_stream,
            lanes,
            pending_writes,
            prune_remotes,
            ..
        } = self;

        let InactiveTimeout {
            timeout,
            timeout_delay,
            enabled: timeout_enabled,
        } = inactive_timeout;

        let mut delay = timeout_delay.as_mut();

        loop {
            tokio::select! {
                biased;
                maybe_remote = prune_remotes.next(), if !prune_remotes.is_empty() => {
                    if let Some(remote_id) = maybe_remote {
                        break WriteTaskEvent::PruneRemote(remote_id);
                    }
                }
                maybe_msg = message_stream.next() => {
                    break if let Some(msg) = maybe_msg {
                        if msg.generates_activity() {
                            delay.as_mut().reset(
                                Instant::now()
                                    .checked_add(*timeout)
                                    .expect("Timer overflow."),
                            );
                        }
                        WriteTaskEvent::Message(msg)
                    } else {
                        trace!("Stopping as the coordination task stopped.");
                        WriteTaskEvent::Stop
                    };
                }
                maybe_write_done = pending_writes.next(), if !pending_writes.is_empty() => {
                    if let Some(result) = maybe_write_done {
                        break WriteTaskEvent::WriteDone(result);
                    }
                }
                maybe_result = lanes.next(), if !lanes.is_empty() => {
                    match maybe_result {
                        Some(Ok((id, response))) =>  {
                            delay.as_mut().reset(
                                Instant::now()
                                    .checked_add(*timeout)
                                    .expect("Timer overflow."),
                            );
                            break WriteTaskEvent::Event { id, response };
                        },
                        Some(Err(Failed(lane_id))) => {
                            break WriteTaskEvent::LaneFailed(lane_id);
                        }
                        _ => {}
                    }
                }
                _ = &mut delay, if *timeout_enabled => {
                    break if lanes.is_empty() {
                        trace!("Stopping as there are no active lanes.");
                        WriteTaskEvent::Stop
                    } else {
                        WriteTaskEvent::Timeout
                    };
                }
            };
        }
    }

    async fn next_write(&mut self) -> Option<WriteResult> {
        let WriteTaskEvents { pending_writes, .. } = self;
        if pending_writes.is_empty() {
            None
        } else {
            pending_writes.next().await
        }
    }
}

#[derive(Debug)]
struct WriteTaskState {
    links: Links,
    remote_tracker: RemoteTracker,
}

#[derive(Debug)]
enum TaskMessageResult<W> {
    AddLane(LaneStream),
    ScheduleWrite {
        write: W,
        schedule_prune: Option<Uuid>,
    },
    AddPruneTimeout(Uuid),
    Nothing,
}

impl<W> From<Option<W>> for TaskMessageResult<W> {
    fn from(opt: Option<W>) -> Self {
        if let Some(write) = opt {
            TaskMessageResult::ScheduleWrite {
                write,
                schedule_prune: None,
            }
        } else {
            TaskMessageResult::Nothing
        }
    }
}

fn discard_error<W>(error: InvalidKey) -> Option<W> {
    warn!("Discarding invalid map lane event: {}.", error);
    None
}

enum Writes<W> {
    Zero,
    Single(W),
    Two(W, W),
}

impl<W> Default for Writes<W> {
    fn default() -> Self {
        Writes::Zero
    }
}

impl<W> From<Option<W>> for Writes<W> {
    fn from(opt: Option<W>) -> Self {
        match opt {
            Some(w) => Writes::Single(w),
            _ => Writes::Zero,
        }
    }
}

impl<W> From<(Option<W>, Option<W>)> for Writes<W> {
    fn from(pair: (Option<W>, Option<W>)) -> Self {
        match pair {
            (Some(w1), Some(w2)) => Writes::Two(w1, w2),
            (Some(w), _) => Writes::Single(w),
            (_, Some(w)) => Writes::Single(w),
            _ => Writes::Zero,
        }
    }
}

impl<W> Iterator for Writes<W> {
    type Item = W;

    fn next(&mut self) -> Option<Self::Item> {
        match std::mem::take(self) {
            Writes::Two(w1, w2) => {
                *self = Writes::Single(w2);
                Some(w1)
            }
            Writes::Single(w) => Some(w),
            _ => None,
        }
    }
}

impl WriteTaskState {
    fn new(identity: RoutingAddr, node_uri: Text) -> Self {
        WriteTaskState {
            links: Default::default(),
            remote_tracker: RemoteTracker::new(identity, node_uri),
        }
    }

    #[must_use]
    fn handle_task_message(&mut self, reg: WriteTaskMessage) -> TaskMessageResult<WriteTask> {
        let WriteTaskState {
            links,
            remote_tracker,
            ..
        } = self;
        match reg {
            WriteTaskMessage::Lane(endpoint) => {
                let lane_stream = endpoint.into_lane_stream(remote_tracker.lane_registry());
                TaskMessageResult::AddLane(lane_stream)
            }
            WriteTaskMessage::Remote {
                id,
                writer,
                completion,
                on_attached,
            } => {
                remote_tracker.insert(id, writer, completion);
                if let Some(on_attached) = on_attached {
                    on_attached.trigger();
                }
                TaskMessageResult::AddPruneTimeout(id)
            }
            WriteTaskMessage::Coord(RwCoorindationMessage::Link { origin, lane }) => {
                info!("Attempting to set up link from '{}' to {}.", lane, origin);
                match remote_tracker.lane_registry().id_for(lane.as_str()) {
                    Some(id) if remote_tracker.has_remote(origin) => {
                        links.insert(id, origin);
                        remote_tracker
                            .push_special(SpecialAction::Linked(id), &origin)
                            .into()
                    }
                    Some(_) => {
                        error!("No remote with ID {}.", origin);
                        TaskMessageResult::Nothing
                    }
                    _ => {
                        if remote_tracker.has_remote(origin) {
                            error!("No lane named '{}'.", lane);
                        } else {
                            error!("No lane named '{}' or remote with ID {}.", lane, origin);
                        }
                        TaskMessageResult::Nothing
                    }
                }
            }
            WriteTaskMessage::Coord(RwCoorindationMessage::Unlink { origin, lane }) => {
                info!(
                    "Attempting to close any link from '{}' to {}.",
                    lane, origin
                );
                if let Some(lane_id) = remote_tracker.lane_registry().id_for(lane.as_str()) {
                    if links.is_linked(origin, lane_id) {
                        let schedule_prune = links.remove(lane_id, origin).into_option();
                        let message = Text::new("Link closed.");
                        let maybe_write = remote_tracker
                            .push_special(SpecialAction::unlinked(lane_id, message), &origin);
                        if let Some(write) = maybe_write {
                            TaskMessageResult::ScheduleWrite {
                                write,
                                schedule_prune,
                            }
                        } else if let Some(remote_id) = schedule_prune {
                            TaskMessageResult::AddPruneTimeout(remote_id)
                        } else {
                            TaskMessageResult::Nothing
                        }
                    } else {
                        info!("Lane {} is not linked to {}.", lane, origin);
                        TaskMessageResult::Nothing
                    }
                } else {
                    error!("No lane named '{}'.", lane);
                    TaskMessageResult::Nothing
                }
            }
            WriteTaskMessage::Coord(RwCoorindationMessage::UnknownLane { origin, path }) => {
                info!(
                    "Received envelope for non-existent lane '{}' from {}.",
                    path.lane, origin
                );
                remote_tracker
                    .push_special(SpecialAction::lane_not_found(path.lane), &origin)
                    .into()
            }
            WriteTaskMessage::Coord(RwCoorindationMessage::BadEnvelope {
                origin,
                lane,
                error,
            }) => {
                info!(error = ?error, "Received in invalid envelope for lane '{}' from {}.", lane, origin);
                TaskMessageResult::Nothing
            }
        }
    }

    fn handle_event(
        &mut self,
        id: u64,
        response: RawLaneResponse,
    ) -> impl Iterator<Item = WriteTask> + '_ {
        let WriteTaskState {
            links,
            remote_tracker: write_tracker,
            ..
        } = self;

        use either::Either;

        let RawLaneResponse { target, response } = response;
        if let Some(remote_id) = target {
            trace!(response = ?response, "Routing response to {}.", remote_id);
            let write = if response.is_synced() && !links.is_linked(remote_id, id) {
                trace!(response = ?response, "Sending implicit linked message to {}.", remote_id);
                links.insert(id, remote_id);
                let write1 = write_tracker.push_special(SpecialAction::Linked(id), &remote_id);
                let write2 = write_tracker
                    .push_write(id, response, &remote_id)
                    .unwrap_or_else(discard_error);
                Writes::from((write1, write2))
            } else {
                Writes::from(
                    write_tracker
                        .push_write(id, response, &remote_id)
                        .unwrap_or_else(discard_error),
                )
            };
            Either::Left(write)
        } else if let Some(targets) = links.linked_from(id) {
            trace!(response = ?response, targets = ?targets, "Broadcasting response to all linked remotes.");
            Either::Right(targets.iter().zip(std::iter::repeat(response)).flat_map(
                move |(remote_id, response)| {
                    write_tracker
                        .push_write(id, response, remote_id)
                        .unwrap_or_else(discard_error)
                },
            ))
        } else {
            trace!(response = ?response, "Discarding response.");
            Either::Left(Writes::Zero)
        }
    }

    fn remove_remote(&mut self, remote_id: Uuid, reason: DisconnectionReason) {
        info!("Removing remote connection {}.", remote_id);
        self.links.remove_remote(remote_id);
        self.remote_tracker.remove_remote(remote_id, reason);
    }

    fn remove_remote_if_idle(&mut self, remote_id: Uuid) {
        if self.links.linked_to(remote_id).is_none() {
            self.remove_remote(remote_id, DisconnectionReason::RemoteTimedOut);
        }
    }

    fn remove_lane(
        &mut self,
        lane_id: u64,
    ) -> impl Iterator<Item = (TriggerUnlink, Option<WriteTask>)> + '_ {
        let WriteTaskState {
            links,
            remote_tracker: write_tracker,
            ..
        } = self;
        info!("Attempting to remove lane with id {}.", lane_id);
        write_tracker.remove_lane(lane_id);
        let linked_remotes = links.remove_lane(lane_id);
        linked_remotes.into_iter().map(move |unlink| {
            let TriggerUnlink { remote_id, .. } = unlink;
            info!(
                "Unlinking remote {} connected to lane with id {}.",
                remote_id, lane_id
            );
            let task = write_tracker.unlink_lane(remote_id, lane_id);
            (unlink, task)
        })
    }

    fn replace(&mut self, writer: RemoteSender, buffer: BytesMut) -> Option<WriteTask> {
        let WriteTaskState { remote_tracker, .. } = self;
        trace!(
            "Replacing writer {} after completed write.",
            writer.remote_id()
        );
        remote_tracker.replace_and_pop(writer, buffer)
    }

    fn has_remotes(&self) -> bool {
        !self.remote_tracker.is_empty()
    }

    fn unlink_all(&mut self) -> impl Iterator<Item = WriteTask> + '_ {
        info!("Unlinking all open links for shutdown.");
        let WriteTaskState {
            links,
            remote_tracker,
        } = self;
        links
            .all_links()
            .flat_map(move |(lane_id, remote_id)| remote_tracker.unlink_lane(remote_id, lane_id))
    }

    fn dispose_of_remotes(self, reason: DisconnectionReason) {
        let WriteTaskState { remote_tracker, .. } = self;
        remote_tracker.dispose_of_remotes(reason);
    }
}

async fn write_task(
    configuration: WriteTaskConfiguration,
    initial_endpoints: Vec<LaneEndpoint<ByteReader>>,
    message_rx: mpsc::Receiver<WriteTaskMessage>,
    stop_voter: timeout_coord::Voter,
    stopping: trigger::Receiver,
) {
    let message_stream = ReceiverStream::new(message_rx).take_until(stopping);

    let WriteTaskConfiguration {
        identity,
        node_uri,
        runtime_config,
    } = configuration;

    let timeout_delay = sleep(runtime_config.inactive_timeout);
    let remote_prune_delay = sleep(Duration::ZERO);

    pin_mut!(timeout_delay);
    pin_mut!(remote_prune_delay);
    let mut streams = WriteTaskEvents::new(
        runtime_config.inactive_timeout,
        runtime_config.prune_remote_delay,
        timeout_delay.as_mut(),
        remote_prune_delay,
        message_stream,
    );
    let mut state = WriteTaskState::new(identity, node_uri);

    info!(endpoints = ?initial_endpoints, "Adding initial endpoints.");
    for endpoint in initial_endpoints {
        let lane_stream = endpoint.into_lane_stream(state.remote_tracker.lane_registry());
        streams.add_lane(lane_stream);
    }

    let mut voted = false;

    let mut remote_reason = DisconnectionReason::AgentStoppedExternally;

    loop {
        let next = streams.select_next().await;
        match next {
            WriteTaskEvent::Message(reg) => match state.handle_task_message(reg) {
                TaskMessageResult::AddLane(lane) => {
                    streams.add_lane(lane);
                }
                TaskMessageResult::ScheduleWrite {
                    write,
                    schedule_prune,
                } => {
                    if voted {
                        if stop_voter.rescind() {
                            info!(STOP_VOTED);
                            remote_reason = DisconnectionReason::AgentTimedOut;
                            break;
                        }
                        streams.enable_timeout();
                        voted = false;
                    }
                    streams.schedule_write(write.into_future());
                    if let Some(remote_id) = schedule_prune {
                        streams.schedule_prune(remote_id);
                    }
                }
                TaskMessageResult::AddPruneTimeout(remote_id) => {
                    streams.schedule_prune(remote_id);
                }
                TaskMessageResult::Nothing => {}
            },
            WriteTaskEvent::Event { id, response } => {
                if voted {
                    if stop_voter.rescind() {
                        info!(STOP_VOTED);
                        remote_reason = DisconnectionReason::AgentTimedOut;
                        break;
                    }
                    streams.enable_timeout();
                    voted = false;
                }
                for write in state.handle_event(id, response) {
                    streams.schedule_write(write.into_future());
                }
            }
            WriteTaskEvent::WriteDone((writer, buffer, result)) => {
                if result.is_ok() {
                    if let Some(write) = state.replace(writer, buffer) {
                        streams.schedule_write(write.into_future());
                    }
                } else {
                    let remote_id = writer.remote_id();
                    error!(
                        "Writing to remote {} failed. Removing attached uplinks.",
                        remote_id
                    );
                    state.remove_remote(remote_id, DisconnectionReason::ChannelClosed);
                }
            }
            WriteTaskEvent::LaneFailed(lane_id) => {
                error!(
                    "Lane with ID {} failed. Unlinking all attached uplinks.",
                    lane_id
                );
                for (unlink, maybe_write) in state.remove_lane(lane_id) {
                    if let Some(write) = maybe_write {
                        streams.schedule_write(write.into_future());
                    }
                    let TriggerUnlink {
                        remote_id,
                        schedule_prune,
                    } = unlink;
                    if schedule_prune {
                        streams.schedule_prune(remote_id);
                    }
                }
            }
            WriteTaskEvent::PruneRemote(remote_id) => {
                state.remove_remote_if_idle(remote_id);
            }
            WriteTaskEvent::Timeout => {
                info!(
                    "No events sent within {:?}, voting to stop.",
                    runtime_config.inactive_timeout
                );
                if !state.has_remotes() {
                    info!("Stopping after timeout with no remotes.");
                    break;
                }
                voted = true;
                streams.disable_timeout();
                if stop_voter.vote() {
                    info!(STOP_VOTED);
                    remote_reason = DisconnectionReason::AgentTimedOut;
                    break;
                }
            }
            WriteTaskEvent::Stop => {
                info!("Write task stopping.");
                break;
            }
        }
    }
    let cleanup_result = timeout(runtime_config.shutdown_timeout, async move {
        info!("Unlinking all links on shutdown.");
        streams.clear_lanes();
        for write in state.unlink_all() {
            streams.schedule_write(write.into_future());
        }
        while let Some((writer, buffer, result)) = streams.next_write().await {
            if result.is_ok() {
                if let Some(write) = state.replace(writer, buffer) {
                    streams.schedule_write(write.into_future());
                }
            }
        }
        state.dispose_of_remotes(remote_reason);
    })
    .await;
    if cleanup_result.is_err() {
        error!(
            "Unlinking lanes on shutdown did not complete within {:?}.",
            runtime_config.shutdown_timeout
        );
    }
}

async fn await_io_tasks<F1, F2>(read: F1, write: F2, kill_switch_tx: trigger::Sender)
where
    F1: Future<Output = ()>,
    F2: Future<Output = ()>,
{
    pin_mut!(read);
    pin_mut!(write);
    let first_finished = fselect(read, write).await;
    kill_switch_tx.trigger();
    match first_finished {
        Either::Left((_, write_fut)) => write_fut.await,
        Either::Right((_, read_fut)) => read_fut.await,
    }
}
