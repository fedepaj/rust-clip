//! High-level API for the rust-clip mesh network.
//!
//! # Example
//!
//! ```no_run
//! use rust_clip::node::RustClipNode;
//! use rust_clip::core::identity::RingIdentity;
//!
//! let identity = RingIdentity::load().unwrap_or_else(|_| {
//!     RingIdentity::create_new().expect("Failed to create identity")
//! });
//!
//! let node = RustClipNode::new(identity);
//! node.run().expect("Node crashed");
//! ```

use anyhow::Result;
use std::sync::Arc;

use crate::core::identity::RingIdentity;
use crate::mesh::swarm::Swarm;
use crate::mesh::topology::Topology;
use crate::transport::lan::mdns::MdnsService;
use crate::transport::lan::udp::UdpTransport;
use crate::transport::{ChannelSender, TransportRegistry, TransportType};

// ─── Configuration ──────────────────────────────────────────────

/// Transport configuration for the node.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub enable_ble: bool,
    pub enable_lan: bool,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            enable_ble: true,
            enable_lan: true,
        }
    }
}

// ─── Node handle (for querying state from other threads) ────────

/// A thread-safe handle for querying node state while it's running.
#[derive(Clone)]
pub struct NodeHandle {
    identity: RingIdentity,
    topology: Topology,
}

impl NodeHandle {
    pub fn identity(&self) -> &RingIdentity {
        &self.identity
    }

    pub fn stable_peer_id(&self) -> &str {
        self.identity.stable_peer_id()
    }

    /// List StablePeerIds of all connected peers with active sessions.
    pub fn active_peers(&self) -> Vec<String> {
        self.topology.active_peers()
    }

    /// Number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.topology.active_peers().len()
    }
}

// ─── Node ───────────────────────────────────────────────────────

/// The main entry point for the rust-clip mesh network.
///
/// Creates and manages all transports, the swarm event loop, and the
/// clipboard monitor. Designed to be used as a library: create a node,
/// optionally get a handle for querying state, then call `run()` which
/// blocks the calling thread.
///
/// On macOS, the calling thread is used for the BLE CoreBluetooth RunLoop.
/// On Windows, a tokio runtime is created for BLE.
/// On other platforms, the thread parks until shutdown.
pub struct RustClipNode {
    identity: RingIdentity,
    config: NodeConfig,
}

impl RustClipNode {
    /// Create a new node with the given identity and default config.
    pub fn new(identity: RingIdentity) -> Self {
        Self {
            identity,
            config: NodeConfig::default(),
        }
    }

    /// Set the transport configuration.
    pub fn with_config(mut self, config: NodeConfig) -> Self {
        self.config = config;
        self
    }

    /// Enable or disable BLE transport.
    pub fn enable_ble(mut self, enabled: bool) -> Self {
        self.config.enable_ble = enabled;
        self
    }

    /// Enable or disable LAN (UDP + mDNS) transport.
    pub fn enable_lan(mut self, enabled: bool) -> Self {
        self.config.enable_lan = enabled;
        self
    }

    /// Get a handle for querying node state from other threads.
    /// Must be called before `run()` since `run()` consumes self.
    pub fn handle(&self) -> NodeHandle {
        // Topology is created fresh here — it will be shared with the swarm.
        // The caller should call handle() before run().
        // For a cleaner API, we use build() + run() pattern.
        NodeHandle {
            identity: self.identity.clone(),
            topology: Topology::new(),
        }
    }

    /// Build the node internals and return a handle + a runnable node.
    /// The handle can be used from other threads while the node is running.
    pub fn build(self) -> Result<(RunnableNode, NodeHandle)> {
        let topology = Topology::new();
        let handle = NodeHandle {
            identity: self.identity.clone(),
            topology: topology.clone(),
        };

        let runnable = RunnableNode {
            identity: self.identity,
            config: self.config,
            topology,
        };

        Ok((runnable, handle))
    }

    /// Start and run the node. Blocks the calling thread.
    ///
    /// This is a convenience method that combines `build()` + `run()`.
    /// If you need a handle to query state, use `build()` instead.
    pub fn run(self) -> Result<()> {
        let (runnable, _handle) = self.build()?;
        runnable.run()
    }
}

// ─── Runnable node (post-build, ready to run) ───────────────────

/// A node that has been built and is ready to run.
/// Created by `RustClipNode::build()`.
pub struct RunnableNode {
    identity: RingIdentity,
    config: NodeConfig,
    topology: Topology,
}

impl RunnableNode {
    /// Start and run the node. Blocks the calling thread.
    ///
    /// On macOS: the calling thread runs the BLE CoreBluetooth RunLoop.
    /// On Windows: a tokio runtime runs BLE in the background.
    /// On other platforms: the calling thread parks.
    ///
    /// The Swarm event loop always runs in a background thread.
    pub fn run(self) -> Result<()> {
        let identity = self.identity;
        let topology = self.topology;
        let config = self.config;

        println!("  [Node] Identity loaded");
        println!("  [Node] StablePeerId: {}", identity.stable_peer_id());

        // ── Build transport registry ────────────────────────────
        let mut registry = TransportRegistry::new();

        // BLE channel (Swarm → BLE transport)
        let (ble_send_tx, ble_send_rx) = flume::unbounded::<(String, Vec<u8>)>();
        if config.enable_ble {
            registry.register(Arc::new(ChannelSender::new(TransportType::Ble, ble_send_tx)));
        }

        // UDP transport
        let udp_transport = if config.enable_lan {
            match UdpTransport::new() {
                Ok(udp) => {
                    println!("  [Node] UDP bound to port {}", udp.get_port());
                    registry.register(Arc::new(udp.clone()));
                    Some(udp)
                }
                Err(e) => {
                    println!("  [Node] UDP bind failed: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // ── Create and start Swarm ──────────────────────────────
        let mut swarm = Swarm::new(identity.clone(), topology, registry);
        let event_tx = swarm.event_sender();

        // UDP listener → PacketReceived events
        if let Some(ref udp) = udp_transport {
            if let Err(e) = udp.start_listener(event_tx.clone()) {
                println!("  [Node] UDP listener failed: {}", e);
            }
        }

        // mDNS → PeerDiscovered/PeerLost events
        if config.enable_lan {
            if let Some(ref udp) = udp_transport {
                let rotating_id = identity.get_rotating_id();
                match MdnsService::new(rotating_id, udp.get_port()) {
                    Ok(mdns) => {
                        if let Err(e) = mdns.start(event_tx.clone()) {
                            println!("  [Node] mDNS failed: {}", e);
                        } else {
                            println!("  [Node] mDNS started");
                        }
                    }
                    Err(e) => println!("  [Node] mDNS init failed: {}", e),
                }
            }
        }

        // Spawn Swarm event loop in background
        let ble_event_tx = event_tx.clone();
        std::thread::spawn(move || {
            swarm.run().expect("Swarm crashed");
        });

        // ── Platform-specific BLE (blocks calling thread) ───────
        if config.enable_ble {
            run_ble_main_thread(ble_event_tx, ble_send_rx)?;
        } else {
            println!("  [Node] BLE disabled. Running LAN only.");
            loop {
                std::thread::park();
            }
        }

        Ok(())
    }
}

// ─── Platform-specific BLE startup ──────────────────────────────

#[cfg(target_os = "macos")]
fn run_ble_main_thread(
    event_tx: flume::Sender<crate::transport::TransportEvent>,
    send_rx: flume::Receiver<(String, Vec<u8>)>,
) -> Result<()> {
    use crate::transport::ble::macos::run_ble_runloop;
    run_ble_runloop(event_tx, send_rx)
}

#[cfg(target_os = "windows")]
fn run_ble_main_thread(
    event_tx: flume::Sender<crate::transport::TransportEvent>,
    send_rx: flume::Receiver<(String, Vec<u8>)>,
) -> Result<()> {
    use crate::transport::ble::windows::start_ble_service;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        if let Err(e) = start_ble_service(event_tx, send_rx).await {
            eprintln!("  [Node] Windows BLE error: {}", e);
        }
        // Keep runtime alive
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_ble_main_thread(
    _event_tx: flume::Sender<crate::transport::TransportEvent>,
    _send_rx: flume::Receiver<(String, Vec<u8>)>,
) -> Result<()> {
    println!("  [Node] BLE not supported on this platform.");
    loop {
        std::thread::park();
    }
}
