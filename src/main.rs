use clap::Parser;
use env_logger::Builder;
use log::{debug, error, info};
use pnet::datalink::{self, Channel::Ethernet, Config};
use pnet::packet::ethernet::{EthernetPacket, MutableEthernetPacket};
use pnet::packet::ipv4::Ipv4Packet;
use pnet::packet::udp::UdpPacket;
use pnet::packet::Packet;
use std::env;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
/// Command-line arguments for the program
#[derive(Parser)]
#[command(name = "Network Packet Forwarder")]
#[command(about = "Packet forwarder between two network interfaces.")]
struct Args {
    /// Name of the external network interface
    #[arg(long)]
    external_iface: String,

    /// Name of the internal network interface
    #[arg(long)]
    internal_iface: String,
}
#[tokio::main]
async fn main() {
    env::set_var("RUST_BACKTRACE", "1");
    // Initialize env_logger
    // You can set the level in code here
    Builder::new()
        .filter_level(log::LevelFilter::Debug) // Set to Debug level in code
        .init();
    // Parse command-line arguments using clap
    let args = Args::parse();
    // Get the network interfaces inside the async block to ensure it lives long enough
    let interfaces = datalink::interfaces();

    // Find the external interface
    let external_iface = interfaces
        .iter()
        .find(|iface| iface.name == args.external_iface)
        .expect("No matching external interface found")
        .clone(); // Clone the interface to avoid borrowing issues

    // Find the internal interface
    let internal_iface = interfaces
        .iter()
        .find(|iface| iface.name == args.internal_iface)
        .expect("No matching internal interface found")
        .clone(); // Clone the interface to avoid borrowing issues
    info!(
        "Using interfaces: {},ip:{:?} and {}, ip:{:?}",
        external_iface.name, external_iface.ips, internal_iface.name, internal_iface.ips
    );

    // Create channels for both interfaces
    let config = Config::default();
    let (mut tx1, mut rx1) = match datalink::channel(&external_iface, config.clone()) {
        Ok(Ethernet(tx, rx)) => (tx, rx),
        Ok(_) => panic!("Unhandled channel type"),
        Err(e) => panic!(
            "Failed to create datalink channel for {}: {}",
            external_iface.name, e
        ),
    };
    let (mut tx2, mut rx2) = match datalink::channel(&internal_iface, config) {
        Ok(Ethernet(tx, rx)) => (tx, rx),
        Ok(_) => panic!("Unhandled channel type"),
        Err(e) => panic!(
            "Failed to create datalink channel for {}: {}",
            internal_iface.name, e
        ),
    };

    // Wrap `tx1` and `tx2` in Arc<Mutex<>> for thread-safe access
    let tx1 = Arc::new(Mutex::new(tx1));
    let tx2 = Arc::new(Mutex::new(tx2));
    // Create a CancellationToken
    let token = CancellationToken::new();

    let token1 = token.clone();
    let token2 = token.clone();

    // Spawn a blocking thread for packet processing (capture loop) on eth0
    let internal_task = tokio::spawn(async move {
        info!("Starting packet capture on {}...", internal_iface.name);
        loop {
            tokio::select! {
                // Step 3: Use the cancellation token
                _ = token1.cancelled() => {
                    // Token was cancelled, clean up and exit task
                    info!("Cancellation token triggered, shutting down capture on {}...",internal_iface.name);
                    break;
                }
                 // The loop to receive packets and forward them to eth1
            _ = async {
                match rx1.next() {
                    Ok(frame) => {
                        let frame_data = frame.to_vec();
                        debug!("Received frame on eth0: {:?}", frame_data);

                        // Forward packet to eth1
                        let tx_clone = Arc::clone(&tx2);

                        process_packet(tx_clone, &frame_data).await;
                    }
                    Err(e) => error!("Error receiving packet on eth0: {}", e),
                }
            }=> {}
            }
        }
        info!("Task for {} is cleaning up", internal_iface.name);
    });

    // Spawn another blocking thread for packet processing (capture loop) on eth1
    let external_task = tokio::spawn(async move {
        info!("Starting packet capture on {}...", external_iface.name);
        loop {
            tokio::select! {
                // Step 3: Use the cancellation token
                _ = token2.cancelled() => {
                    // Token was cancelled, clean up and exit task
                    info!("Cancellation token triggered, shutting down capture on {}...",external_iface.name);
                    break;
                }
                 // The loop to receive packets and forward them to eth1
            _ = async {
                match rx2.next() {
                    Ok(frame) => {
                        let frame_data = frame.to_vec();
                        debug!("Received frame on eth1: {:?}", frame_data);

                        // Forward packet to eth0
                        let tx_clone = Arc::clone(&tx1);

                        process_packet(tx_clone, &frame_data).await;
                    }
                    Err(e) => error!("Error receiving packet on eth1: {}", e),
                }
            }=> {}
            }
        }
        info!("Task for {} is cleaning up", external_iface.name);
    });

    // Gracefully handle shutdown (e.g., on SIGINT)
    let shutdown = signal::ctrl_c().await;
    if let Err(e) = shutdown {
        error!("Error while waiting for shutdown signal: {}", e);
    }
    info!("Shutting down gracefully...");

    // Send a cancellation signal
    token.cancel();

    // Wait for the tasks to finish
    let _ = tokio::join!(external_task, internal_task);
}

// Async function to forward the packet to the destination interface
async fn process_packet(tx: Arc<Mutex<Box<dyn pnet::datalink::DataLinkSender>>>, packet: &Vec<u8>) {
    let mut tx = tx.lock().await; // Acquire lock asynchronously

    if !should_forward(&packet).await {
        debug!("packet dropped");
    } else {
        match tx.send_to(packet, None) {
            Some(Ok(_)) => {
                debug!("Forwarded packet: {:?}", packet);
            }
            Some(Err(e)) => {
                error!("Error sending packet: {}", e);
            }
            None => error!("Error: Send failed, no destination address."),
        }
    }
}

async fn should_forward(packet: &Vec<u8>) -> bool {
    if let Some(eth_packet) = EthernetPacket::new(&packet) {
        debug!("Received packet: {:?}", eth_packet);

        // Filter only IPv4 packets (EtherType 0x0800)
        if eth_packet.get_ethertype().0 == 0x0800 {
            if let Some(ip_packet) = Ipv4Packet::new(eth_packet.payload()) {
                // Check if the protocol is UDP (protocol 17 for IPv4)
                if ip_packet.get_next_level_protocol()
                    == pnet::packet::ip::IpNextHeaderProtocols::Udp
                {
                    if let Some(udp_packet) = UdpPacket::new(ip_packet.payload()) {
                        // Check if the UDP packet is using port 1900 (SSDP default port)
                        if udp_packet.get_destination() == 1900 || udp_packet.get_source() == 1900 {
                            debug!("SSDP packet detected");
                            return true;
                        } else {
                            info!("Non-SSDP UDP packet dropped");
                        }
                    }
                }
            }
        }
        info!("Non-IPv4 or non-UDP packet dropped");
    }

    false
}
