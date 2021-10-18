// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use async_std::stream::{self, Interval};
use async_std::task;
use futures::prelude::*;
use libp2p::swarm::DialError;
use libp2p::{
    core::{
        connection::{ConnectionId, ListenerId},
        ConnectedPoint, Multiaddr, PeerId, PublicKey,
    },
    kad::{handler::KademliaHandlerProto, Kademlia, KademliaConfig, KademliaEvent, QueryId},
    mdns::MdnsEvent,
    multiaddr::Protocol,
    swarm::{
        toggle::{Toggle, ToggleIntoProtoHandler},
        IntoProtocolsHandler, NetworkBehaviour, NetworkBehaviourAction, PollParameters,
        ProtocolsHandler,
    },
};
use libp2p::{kad::record::store::MemoryStore, mdns::Mdns};
use log::{debug, error, trace, warn};
use std::collections::HashMap;
use std::{
    cmp,
    collections::{HashSet, VecDeque},
    io,
    task::{Context, Poll},
    time::Duration,
};

/// Event generated by the `DiscoveryBehaviour`.
#[derive(Debug)]
pub enum DiscoveryOut {
    /// Event that notifies that we connected to the node with the given peer id.
    Connected(PeerId),

    /// Event that notifies that we disconnected with the node with the given peer id.
    Disconnected(PeerId),
}

/// `DiscoveryBehaviour` configuration.
///
/// Note: In order to discover nodes or load and store values via Kademlia one has to add at least
///       one protocol via [`DiscoveryConfig::add_protocol`].
pub struct DiscoveryConfig<'a> {
    local_peer_id: PeerId,
    user_defined: Vec<Multiaddr>,
    discovery_max: u64,
    enable_mdns: bool,
    enable_kademlia: bool,
    network_name: &'a str,
}

impl<'a> DiscoveryConfig<'a> {
    /// Create a default configuration with the given public key.
    pub fn new(local_public_key: PublicKey, network_name: &'a str) -> Self {
        DiscoveryConfig {
            local_peer_id: local_public_key.to_peer_id(),
            user_defined: Vec::new(),
            discovery_max: std::u64::MAX,
            enable_mdns: false,
            enable_kademlia: true,
            network_name,
        }
    }

    /// Set the number of active connections at which we pause discovery.
    pub fn discovery_limit(&mut self, limit: u64) -> &mut Self {
        self.discovery_max = limit;
        self
    }

    /// Set custom nodes which never expire, e.g. bootstrap or reserved nodes.
    pub fn with_user_defined<I>(&mut self, user_defined: I) -> &mut Self
    where
        I: IntoIterator<Item = Multiaddr>,
    {
        self.user_defined.extend(user_defined);
        self
    }

    /// Configures if mdns is enabled.
    pub fn with_mdns(&mut self, value: bool) -> &mut Self {
        self.enable_mdns = value;
        self
    }

    /// Configures if Kademlia is enabled.
    pub fn with_kademlia(&mut self, value: bool) -> &mut Self {
        self.enable_kademlia = value;
        self
    }

    /// Create a `DiscoveryBehaviour` from this config.
    pub fn finish(self) -> DiscoveryBehaviour {
        let DiscoveryConfig {
            local_peer_id,
            user_defined,
            discovery_max,
            enable_mdns,
            enable_kademlia,
            network_name,
        } = self;

        let mut peers = HashSet::new();
        let peer_addresses = HashMap::new();

        // Kademlia config
        let store = MemoryStore::new(local_peer_id.to_owned());
        let mut kad_config = KademliaConfig::default();
        let network = format!("/fil/kad/{}/kad/1.0.0", network_name);
        kad_config.set_protocol_name(network.as_bytes().to_vec());

        // TODO this parsing should probably be done when parsing config, not initializing node
        let user_defined: Vec<(PeerId, Multiaddr)> = user_defined
            .into_iter()
            .filter_map(|multiaddr| {
                let mut addr = multiaddr.to_owned();
                if let Some(Protocol::P2p(mh)) = addr.pop() {
                    let peer_id = PeerId::from_multihash(mh).unwrap();
                    Some((peer_id, addr))
                } else {
                    warn!("Could not parse bootstrap addr {}", multiaddr);
                    None
                }
            })
            .collect();

        let kademlia_opt = if enable_kademlia {
            let mut kademlia = Kademlia::with_config(local_peer_id, store, kad_config);
            for (peer_id, addr) in user_defined.iter() {
                kademlia.add_address(peer_id, addr.clone());
                peers.insert(*peer_id);
            }
            if let Err(e) = kademlia.bootstrap() {
                warn!("Kademlia bootstrap failed: {}", e);
            }
            Some(kademlia)
        } else {
            None
        };

        let mdns_opt = if enable_mdns {
            Some(task::block_on(async {
                Mdns::new(Default::default())
                    .await
                    .expect("Could not start mDNS")
            }))
        } else {
            None
        };

        DiscoveryBehaviour {
            user_defined,
            kademlia: kademlia_opt.into(),
            next_kad_random_query: stream::interval(Duration::new(0, 0)),
            duration_to_next_kad: Duration::from_secs(1),
            pending_events: VecDeque::new(),
            num_connections: 0,
            mdns: mdns_opt.into(),
            peers,
            peer_addresses,
            discovery_max,
        }
    }
}

/// Implementation of `NetworkBehaviour` that discovers the nodes on the network.
pub struct DiscoveryBehaviour {
    /// User-defined list of nodes and their addresses. Typically includes bootstrap nodes and
    /// reserved nodes.
    user_defined: Vec<(PeerId, Multiaddr)>,
    /// Kademlia discovery.
    kademlia: Toggle<Kademlia<MemoryStore>>,
    /// Discovers nodes on the local network.
    mdns: Toggle<Mdns>,
    /// Stream that fires when we need to perform the next random Kademlia query.
    next_kad_random_query: Interval,
    /// After `next_kad_random_query` triggers, the next one triggers after this duration.
    duration_to_next_kad: Duration,
    /// Events to return in priority when polled.
    pending_events: VecDeque<DiscoveryOut>,
    /// Number of nodes we're currently connected to.
    num_connections: u64,
    /// Keeps hash set of peers connected.
    peers: HashSet<PeerId>,
    /// Keeps hash map of peers and their multiaddresses
    peer_addresses: HashMap<PeerId, Vec<Multiaddr>>,
    /// Number of active connections to pause discovery on.
    discovery_max: u64,
}

impl DiscoveryBehaviour {
    /// Returns reference to peer set.
    pub fn peers(&self) -> &HashSet<PeerId> {
        &self.peers
    }

    /// Returns a map of peer ids and their multiaddresses
    pub fn peer_addresses(&self) -> &HashMap<PeerId, Vec<Multiaddr>> {
        &self.peer_addresses
    }

    /// Bootstrap Kademlia network
    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        if let Some(active_kad) = self.kademlia.as_mut() {
            active_kad.bootstrap().map_err(|e| e.to_string())
        } else {
            Err("Kademlia is not activated".to_string())
        }
    }
}

impl NetworkBehaviour for DiscoveryBehaviour {
    type ProtocolsHandler = ToggleIntoProtoHandler<KademliaHandlerProto<QueryId>>;
    type OutEvent = DiscoveryOut;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        self.kademlia.new_handler()
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let mut list = self
            .user_defined
            .iter()
            .filter_map(|(p, a)| if p == peer_id { Some(a.clone()) } else { None })
            .collect::<Vec<_>>();

        {
            let mut list_to_filter = Vec::new();
            if let Some(k) = self.kademlia.as_mut() {
                list_to_filter.extend(k.addresses_of_peer(peer_id))
            }

            list_to_filter.extend(self.mdns.addresses_of_peer(peer_id));

            list.extend(list_to_filter);
        }

        trace!("Addresses of {:?}: {:?}", peer_id, list);

        list
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        self.num_connections += 1;

        self.kademlia
            .inject_connection_established(peer_id, conn, endpoint, failed_addresses)
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        let multiaddr = self.addresses_of_peer(peer_id);
        self.peer_addresses.insert(*peer_id, multiaddr);
        self.peers.insert(*peer_id);
        self.pending_events
            .push_back(DiscoveryOut::Connected(*peer_id));

        self.kademlia.inject_connected(peer_id)
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        handler: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        self.num_connections -= 1;

        self.kademlia
            .inject_connection_closed(peer_id, conn, endpoint, handler)
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        self.peers.remove(peer_id);
        self.pending_events
            .push_back(DiscoveryOut::Disconnected(*peer_id));

        self.kademlia.inject_disconnected(peer_id)
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent,
    ) {
        if let Some(kad) = self.kademlia.as_mut() {
            return kad.inject_event(peer_id, connection, event);
        }
        error!("inject_node_event: no kademlia instance registered for protocol")
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        self.kademlia.inject_new_external_addr(addr)
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.kademlia.inject_expired_listen_addr(id, addr);
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        handler: Self::ProtocolsHandler,
        err: &DialError,
    ) {
        self.kademlia.inject_dial_failure(peer_id, handler, err)
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.kademlia.inject_new_listen_addr(id, addr)
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        self.kademlia.inject_listener_error(id, err)
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        self.kademlia.inject_listener_closed(id, reason)
    }

    #[allow(clippy::type_complexity)]
    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ProtocolsHandler>> {
        // Immediately process the content of `discovered`.
        if let Some(ev) = self.pending_events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
        }

        // Poll the stream that fires when we need to start a random Kademlia query.
        while self.next_kad_random_query.poll_next_unpin(cx).is_ready() {
            if self.num_connections < self.discovery_max {
                // We still have not hit the discovery max, send random request for peers.
                let random_peer_id = PeerId::random();
                debug!(
                    "Libp2p <= Starting random Kademlia request for {:?}",
                    random_peer_id
                );
                if let Some(k) = self.kademlia.as_mut() {
                    k.get_closest_peers(random_peer_id);
                }
            }

            // Schedule the next random query with exponentially increasing delay,
            // capped at 60 seconds.
            self.next_kad_random_query = stream::interval(self.duration_to_next_kad);
            self.duration_to_next_kad =
                cmp::min(self.duration_to_next_kad * 2, Duration::from_secs(60));
        }

        // Poll Kademlia.
        while let Poll::Ready(ev) = self.kademlia.poll(cx, params) {
            match ev {
                NetworkBehaviourAction::GenerateEvent(ev) => match ev {
                    // Adding to Kademlia buckets is automatic with our config,
                    // no need to do manually.
                    KademliaEvent::RoutingUpdated { .. } => {}
                    KademliaEvent::RoutablePeer { .. } => {}
                    KademliaEvent::PendingRoutablePeer { .. } => {
                        // Intentionally ignore
                    }
                    other => {
                        debug!("Libp2p => Unhandled Kademlia event: {:?}", other)
                    }
                },
                NetworkBehaviourAction::DialAddress { address, handler } => {
                    return Poll::Ready(NetworkBehaviourAction::DialAddress { address, handler })
                }
                NetworkBehaviourAction::DialPeer {
                    peer_id,
                    condition,
                    handler,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::DialPeer {
                        peer_id,
                        condition,
                        handler,
                    })
                }
                NetworkBehaviourAction::NotifyHandler {
                    peer_id,
                    handler,
                    event,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                        peer_id,
                        handler,
                        event,
                    })
                }
                NetworkBehaviourAction::ReportObservedAddr { address, score } => {
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr {
                        address,
                        score,
                    })
                }
                NetworkBehaviourAction::CloseConnection {
                    peer_id,
                    connection,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection,
                    })
                }
            }
        }

        // Poll mdns.
        while let Poll::Ready(ev) = self.mdns.poll(cx, params) {
            match ev {
                NetworkBehaviourAction::GenerateEvent(event) => match event {
                    MdnsEvent::Discovered(list) => {
                        if self.num_connections >= self.discovery_max {
                            // Already over discovery max, don't add discovered peers.
                            // We could potentially buffer these addresses to be added later,
                            // but mdns is not an important use case and may be removed in future.
                            continue;
                        }

                        // Add any discovered peers to Kademlia
                        for (peer_id, multiaddr) in list {
                            if let Some(kad) = self.kademlia.as_mut() {
                                kad.add_address(&peer_id, multiaddr);
                            }
                        }
                    }
                    MdnsEvent::Expired(_) => {}
                },
                NetworkBehaviourAction::DialAddress { .. } => {}
                NetworkBehaviourAction::DialPeer { .. } => {}
                // Nothing to notify handler
                NetworkBehaviourAction::NotifyHandler { event, .. } => match event {},
                NetworkBehaviourAction::ReportObservedAddr { address, score } => {
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr {
                        address,
                        score,
                    })
                }
                NetworkBehaviourAction::CloseConnection {
                    peer_id,
                    connection,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection,
                    })
                }
            }
        }

        // Poll pending events
        if let Some(ev) = self.pending_events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
        }

        Poll::Pending
    }
}
