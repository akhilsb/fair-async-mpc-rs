use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV4},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use config::Node;

use fnv::FnvHashMap;
use network::{
    plaintcp::{CancelHandler, TcpReceiver, TcpReliableSender},
    Acknowledgement,
};
use protocol::{LargeFieldSer, LargeField, AvssShare, gen_roots_of_unity, FIELD_DIV_2};
use signal_hook::{iterator::Signals, consts::{SIGINT, SIGTERM}};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, Receiver, Sender, channel},
    oneshot,
};
// use tokio_util::time::DelayQueue;
use types::{Replica, WrapperMsg, SyncMsg, SyncState};

use crypto::{aes_hash::HashState, hash::Hash};

use crate::{msg::ProtMsg, handlers::{sync_handler::SyncHandler, handler::Handler}, protocol::{RandSharings, MultState, VerificationState, rand_sharings::rand_mask::RandomOutputMaskStruct, online_phase::mix_circuit_state::MixCircuitState}};

pub struct Context {
    /// Networking context
    pub net_send: TcpReliableSender<Replica, WrapperMsg<ProtMsg>, Acknowledgement>,
    pub net_recv: UnboundedReceiver<WrapperMsg<ProtMsg>>,

    /// Data context
    pub num_nodes: usize,
    pub myid: usize,
    pub num_faults: usize,
    _byz: bool,

    /// Secret Key map
    pub sec_key_map: HashMap<Replica, Vec<u8>>,

    /// Hardware acceleration context
    pub hash_context: HashState,

    /// Cancel Handlers
    pub cancel_handlers: HashMap<u64, Vec<CancelHandler<Acknowledgement>>>,
    exit_rx: oneshot::Receiver<()>,

    pub inputs: Vec<LargeField>,
    pub input_acss_id_offset: usize,
    
    pub k_value: usize,
    pub log_k: usize,
    pub per_batch_maximum: usize,
    pub tot_batches: usize,

    pub total_sharings_for_coins: usize,

    // Maximum number of RBCs that can be initiated by a node. Keep this as an identifier for RBC service. 
    pub threshold: usize, 

    pub max_id: usize, 

    /// Input and output message queues for Reliable Broadcast
    pub acss_ab_send: Sender<(usize, Vec<LargeFieldSer>)>,
    pub acss_ab_out_recv: Receiver<(usize, Replica, Option<Vec<LargeFieldSer>>)>,

    pub avss_send: Sender<(bool, Option<Vec<LargeFieldSer>>, Option<(Replica, Replica, AvssShare)>)>,
    pub avss_out_recv: Receiver<(bool, Option<(Replica,AvssShare)>, Option<(Replica,Replica,AvssShare)>)>,

    pub sh2t_send: Sender<(usize, Vec<LargeFieldSer>)>,
    pub sh2t_out_recv: Receiver<(usize, Replica, Option<Vec<LargeFieldSer>>)>,

    pub acs_event_send: Sender<(usize,usize, Vec<Hash>)>,
    pub acs_out_recv: Receiver<(usize,Vec<usize>)>,

    pub ctrbc_event_send: Sender<Vec<u8>>,
    pub ctrbc_out_recv: Receiver<(usize, Replica, Vec<u8>)>,

    pub acs_2_event_send: Sender<(usize,usize, Vec<Hash>)>,
    pub acs_2_out_recv: Receiver<(usize,Vec<usize>)>,

    // Housekeeping processes for tracking metrics of the protocol
    pub sync_send: TcpReliableSender<Replica, SyncMsg, Acknowledgement>,
    pub sync_recv: UnboundedReceiver<SyncMsg>,

    /// State structures for keeping track of the state of the protocol
    // Preparation phase: Random sharings and 2t sharings of zero
    pub rand_sharings_state: RandSharings,
    // Multiplication state
    pub mult_state: MultState,
    // Verification state for multiplication triples
    pub verf_state: VerificationState,
    // Random masks for the output
    pub output_mask_state: RandomOutputMaskStruct,
    // Mix circuit state for mixing circuit implementation
    pub mix_circuit_state: MixCircuitState,

    pub field_div_2: LargeField,

    pub tmp_mult_state: HashMap<usize, (Vec<LargeField>,Vec<Vec<LargeField>>)>,

    /// Fast fourier transforms utility
    pub use_fft: bool,
    pub roots_of_unity: Vec<LargeField>,

    // Protocol parameters
    pub total_sharings: usize,
    pub max_depth: usize,
    pub output_mask_size: usize,

    pub preprocessing_mult_depth: usize,
    pub delinearization_depth: usize, 
    pub compression_factor: usize,
    pub multiplication_switch_threshold: usize,
}

impl Context {
    pub fn spawn(
        config: Node,
        per_batch: usize,
        mixing_batch_size: usize,
        compression_factor: usize,
        _byz: bool
    ) -> anyhow::Result<oneshot::Sender<()>> {
        // Add a separate configuration for RBC service. 

        let mut consensus_addrs: FnvHashMap<Replica, SocketAddr> = FnvHashMap::default();

        let mut acss_ab_config = config.clone();
        let mut sh2t_config = config.clone();
        let mut acs_config = config.clone();
        let mut ctrbc_config = config.clone();
        let mut acs_2_config = config.clone();

        let port_acss_ab: u16 = 0;
        let port_sh2t: u16 = 600;
        let port_acs: u16 = 1200;
        let port_ctrbc = 1800;
        let port_acs_2 = 1950;

        for (replica, address) in config.net_map.iter() {
            let address: SocketAddr = address.parse().expect("Unable to parse address");

            let acss_ab_address: SocketAddr = SocketAddr::new(address.ip(), address.port() + port_acss_ab);
            let sh2t_address: SocketAddr = SocketAddr::new(address.ip(), address.port() + port_sh2t);
            let acs_address: SocketAddr = SocketAddr::new(address.ip(), address.port() + port_acs);
            let ctrbc_address: SocketAddr = SocketAddr::new(address.ip(), address.port() + port_ctrbc);
            let acs_2_address: SocketAddr = SocketAddr::new(address.ip(), address.port() + port_acs_2);

            acss_ab_config.net_map.insert(*replica, acss_ab_address.to_string());
            sh2t_config.net_map.insert(*replica, sh2t_address.to_string());
            acs_config.net_map.insert(*replica, acs_address.to_string());
            ctrbc_config.net_map.insert(*replica, ctrbc_address.to_string());
            acs_2_config.net_map.insert(*replica, acs_2_address.to_string());

            consensus_addrs.insert(*replica, SocketAddr::from(address.clone()));
            
        }
        log::info!("Consensus addresses: {:?}", consensus_addrs);
        let my_port = consensus_addrs.get(&config.id).unwrap();
        let my_address = to_socket_address("0.0.0.0", my_port.port());
        
        let mut syncer_map: FnvHashMap<Replica, SocketAddr> = FnvHashMap::default();
        syncer_map.insert(0, config.client_addr);

        let syncer_listen_port = config.client_port;
        let syncer_l_address = to_socket_address("0.0.0.0", syncer_listen_port);

        // The server must listen to the client's messages on some port that is not being used to listen to other servers
        let (tx_net_to_client, rx_net_from_client) = unbounded_channel();
        TcpReceiver::<Acknowledgement, SyncMsg, _>::spawn(
            syncer_l_address,
            SyncHandler::new(tx_net_to_client),
        );

        let sync_net =
            TcpReliableSender::<Replica, SyncMsg, Acknowledgement>::with_peers(syncer_map);

        // Setup networking
        let (tx_net_to_consensus, rx_net_to_consensus) = unbounded_channel();
        TcpReceiver::<Acknowledgement, WrapperMsg<ProtMsg>, _>::spawn(
            my_address,
            Handler::new(tx_net_to_consensus),
        );

        let consensus_net = TcpReliableSender::<Replica, WrapperMsg<ProtMsg>, Acknowledgement>::with_peers(
            consensus_addrs.clone(),
        );

        let (exit_tx, exit_rx) = oneshot::channel();

        // Keyed AES ciphers
        let key0 = [5u8; 16];
        let key1 = [29u8; 16];
        let key2 = [23u8; 16];
        let hashstate = HashState::new(key0, key1, key2);

        let (acss_ab_send, acss_ab_recv) = channel(10000);
        let (acss_ab_out_send, acss_ab_out_recv) = channel(10000);

        let (avss_send, avss_recv) = channel(500000);
        let (avss_out_send, avss_out_recv) = channel(500000);

        let (sh2t_send, sh2t_recv) = channel(10000);
        let (sh2t_out_send, sh2t_out_recv) = channel(10000);

        let (acs_inp_send, acs_inp_recv) = channel(10000);
        let (acs_out_send, acs_out_recv) = channel(10000);

        let (ctrbc_send, ctrbc_recv) = channel(10000);
        let (ctrbc_out_send, ctrbc_out_recv) = channel(10000);

        let (acs_2_send, acs_2_recv) = channel(10000);
        let (acs_2_out_send, acs_2_out_recv) = channel(10000);

        let threshold:usize = 10000;
        let rbc_start_id = threshold*config.id;

        let use_fft = false;

        let k = mixing_batch_size as u64;
        let log_k = (u64::BITS - k.leading_zeros() -1) as usize;
        let k = k as usize;

        let sqrt_power = LargeField::from_hex(FIELD_DIV_2).unwrap();
        
        let tot_sharings = (((k)*log_k*log_k)/(config.num_faults+1))+20;
        let num_batches = (tot_sharings.max(per_batch))/per_batch;
        // Ensure this is a power of 2. 

        let high_threshold = 2*config.num_faults+1;
        let inputs_per_party = (k / high_threshold) + 1;
        
        let inputs: Vec<LargeField> = (0..inputs_per_party).into_iter().map(|x| LargeField::from((config.id*inputs_per_party + x) as u64)).collect();
        log::info!("Generating {} random sharings and proposing {} sharings over {} batches for mixing {} inputs", 8*(k/2)*log_k*log_k, tot_sharings, num_batches, k);
        tokio::spawn(async move {
            let mut c = Context {
                net_send: consensus_net,
                net_recv: rx_net_to_consensus,
                
                num_nodes: config.num_nodes,
                sec_key_map: HashMap::default(),
                hash_context: hashstate,
                myid: config.id,
                _byz: _byz,
                num_faults: config.num_faults,
                cancel_handlers: HashMap::default(),
                exit_rx: exit_rx,
                
                threshold: 10000,

                max_id: rbc_start_id,

                inputs: inputs.clone(),
                input_acss_id_offset: 500,

                k_value: k,
                log_k: log_k,
                per_batch_maximum: per_batch,
                tot_batches: num_batches,

                total_sharings_for_coins: 10*config.num_nodes,
                
                acss_ab_send: acss_ab_send,
                acss_ab_out_recv: acss_ab_out_recv,

                avss_send: avss_send,
                avss_out_recv: avss_out_recv,

                sh2t_send: sh2t_send,
                sh2t_out_recv: sh2t_out_recv,

                acs_event_send: acs_inp_send,
                acs_out_recv: acs_out_recv,

                ctrbc_event_send: ctrbc_send,
                ctrbc_out_recv: ctrbc_out_recv,

                acs_2_event_send: acs_2_send,
                acs_2_out_recv: acs_2_out_recv,

                // Syncer related stuff
                sync_send: sync_net,
                sync_recv: rx_net_from_client,

                rand_sharings_state: RandSharings::new(),
                mult_state: MultState::new(),
                verf_state: VerificationState::new(),
                output_mask_state: RandomOutputMaskStruct::new(),
                mix_circuit_state: MixCircuitState::new(),
                tmp_mult_state: HashMap::default(),

                field_div_2: sqrt_power,

                use_fft: use_fft,
                roots_of_unity: gen_roots_of_unity(config.num_nodes),

                total_sharings: tot_sharings,
                max_depth: log_k*log_k,
                output_mask_size: 2*k/(config.num_faults+1),

                preprocessing_mult_depth: 0,
                delinearization_depth: 5000, 
                compression_factor: compression_factor,
                multiplication_switch_threshold: 0
            };

            // Populate secret keys from config
            for (id, sk_data) in config.sk_map.clone() {
                c.sec_key_map.insert(id, sk_data.clone());
            }

            // Run the consensus context
            if let Err(e) = c.run().await {
                log::error!("Consensus error: {}", e);
            }
        });

        let status = acss_ab::Context::spawn(
            acss_ab_config,
            acss_ab_recv,
            acss_ab_out_send,
            avss_recv,
            avss_out_send,
            use_fft,
            false,
        );
        if status.is_err() {
            log::error!("Error spawning acss_ab because of {:?}", status.err().unwrap());
        }

        let status_sh2t = sh2t::Context::spawn(
            sh2t_config,
            sh2t_recv,
            sh2t_out_send,
            use_fft,
            false,
        );

        if status_sh2t.is_err() {
            log::error!("Error spawning status_sh2t because of {:?}", status_sh2t.err().unwrap());
        }

        let ctrbc_status = ctrbc::Context::spawn(
            ctrbc_config,
            ctrbc_recv,
            ctrbc_out_send,
            false,
        );

        if ctrbc_status.is_err() {
            log::error!("Error spawning CTRBC because of {:?}", ctrbc_status.err().unwrap());
        }

        let status_acs = acs::Context::spawn(
            acs_config,
            acs_inp_recv,
            acs_out_send,
            false,
        );
        
        if status_acs.is_err() {
            log::error!("Error spawning acs because of {:?}", status_acs.err().unwrap());
        }
        
        let status_acs_2 = acs::Context::spawn(
            acs_2_config,
            acs_2_recv,
            acs_2_out_send,
            false,
        );
        
        if status_acs_2.is_err() {
            log::error!("Error spawning acs 2 because of {:?}", status_acs_2.err().unwrap());
        }

        let mut signals = Signals::new(&[SIGINT, SIGTERM])?;
        signals.forever().next();
        log::error!("Received termination signal");

        Ok(exit_tx)
    }

    pub async fn broadcast(&mut self, protmsg: ProtMsg) {
        let sec_key_map = self.sec_key_map.clone();
        for (replica, sec_key) in sec_key_map.into_iter() {
            let wrapper_msg = WrapperMsg::new(protmsg.clone(), self.myid, &sec_key.as_slice());
            let cancel_handler: CancelHandler<Acknowledgement> = self.net_send.send(replica, wrapper_msg).await;
            self.add_cancel_handler(cancel_handler);
        }
    }

    pub fn add_cancel_handler(&mut self, canc: CancelHandler<Acknowledgement>) {
        self.cancel_handlers.entry(0).or_default().push(canc);
    }

    pub async fn send(&mut self, replica: Replica, wrapper_msg: WrapperMsg<ProtMsg>) {
        let cancel_handler: CancelHandler<Acknowledgement> =
            self.net_send.send(replica, wrapper_msg).await;
        self.add_cancel_handler(cancel_handler);
    }

    pub async fn run(&mut self) -> Result<()> {
        // The process starts listening to messages in this process.
        // First, the node sends an alive message
        let cancel_handler = self
            .sync_send
            .send(
                0,
                SyncMsg {
                    sender: self.myid,
                    state: SyncState::ALIVE,
                    value: "".to_string().into_bytes(),
                },
            )
            .await;
        self.add_cancel_handler(cancel_handler);
        loop {
            tokio::select! {
                // Receive exit handlers
                exit_val = &mut self.exit_rx => {
                    exit_val.map_err(anyhow::Error::new)?;
                    log::info!("Termination signal received by the server. Exiting.");
                    break
                },
                msg = self.net_recv.recv() => {
                    // Received messages are processed here
                    log::trace!("Got a consensus message from the network: {:?}", msg);
                    let msg = msg.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    self.process_msg(msg).await;
                },
                acss_msg = self.acss_ab_out_recv.recv() => {
                    let acss_msg_unwrap = acss_msg.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::info!("Received shares from ACSS module for instance {} from party {}",acss_msg_unwrap.0,acss_msg_unwrap.1);
                    // Check if the option is none. It means some party aborted
                    if acss_msg_unwrap.0 >= self.input_acss_id_offset{
                        self.handle_input_acss_termination(acss_msg_unwrap.0, acss_msg_unwrap.1, acss_msg_unwrap.2).await;
                    }
                    else{
                        self.handle_acss_term_msg(acss_msg_unwrap.0, acss_msg_unwrap.1, acss_msg_unwrap.2).await;
                    }
                },
                sh2t_msg = self.sh2t_out_recv.recv() => {
                    let sh2t_msg_unwrap = sh2t_msg.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::info!("Received shares from SH2T module for instance {} from party {}",sh2t_msg_unwrap.0,sh2t_msg_unwrap.1);
                    // Check if the option is none. It means some party aborted
                    self.handle_sh2t_term_msg(sh2t_msg_unwrap.0, sh2t_msg_unwrap.1, sh2t_msg_unwrap.2).await;
                },
                acs_output = self.acs_out_recv.recv() =>{
                    let acs_output = acs_output.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::debug!("Received message from RBC channel {:?}", acs_output);
                    self.handle_acs_output(acs_output.1).await;
                },
                ctrbc_output = self.ctrbc_out_recv.recv() =>{
                    let ctrbc_output = ctrbc_output.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::debug!("Received message from CTRBC channel {:?}", ctrbc_output);
                    self.handle_output_delivery_ctrbc(ctrbc_output.0, ctrbc_output.1, ctrbc_output.2).await;
                },
                acs_2_output = self.acs_2_out_recv.recv() =>{
                    let acs_output = acs_2_output.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::debug!("Received message from RBC channel {:?}", acs_output);
                    self.handle_prot_end_ba_output(acs_output.1).await;
                },
                avss_output = self.avss_out_recv.recv() =>{
                    let avss_output = avss_output.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::debug!("Received message from AVSS channel {:?}", avss_output);
                    if avss_output.0 {
                        // This is a sharing output
                        let (origin, avss_share) = avss_output.1.unwrap();
                        self.handle_avss_share_output(origin, avss_share).await;
                    }
                    else{
                        // This is a reconstruction output
                        let (origin, share_sender, avss_share) = avss_output.2.unwrap();
                        self.handle_avss_share_oracle_output(origin, share_sender, avss_share).await;
                    }
                },
                sync_msg = self.sync_recv.recv() =>{
                    let sync_msg = sync_msg.ok_or_else(||
                        anyhow!("Networking layer has closed")
                    )?;
                    log::info!("Received sync message from party {} at time: {:?}", sync_msg.sender, SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis());
                    match sync_msg.state {
                        SyncState::START =>{
                            // Code used for internal purposes
                            log::info!("Consensus Start time: {:?}", SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis());
                            // Start your protocol from here
                            // Write a function to broadcast a message. We demonstrate an example with a PING function
                            // Dealer sends message to everybody. <M, init>
                            self.init_rand_sh().await;
                        },
                        SyncState::STOP =>{
                            // Code used for internal purposes
                            log::info!("Consensus Stop time: {:?}", SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_millis());
                            log::info!("Termination signal received by the server. Exiting.");
                            break
                        },
                        _=>{}
                    }
                }
            };
        }
        Ok(())
    }
}

pub fn to_socket_address(ip_str: &str, port: u16) -> SocketAddr {
    let addr = SocketAddrV4::new(ip_str.parse().unwrap(), port);
    addr.into()
}
