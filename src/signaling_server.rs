use std::sync::Arc;

use futures::{
    stream::{SplitSink, StreamExt},
    SinkExt,
};
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors, media_engine::MediaEngine, APIBuilder,
    },
    data::data_channel::DataChannel,
    data_channel::RTCDataChannel,
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration, policy::ice_transport_policy::RTCIceTransportPolicy,
        sdp::session_description::RTCSessionDescription, RTCPeerConnection,
    },
};

pub struct SignalingServer {
    tx: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    handle: tokio::task::JoinHandle<()>,
    pending_candidates: Arc<Mutex<Vec<String>>>,
    rtc_peer_conn: Arc<RTCPeerConnection>,
    channel: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
}

impl SignalingServer {
    pub async fn new(ws_url: &'static str) -> anyhow::Result<Self> {
        let (socket, _) = connect_async(ws_url).await?;
        let (tx, mut rx) = socket.split();
        let mut m = MediaEngine::default();
        m.register_default_codecs()?;
        let mut registery = Registry::new();
        registery = register_default_interceptors(registery, &mut m)?;
        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registery)
            .build();

        let rtc_config = RTCConfiguration {
            ice_servers: vec![
                RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                    ..Default::default()
                },
                RTCIceServer {
                    urls: vec!["turn:127.0.0.1:9696".to_owned()],
                    ..Default::default()
                },
            ],
            ice_transport_policy: RTCIceTransportPolicy::Relay,
            ..Default::default()
        };
        let rtc_peer_conn = api.new_peer_connection(rtc_config).await?;
        let tx = Arc::new(Mutex::new(tx));
        let tx_for_ice_cands = tx.clone();
        let pending_candidates = Arc::new(Mutex::new(Vec::<String>::with_capacity(10)));
        let rtc_peer_conn = Arc::new(rtc_peer_conn);
        {
            let pending_candidates = pending_candidates.clone();
            let conn_for_ice_cands = rtc_peer_conn.clone();
            rtc_peer_conn.on_ice_candidate(Box::new(move |x| {
                let tx_for_ice_cands = tx_for_ice_cands.clone();
                let conn = conn_for_ice_cands.clone();
                let pending_candidates = pending_candidates.clone();
                Box::pin(async move {
                    if let Some(candidate) = x {
                        if conn.remote_description().await.is_some() {
                            let candidate = candidate.to_json().unwrap().candidate;
                            let mut text = String::with_capacity(200);
                            text.push_str("ice:");
                            text.push_str(&candidate);
                            println!("sending ws {}", text);
                            let result = tx_for_ice_cands
                                .lock()
                                .await
                                .send(Message::Text(text))
                                .await;
                            if let Err(err) = result {
                                print!("Error sending Ice Candidate {err}\n");
                            } else {
                                print!("Sent ICE Candidate\n");
                            }
                        } else {
                            pending_candidates
                                .lock()
                                .await
                                .push(candidate.to_json().unwrap().candidate);
                        }
                    }
                })
            }));
        }
        let channel = Arc::new(Mutex::new(None));

        let channel_clone = channel.clone();

        let conn_for_incoming_messages = rtc_peer_conn.clone();
        let tx_for_incoming_messages = tx.clone();
        let pending_candidates_for_incoming_messages = pending_candidates.clone();
        let handle = tokio::spawn(async move {
            while let Some(Ok(message)) = rx.next().await {
                if let Some(s) = match message {
                    Message::Text(s) => Some(s),
                    Message::Binary(b) => String::from_utf8(b).ok(),
                    _ => {
                        eprint!("Received Invalid Type Message\n");
                        None
                    }
                } {
                    println!("Received Message {s}");
                    if s.starts_with("ice:") {
                        let conn = conn_for_incoming_messages.clone();
                        if conn.remote_description().await.is_none() {
                            continue;
                        }
                        let candidate = RTCIceCandidateInit {
                            candidate: String::from(&s[4..]),
                            ..Default::default()
                        };
                        if let Err(err) = conn.add_ice_candidate(candidate).await {
                            eprintln!("Error adding ICE Candidate {err}");
                        } else {
                            println!("Added Ice candidate");
                        }
                    } else if s.starts_with("sdp:") {
                        let sdp_str = &s[4..];
                        let mut tx = tx_for_incoming_messages.lock().await;
                        let conn = conn_for_incoming_messages.clone();
                        if conn.local_description().await.is_some() {
                            conn.set_remote_description(serde_json::from_str(sdp_str).unwrap())
                                .await
                                .unwrap();
                        } else {
                            let channel_clone = channel_clone.clone();
                            conn.on_data_channel(Box::new(move |channel| {
                                println!("Received data channel {}", channel.label());
                                channel.on_open(Box::new(|| {
                                    println!("data channel opened");
                                    Box::pin(async {})
                                }));
                                {
                                    let channel_clone = channel_clone.clone();
                                    channel.on_close(Box::new(move || {
                                        println!("Data channel closed");

                                        let channel_clone = channel_clone.clone();
                                        Box::pin(async move {
                                            *channel_clone.lock().await = None;
                                        })
                                    }));
                                }
                                channel.on_message(Box::new(|x| {
                                    let s = String::from_utf8(x.data.to_vec()).unwrap();
                                    println!("datachannel received {}", s);
                                    Box::pin(async {})
                                }));
                                {
                                    let channel_clone = channel_clone.clone();
                                    Box::pin(async move {
                                        *channel_clone.lock().await = Some(channel.clone());
                                    })
                                }
                            }));
                            conn.set_remote_description(serde_json::from_str(sdp_str).unwrap())
                                .await
                                .unwrap();
                            let answer = conn.create_answer(None).await.unwrap();
                            let answer_str = serde_json::to_string(&answer).unwrap();
                            let mut text = String::with_capacity(512);

                            text.push_str("sdp:");
                            text.push_str(&answer_str);

                            println!("sending ws {}", text);
                            tx.send(Message::Text(text)).await.unwrap();

                            conn.set_local_description(answer).await.unwrap();
                        }
                        let cands = pending_candidates_for_incoming_messages.lock().await;

                        for cand in &*cands {
                            let mut text = String::with_capacity(512);
                            text.push_str("ice:");
                            text.push_str(cand);

                            println!("sending ws {}", text);
                            tx.send(Message::Text(text)).await.unwrap();
                        }
                    }
                }
            }
        });

        Ok(Self {
            tx,
            handle,
            rtc_peer_conn,
            pending_candidates,
            channel,
        })
    }

    pub async fn send_message(&self, data: &str) -> anyhow::Result<()> {
        let channel = self.channel.lock().await;
        if let Some(channel) = &*channel {
            channel.send_text(data).await?;
        }
        Ok(())
    }

    pub fn stop_stream(self) {
        drop(self.handle);
    }

    pub async fn create_offer(&self) -> anyhow::Result<RTCSessionDescription> {
        let conn = &self.rtc_peer_conn;
        let offer = conn.create_offer(None).await?;
        conn.set_local_description(offer.clone()).await?;
        println!("Set Local description");
        Ok(offer)
    }

    pub async fn send_offer(&self) -> anyhow::Result<()> {
        let conn = &self.rtc_peer_conn;
        let channel = conn.create_data_channel("data", None).await?;
        *self.channel.lock().await = Some(channel.clone());
        channel.on_open(Box::new(|| {
            println!("Data Channel opened");
            Box::pin(async {})
        }));
        let self_channel = self.channel.clone();
        channel.on_close(Box::new(move || {
            println!("Data channel closed");
            let self_channel = self_channel.clone();
            Box::pin(async move {
                *self_channel.lock().await = None;
            })
        }));

        channel.on_message(Box::new(|x| {
            let s = String::from_utf8(x.data.to_vec()).unwrap();
            println!("datachannel received {}", s);
            Box::pin(async {})
        }));
        let offer = conn.create_offer(None).await?;
        let sdp_str = serde_json::to_string(&offer).unwrap();
        conn.set_local_description(offer).await.unwrap();
        let mut text = String::with_capacity(512);
        text.push_str("sdp:");
        text.push_str(&sdp_str);
        println!("sending ws {}", text);
        self.tx.lock().await.send(Message::Text(text)).await?;
        Ok(())
    }

    pub async fn create_answer(
        &self,
        offer: RTCSessionDescription,
    ) -> anyhow::Result<RTCSessionDescription> {
        let conn = &self.rtc_peer_conn;
        conn.set_remote_description(offer).await?;
        println!("Set remote description");
        let answer = conn.create_answer(None).await?;
        conn.set_local_description(answer.clone()).await?;
        println!("Set Local description");
        Ok(answer)
    }

    pub async fn accept_answer(&self, answer: RTCSessionDescription) -> Result<(), webrtc::Error> {
        let conn = &self.rtc_peer_conn;
        conn.set_remote_description(answer).await?;
        println!("Set remote description");
        Ok(())
    }
}
