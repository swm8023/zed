pub mod platform_info;
mod project_types;

use anyhow::{Context as _, Result, anyhow};
use clock::SystemClock;
use futures::{FutureExt as _, Stream, StreamExt as _, TryFutureExt as _, future::BoxFuture, stream::BoxStream};
use gpui::{App, AsyncApp, Entity, Global, Task, TaskExt, WeakEntity};
use http_client::{HttpClient, HttpClientWithUrl, read_proxy_from_env};
use parking_lot::Mutex;
use rpc::proto::{AnyTypedEnvelope, EnvelopedMessage, PeerId, RequestMessage};
use serde::Deserialize;
use settings::{RegisterSetting, Settings, SettingsContent};
use std::{
    any::TypeId,
    future::Future,
    marker::PhantomData,
    path::PathBuf,
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use url::Url;
use util::ResultExt as _;

pub use rpc::*;
pub use project_types::*;

pub static ZED_APP_PATH: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var("ZED_APP_PATH").ok().map(PathBuf::from));

pub static ZED_ALWAYS_ACTIVE: LazyLock<bool> =
    LazyLock::new(|| std::env::var("ZED_ALWAYS_ACTIVE").is_ok_and(|e| !e.is_empty()));

#[derive(Deserialize, Default, RegisterSetting)]
pub struct ProxySettings {
    pub proxy: Option<String>,
}

impl ProxySettings {
    pub fn proxy_url(&self) -> Option<Url> {
        self.proxy
            .as_deref()
            .map(str::trim)
            .filter(|input| !input.is_empty())
            .and_then(|input| {
                input
                    .parse::<Url>()
                    .inspect_err(|e| log::error!("Error parsing proxy settings: {}", e))
                    .ok()
            })
            .or_else(read_proxy_from_env)
    }
}

impl Settings for ProxySettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self {
            proxy: content
                .proxy
                .as_deref()
                .map(str::trim)
                .filter(|proxy| !proxy.is_empty())
                .map(ToOwned::to_owned),
        }
    }
}

struct GlobalClient(Arc<Client>);

impl Global for GlobalClient {}

pub struct Client {
    id: AtomicU64,
    peer: Arc<Peer>,
    http: Arc<HttpClientWithUrl>,
    handler_set: Mutex<ProtoMessageHandlerSet>,
}

pub enum Subscription {
    Entity {
        client: Weak<Client>,
        id: (TypeId, u64),
    },
    Message {
        client: Weak<Client>,
        id: TypeId,
    },
}

impl Drop for Subscription {
    fn drop(&mut self) {
        match self {
            Subscription::Entity { client, id } => {
                if let Some(client) = client.upgrade() {
                    let mut state = client.handler_set.lock();
                    let _ = state.entities_by_type_and_remote_id.remove(id);
                }
            }
            Subscription::Message { client, id } => {
                if let Some(client) = client.upgrade() {
                    let mut state = client.handler_set.lock();
                    let _ = state.entity_types_by_message_type.remove(id);
                    let _ = state.message_handlers.remove(id);
                }
            }
        }
    }
}

pub struct PendingEntitySubscription<T: 'static> {
    client: Arc<Client>,
    remote_id: u64,
    _entity_type: PhantomData<T>,
    consumed: bool,
}

impl<T: 'static> PendingEntitySubscription<T> {
    pub fn set_entity(mut self, entity: &Entity<T>, cx: &AsyncApp) -> Subscription {
        self.consumed = true;
        let mut handlers = self.client.handler_set.lock();
        let id = (TypeId::of::<T>(), self.remote_id);
        let Some(EntityMessageSubscriber::Pending(messages)) =
            handlers.entities_by_type_and_remote_id.remove(&id)
        else {
            unreachable!()
        };

        handlers.entities_by_type_and_remote_id.insert(
            id,
            EntityMessageSubscriber::Entity {
                handle: entity.downgrade().into(),
            },
        );
        drop(handlers);
        for message in messages {
            let client_id = self.client.id();
            let type_name = message.payload_type_name();
            let sender_id = message.original_sender_id();
            log::debug!(
                "handling queued rpc message. client_id:{}, sender_id:{:?}, type:{}",
                client_id,
                sender_id,
                type_name
            );
            self.client.handle_message(message, cx);
        }
        Subscription::Entity {
            client: Arc::downgrade(&self.client),
            id,
        }
    }
}

impl<T: 'static> Drop for PendingEntitySubscription<T> {
    fn drop(&mut self) {
        if !self.consumed {
            let mut state = self.client.handler_set.lock();
            if let Some(EntityMessageSubscriber::Pending(messages)) = state
                .entities_by_type_and_remote_id
                .remove(&(TypeId::of::<T>(), self.remote_id))
            {
                for message in messages {
                    log::info!("unhandled message {}", message.payload_type_name());
                }
            }
        }
    }
}

impl Client {
    pub fn new(
        _clock: Arc<dyn SystemClock>,
        http: Arc<HttpClientWithUrl>,
        _cx: &mut App,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: AtomicU64::new(0),
            peer: Peer::new(0),
            http,
            handler_set: Default::default(),
        })
    }

    pub fn production(cx: &mut App) -> Arc<Self> {
        let clock = Arc::new(clock::RealSystemClock);
        let http = Arc::new(HttpClientWithUrl::new_url(
            cx.http_client(),
            "",
            cx.http_client().proxy().cloned(),
        ));
        Self::new(clock, http, cx)
    }


    pub fn id(&self) -> u64 {
        self.id.load(Ordering::SeqCst)
    }

    pub fn http_client(&self) -> Arc<HttpClientWithUrl> {
        self.http.clone()
    }

    pub fn set_id(&self, id: u64) -> &Self {
        self.id.store(id, Ordering::SeqCst);
        self
    }

    pub fn global(cx: &App) -> Arc<Self> {
        cx.global::<GlobalClient>().0.clone()
    }
    pub fn set_global(client: Arc<Client>, cx: &mut App) {
        cx.set_global(GlobalClient(client))
    }

    pub fn peer_id(&self) -> Option<PeerId> {
        None
    }

    pub fn subscribe_to_entity<T>(
        self: &Arc<Self>,
        remote_id: u64,
    ) -> Result<PendingEntitySubscription<T>>
    where
        T: 'static,
    {
        let id = (TypeId::of::<T>(), remote_id);

        let mut state = self.handler_set.lock();
        anyhow::ensure!(
            !state.entities_by_type_and_remote_id.contains_key(&id),
            "already subscribed to entity"
        );

        state
            .entities_by_type_and_remote_id
            .insert(id, EntityMessageSubscriber::Pending(Default::default()));

        Ok(PendingEntitySubscription {
            client: self.clone(),
            remote_id,
            consumed: false,
            _entity_type: PhantomData,
        })
    }

    #[track_caller]
    pub fn add_message_handler<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: EnvelopedMessage,
        E: 'static,
        H: 'static + Sync + Fn(Entity<E>, TypedEnvelope<M>, AsyncApp) -> F + Send + Sync,
        F: 'static + Future<Output = Result<()>>,
    {
        self.add_message_handler_impl(entity, move |entity, message, _, cx| {
            handler(entity, message, cx)
        })
    }

    fn add_message_handler_impl<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: EnvelopedMessage,
        E: 'static,
        H: 'static
            + Sync
            + Fn(Entity<E>, TypedEnvelope<M>, AnyProtoClient, AsyncApp) -> F
            + Send
            + Sync,
        F: 'static + Future<Output = Result<()>>,
    {
        let message_type_id = TypeId::of::<M>();
        let mut state = self.handler_set.lock();
        state
            .entities_by_message_type
            .insert(message_type_id, entity.into());

        let prev_handler = state.message_handlers.insert(
            message_type_id,
            Arc::new(move |subscriber, envelope, client, cx| {
                let subscriber = subscriber.downcast::<E>().unwrap();
                let envelope = envelope.into_any().downcast::<TypedEnvelope<M>>().unwrap();
                handler(subscriber, *envelope, client, cx).boxed_local()
            }),
        );
        if prev_handler.is_some() {
            let location = std::panic::Location::caller();
            panic!(
                "{}:{} registered handler for the same message {} twice",
                location.file(),
                location.line(),
                std::any::type_name::<M>()
            );
        }

        Subscription::Message {
            client: Arc::downgrade(self),
            id: message_type_id,
        }
    }

    pub fn add_request_handler<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: RequestMessage,
        E: 'static,
        H: 'static + Sync + Fn(Entity<E>, TypedEnvelope<M>, AsyncApp) -> F + Send + Sync,
        F: 'static + Future<Output = Result<M::Response>>,
    {
        self.add_message_handler_impl(entity, move |handle, envelope, this, cx| {
            Self::respond_to_request(envelope.receipt(), handler(handle, envelope, cx), this)
        })
    }

    async fn respond_to_request<T: RequestMessage, F: Future<Output = Result<T::Response>>>(
        receipt: Receipt<T>,
        response: F,
        client: AnyProtoClient,
    ) -> Result<()> {
        match response.await {
            Ok(response) => {
                client.send_response(receipt.message_id, response)?;
                Ok(())
            }
            Err(error) => {
                client.send_response(receipt.message_id, error.to_proto())?;
                Err(error)
            }
        }
    }

    fn connection_id(&self) -> Result<ConnectionId> {
        anyhow::bail!("Collaboration is unavailable")
    }

    pub fn send<T: EnvelopedMessage>(&self, message: T) -> Result<()> {
        log::debug!("rpc send. client_id:{}, name:{}", self.id(), T::NAME);
        self.peer.send(self.connection_id()?, message)
    }

    pub fn request<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<T::Response>> + use<T> {
        self.request_envelope(request)
            .map_ok(|envelope| envelope.payload)
    }

    pub fn request_stream<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<impl Stream<Item = Result<T::Response>>>> {
        let client_id = self.id.load(Ordering::SeqCst);
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            T::NAME
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_stream(conn_id, request));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                T::NAME
            );
            response
        }
    }

    pub fn request_envelope<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<TypedEnvelope<T::Response>>> + use<T> {
        let client_id = self.id();
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            T::NAME
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_envelope(conn_id, request));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                T::NAME
            );
            response
        }
    }

    pub fn request_dynamic(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> impl Future<Output = Result<proto::Envelope>> + use<> {
        let client_id = self.id();
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            request_type
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_dynamic(conn_id, envelope, request_type));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                request_type
            );
            Ok(response?.0)
        }
    }

    fn handle_message(self: &Arc<Client>, message: Box<dyn AnyTypedEnvelope>, cx: &AsyncApp) {
        let sender_id = message.sender_id();
        let request_id = message.message_id();
        let type_name = message.payload_type_name();
        let original_sender_id = message.original_sender_id();

        if let Some(future) = ProtoMessageHandlerSet::handle_message(
            &self.handler_set,
            message,
            self.clone().into(),
            cx.clone(),
        ) {
            let client_id = self.id();
            log::debug!(
                "rpc message received. client_id:{}, sender_id:{:?}, type:{}",
                client_id,
                original_sender_id,
                type_name
            );
            cx.spawn(async move |_| match future.await {
                Ok(()) => {
                    log::debug!("rpc message handled. client_id:{client_id}, sender_id:{original_sender_id:?}, type:{type_name}");
                }
                Err(error) => {
                    log::error!("error handling message. client_id:{client_id}, sender_id:{original_sender_id:?}, type:{type_name}, error:{error:#}");
                }
            })
            .detach();
        } else {
            log::info!("unhandled message {}", type_name);
            self.peer
                .respond_with_unhandled_message(sender_id.into(), request_id, type_name)
                .log_err();
        }
    }

}

impl ProtoClient for Client {
    fn request(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> BoxFuture<'static, Result<proto::Envelope>> {
        self.request_dynamic(envelope, request_type).boxed()
    }

    fn request_stream(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<proto::Envelope>>>> {
        let client_id = self.id();
        let response = self.connection_id().map(|connection_id| {
            self.peer
                .request_stream_dynamic(connection_id, envelope, request_type)
        });

        async move {
            log::debug!(
                "rpc stream request start. client_id:{}. name:{}",
                client_id,
                request_type
            );
            let response = response?.await;
            log::debug!(
                "rpc stream request opened. client_id:{}. name:{}",
                client_id,
                request_type
            );
            response
        }
        .boxed()
    }

    fn send(&self, envelope: proto::Envelope, message_type: &'static str) -> Result<()> {
        log::debug!("rpc send. client_id:{}, name:{}", self.id(), message_type);
        let connection_id = self.connection_id()?;
        self.peer.send_dynamic(connection_id, envelope)
    }

    fn send_response(&self, envelope: proto::Envelope, message_type: &'static str) -> Result<()> {
        log::debug!(
            "rpc respond. client_id:{}, name:{}",
            self.id(),
            message_type
        );
        let connection_id = self.connection_id()?;
        self.peer.send_dynamic(connection_id, envelope)
    }

    fn message_handler_set(&self) -> &Mutex<ProtoMessageHandlerSet> {
        &self.handler_set
    }

    fn is_via_collab(&self) -> bool {
        false
    }

    fn has_wsl_interop(&self) -> bool {
        false
    }
}

/// prefix for the zed:// url scheme
pub const ZED_URL_SCHEME: &str = "zed";

#[cfg(test)]
mod tests {
    use super::ProxySettings;

    #[test]
    fn proxy_settings_ignore_whitespace() {
        let settings = ProxySettings { proxy: Some(" http://127.0.0.1:8765 ".into()) };
        assert_eq!(settings.proxy_url().unwrap().as_str(), "http://127.0.0.1:8765/");
    }
}
