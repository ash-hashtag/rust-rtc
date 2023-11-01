use std::sync::Arc;

use futures::{
    stream::{SplitSink, StreamExt},
    SinkExt,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors, media_engine::MediaEngine, APIBuilder,
    },
    data::data_channel::DataChannel,
    data_channel::RTCDataChannel,
    ice_transport::{
        ice_candidate::RTCIceCandidateInit, ice_credential_type::RTCIceCredentialType,
        ice_server::RTCIceServer,
    },
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration, policy::ice_transport_policy::RTCIceTransportPolicy,
        sdp::session_description::RTCSessionDescription, RTCPeerConnection,
    },
    stun::textattrs::Realm,
};

// #[derive(Deserialize, Serialize)]
// struct SDPSignal {
//     sdp: RTCSessionDescription,
// }
#[derive(Deserialize, Serialize)]
struct SDPOfferSignal {
    offer: RTCSessionDescription,
}
#[derive(Deserialize, Serialize)]
struct SDPAnswerSignal {
    answer: RTCSessionDescription,
}

#[derive(Deserialize, Serialize)]
struct ICECandSignal {
    ice: RTCIceCandidateInit,
}

pub struct SignalingServer {
    tx: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    handle: tokio::task::JoinHandle<()>,
    pending_candidates: Arc<Mutex<Vec<RTCIceCandidateInit>>>,
    rtc_peer_conn: Arc<RTCPeerConnection>,
    channel: Arc<Mutex<Option<Arc<RTCDataChannel>>>>,
}

fn timestamp_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
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
        let mut args = std::env::args().skip(1);
        let username = args.next().expect("username not found");
        let password = args.next().expect("password not found");
        let use_relay = args.next().unwrap_or(String::from("no-relay"));
        let use_relay = use_relay == "relay";
        println!("user: {username}, pass: {password}");

        let rtc_config = if use_relay {
            println!("using relay");
            RTCConfiguration {
                ice_servers: vec![
                    RTCIceServer {
                        urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                        ..Default::default()
                    },
                    RTCIceServer {
                        urls: vec!["turn:35.200.208.109:8080".to_owned()],
                        username,
                        credential: password,
                        credential_type: RTCIceCredentialType::Password,
                    },
                ],
                ice_transport_policy: RTCIceTransportPolicy::Relay,
                ..Default::default()
            }
        } else {
            println!("not using relay");
            RTCConfiguration {
                ice_servers: vec![RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                    ..Default::default()
                }],
                ..Default::default()
            }
        };
        let rtc_peer_conn = api.new_peer_connection(rtc_config).await?;
        let tx = Arc::new(Mutex::new(tx));
        let tx_for_ice_cands = tx.clone();
        let pending_candidates =
            Arc::new(Mutex::new(Vec::<RTCIceCandidateInit>::with_capacity(10)));
        let rtc_peer_conn = Arc::new(rtc_peer_conn);

        {
            rtc_peer_conn.on_ice_connection_state_change(Box::new(|state| {
                println!("{} ICE CONNECTION STATE {}", timestamp_now(), state);
                Box::pin(async {})
            }));

            rtc_peer_conn.on_ice_gathering_state_change(Box::new(|state| {
                println!("{} ICE GATHERING STATE {}", timestamp_now(), state);
                Box::pin(async {})
            }));
        }
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
                            let candidate = candidate.to_json().unwrap();
                            let text =
                                serde_json::to_string(&ICECandSignal { ice: candidate }).unwrap();
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
                                .push(candidate.to_json().unwrap());
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
                    if let Ok(ice) = serde_json::from_str::<ICECandSignal>(&s) {
                        let conn = conn_for_incoming_messages.clone();
                        if conn.remote_description().await.is_none() {
                            continue;
                        }
                        let candidate = ice.ice;
                        if let Err(err) = conn.add_ice_candidate(candidate).await {
                            eprintln!("Error adding ICE Candidate {err}");
                        } else {
                            println!("Added Ice candidate");
                        }
                    } else if let Ok(sdp) = serde_json::from_str::<SDPAnswerSignal>(&s) {
                        let mut tx = tx_for_incoming_messages.lock().await;
                        let conn = conn_for_incoming_messages.clone();
                        if conn.local_description().await.is_some() {
                            conn.set_remote_description(sdp.answer).await.unwrap();
                        }
                        let cands = pending_candidates_for_incoming_messages.lock().await;
                        for cand in &*cands {
                            let text = serde_json::to_string(&ICECandSignal { ice: cand.clone() })
                                .unwrap();

                            println!("sending ws {}", text);
                            tx.send(Message::Text(text)).await.unwrap();
                        }
                        // else {
                        //     let channel_clone = channel_clone.clone();
                        //     conn.on_data_channel(Box::new(move |channel| {
                        //         println!(
                        //             "{} received data channel {}",
                        //             timestamp_now(),
                        //             channel.label()
                        //         );
                        //         channel.on_open(Box::new(|| {
                        //             println!("{} data channel opened", timestamp_now());
                        //             Box::pin(async {})
                        //         }));
                        //         {
                        //             let channel_clone = channel_clone.clone();
                        //             channel.on_close(Box::new(move || {
                        //                 println!("{} data channel closed", timestamp_now());

                        //                 let channel_clone = channel_clone.clone();
                        //                 Box::pin(async move {
                        //                     *channel_clone.lock().await = None;
                        //                 })
                        //             }));
                        //         }
                        //         channel.on_message(Box::new(|x| {
                        //             let s = String::from_utf8(x.data.to_vec()).unwrap();
                        //             println!("datachannel received {}", s);
                        //             Box::pin(async {})
                        //         }));
                        //         {
                        //             let channel_clone = channel_clone.clone();
                        //             Box::pin(async move {
                        //                 *channel_clone.lock().await = Some(channel.clone());
                        //             })
                        //         }
                        //     }));
                        //     conn.set_remote_description(sdp.sdp).await.unwrap();
                        //     let answer = conn.create_answer(None).await.unwrap();
                        //     let text = serde_json::to_string(&SDPSignal {
                        //         sdp: answer.clone(),
                        //     })
                        //     .unwrap();

                        //     println!("sending ws {}", text);
                        //     tx.send(Message::Text(text)).await.unwrap();

                        //     conn.set_local_description(answer).await.unwrap();
                        // }
                        // let cands = pending_candidates_for_incoming_messages.lock().await;

                        // for cand in &*cands {
                        //     let text = serde_json::to_string(&ICECandSignal { ice: cand.clone() })
                        //         .unwrap();

                        //     println!("sending ws {}", text);
                        //     tx.send(Message::Text(text)).await.unwrap();
                        // }
                    } else if let Ok(sdp) = serde_json::from_str::<SDPOfferSignal>(&s) {
                        let channel_clone = channel_clone.clone();
                        let mut tx = tx_for_incoming_messages.lock().await;
                        let conn = conn_for_incoming_messages.clone();
                        conn.on_data_channel(Box::new(move |channel| {
                            println!(
                                "{} received DATA CHANNEL {}",
                                timestamp_now(),
                                channel.label()
                            );
                            channel.on_open(Box::new(|| {
                                println!("{} DATA CHANNEL opened", timestamp_now());
                                Box::pin(async {})
                            }));
                            {
                                let channel_clone = channel_clone.clone();
                                channel.on_close(Box::new(move || {
                                    println!("{} DATA CHANNEL closed", timestamp_now());

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
                        conn.set_remote_description(sdp.offer).await.unwrap();
                        let answer = conn.create_answer(None).await.unwrap();
                        let text = serde_json::to_string(&SDPAnswerSignal {
                            answer: answer.clone(),
                        })
                        .unwrap();

                        println!("sending ws {}", text);
                        tx.send(Message::Text(text)).await.unwrap();

                        conn.set_local_description(answer).await.unwrap();

                        let cands = pending_candidates_for_incoming_messages.lock().await;

                        for cand in &*cands {
                            let text = serde_json::to_string(&ICECandSignal { ice: cand.clone() })
                                .unwrap();

                            println!("sending ws {}", text);
                            tx.send(Message::Text(text)).await.unwrap();
                        }
                    }
                    //         if s.starts_with("ice:") {
                    //             let conn = conn_for_incoming_messages.clone();
                    //             if conn.remote_description().await.is_none() {
                    //                 continue;
                    //             }
                    //             let candidate = RTCIceCandidateInit {
                    //                 candidate: String::from(&s[4..]),
                    //                 ..Default::default()
                    //             };
                    //             if let Err(err) = conn.add_ice_candidate(candidate).await {
                    //                 eprintln!("Error adding ICE Candidate {err}");
                    //             } else {
                    //                 println!("Added Ice candidate");
                    //             }
                    //         } else if s.starts_with("sdp:") {
                    //             let sdp_str = &s[4..];
                    //             let mut tx = tx_for_incoming_messages.lock().await;
                    //             let conn = conn_for_incoming_messages.clone();
                    //             if conn.local_description().await.is_some() {
                    //                 conn.set_remote_description(serde_json::from_str(sdp_str).unwrap())
                    //                     .await
                    //                     .unwrap();
                    //             } else {
                    //                 let channel_clone = channel_clone.clone();
                    //                 conn.on_data_channel(Box::new(move |channel| {
                    //                     println!(
                    //                         "{} received data channel {}",
                    //                         timestamp_now(),
                    //                         channel.label()
                    //                     );
                    //                     channel.on_open(Box::new(|| {
                    //                         println!("{} data channel opened", timestamp_now());
                    //                         Box::pin(async {})
                    //                     }));
                    //                     {
                    //                         let channel_clone = channel_clone.clone();
                    //                         channel.on_close(Box::new(move || {
                    //                             println!("{} data channel closed", timestamp_now());

                    //                             let channel_clone = channel_clone.clone();
                    //                             Box::pin(async move {
                    //                                 *channel_clone.lock().await = None;
                    //                             })
                    //                         }));
                    //                     }
                    //                     channel.on_message(Box::new(|x| {
                    //                         let s = String::from_utf8(x.data.to_vec()).unwrap();
                    //                         println!("datachannel received {}", s);
                    //                         Box::pin(async {})
                    //                     }));
                    //                     {
                    //                         let channel_clone = channel_clone.clone();
                    //                         Box::pin(async move {
                    //                             *channel_clone.lock().await = Some(channel.clone());
                    //                         })
                    //                     }
                    //                 }));
                    //                 conn.set_remote_description(serde_json::from_str(sdp_str).unwrap())
                    //                     .await
                    //                     .unwrap();
                    //                 let answer = conn.create_answer(None).await.unwrap();
                    //                 let text = serde_json::to_string(&SDPSignal {
                    //                     sdp: answer.clone(),
                    //                 })
                    //                 .unwrap();

                    //                 println!("sending ws {}", text);
                    //                 tx.send(Message::Text(text)).await.unwrap();

                    //                 conn.set_local_description(answer).await.unwrap();
                    //             }
                    //             let cands = pending_candidates_for_incoming_messages.lock().await;

                    //             for cand in &*cands {
                    //                 let text = serde_json::to_string(&ICECandSignal { ice: cand.clone() })
                    //                     .unwrap();

                    //                 println!("sending ws {}", text);
                    //                 tx.send(Message::Text(text)).await.unwrap();
                    //             }
                    //         }
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

    pub async fn stop_stream(self) {
        self.rtc_peer_conn.close().await;
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
            println!("{} data Channel opened", timestamp_now());
            Box::pin(async {})
        }));
        let self_channel = self.channel.clone();
        channel.on_close(Box::new(move || {
            println!("{} data channel closed", timestamp_now());
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
        conn.set_local_description(offer.clone()).await.unwrap();
        let text = serde_json::to_string(&SDPOfferSignal { offer }).unwrap();
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
