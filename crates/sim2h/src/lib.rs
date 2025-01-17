extern crate env_logger;
//#[macro_use]
extern crate log;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate lazy_static;

pub mod cache;
pub mod connection_state;
pub mod crypto;
pub mod error;
use lib3h_protocol::types::AgentPubKey;
mod message_log;
pub mod websocket;
pub mod wire_message;

pub use crate::message_log::MESSAGE_LOGGER;
use crate::{crypto::*, error::*};
use cache::*;
use connection_state::*;
use lib3h_protocol::{
    data_types::{EntryData, FetchEntryData, GetListData, Opaque, SpaceData, StoreEntryAspectData},
    protocol::*,
    types::SpaceHash,
    uri::Lib3hUri,
};

use websocket::streams::{StreamEvent, StreamManager};
pub use wire_message::{WireError, WireMessage};

use log::*;
use parking_lot::RwLock;
use rand::Rng;
use std::{collections::HashMap, convert::TryFrom};

pub struct Sim2h {
    pub bound_uri: Option<Lib3hUri>,
    connection_states: RwLock<HashMap<Lib3hUri, ConnectionState>>,
    spaces: HashMap<SpaceHash, RwLock<Space>>,
    stream_manager: StreamManager<std::net::TcpStream>,
    num_ticks: u32,
}

impl Sim2h {
    pub fn new(stream_manager: StreamManager<std::net::TcpStream>, bind_spec: Lib3hUri) -> Self {
        let mut sim2h = Sim2h {
            bound_uri: None,
            connection_states: RwLock::new(HashMap::new()),
            spaces: HashMap::new(),
            stream_manager,
            num_ticks: 0,
        };

        debug!("Trying to bind to {}...", bind_spec);
        let url: url::Url = bind_spec.clone().into();
        sim2h.bound_uri = Some(
            sim2h
                .stream_manager
                .bind(&url)
                .unwrap_or_else(|e| panic!("Error binding to url {}: {:?}", bind_spec, e))
                .into(),
        );

        sim2h
    }

    fn request_authoring_list(
        &mut self,
        uri: Lib3hUri,
        space_address: SpaceHash,
        provider_agent_id: AgentId,
    ) {
        let wire_message =
            WireMessage::Lib3hToClient(Lib3hToClient::HandleGetAuthoringEntryList(GetListData {
                request_id: "".into(),
                space_address,
                provider_agent_id: provider_agent_id.clone(),
            }));
        self.send(provider_agent_id, uri, &wire_message);
    }

    fn request_gossiping_list(
        &mut self,
        uri: Lib3hUri,
        space_address: SpaceHash,
        provider_agent_id: AgentId,
    ) {
        let wire_message =
            WireMessage::Lib3hToClient(Lib3hToClient::HandleGetGossipingEntryList(GetListData {
                request_id: "".into(),
                space_address,
                provider_agent_id: provider_agent_id.clone(),
            }));
        self.send(provider_agent_id, uri, &wire_message);
    }

    // adds an agent to a space
    fn join(&mut self, uri: &Lib3hUri, data: &SpaceData) -> Sim2hResult<()> {
        trace!("join entered");
        let result =
            if let Some(ConnectionState::Limbo(pending_messages)) = self.get_connection(uri) {
                let _ = self.connection_states.write().insert(
                    uri.clone(),
                    ConnectionState::Joined(data.space_address.clone(), data.agent_id.clone()),
                );
                if !self.spaces.contains_key(&data.space_address) {
                    self.spaces
                        .insert(data.space_address.clone(), RwLock::new(Space::new()));
                    info!(
                        "\n\n+++++++++++++++\nNew Space: {}\n+++++++++++++++\n",
                        data.space_address
                    );
                }
                self.spaces
                    .get(&data.space_address)
                    .unwrap()
                    .write()
                    .join_agent(data.agent_id.clone(), uri.clone());
                info!(
                    "Agent {:?} joined space {:?}",
                    data.agent_id, data.space_address
                );
                self.request_authoring_list(
                    uri.clone(),
                    data.space_address.clone(),
                    data.agent_id.clone(),
                );
                self.request_gossiping_list(
                    uri.clone(),
                    data.space_address.clone(),
                    data.agent_id.clone(),
                );
                for message in *pending_messages {
                    if let Err(err) = self.handle_message(uri, message.clone(), &data.agent_id) {
                        error!(
                            "Error while handling limbo pending message {:?} for {}: {}",
                            message, uri, err
                        );
                    }
                }
                Ok(())
            } else {
                Err(format!("no agent found in limbo at {} ", uri).into())
            };
        trace!("join done");
        result
    }

    // removes an agent from a space
    fn leave(&self, uri: &Lib3hUri, data: &SpaceData) -> Sim2hResult<()> {
        if let Some(ConnectionState::Joined(space_address, agent_id)) = self.get_connection(uri) {
            if (data.agent_id != agent_id) || (data.space_address != space_address) {
                Err(SPACE_MISMATCH_ERR_STR.into())
            } else {
                self.disconnect(uri);
                Ok(())
            }
        } else {
            Err(format!("no joined agent found at {} ", &uri).into())
        }
    }

    // removes a uri from connection and from spaces
    fn disconnect(&self, uri: &Lib3hUri) {
        trace!("disconnect entered");
        if let Some(ConnectionState::Joined(space_address, agent_id)) =
            self.connection_states.write().remove(uri)
        {
            self.spaces
                .get(&space_address)
                .unwrap()
                .write()
                .remove_agent(&agent_id);
        }
        trace!("disconnect done");
    }

    // get the connection status of an agent
    fn get_connection(&self, uri: &Lib3hUri) -> Option<ConnectionState> {
        let reader = self.connection_states.read();
        reader.get(uri).map(|ca| (*ca).clone())
    }

    // find out if an agent is in a space or not and return its URI
    fn lookup_joined(&self, space_address: &SpaceHash, agent_id: &AgentId) -> Option<Lib3hUri> {
        self.spaces
            .get(space_address)?
            .read()
            .agent_id_to_uri(agent_id)
    }

    // handler for incoming connections
    fn handle_incoming_connect(&self, uri: Lib3hUri) -> Sim2hResult<bool> {
        trace!("handle_incoming_connect entered");
        info!("New connection from {:?}", uri);
        if let Some(_old) = self
            .connection_states
            .write()
            .insert(uri.clone(), ConnectionState::new())
        {
            println!("TODO should remove {}", uri); //TODO
        };
        trace!("handle_incoming_connect done");
        Ok(true)
    }

    // handler for messages sent to sim2h
    fn handle_message(
        &mut self,
        uri: &Lib3hUri,
        message: WireMessage,
        signer: &AgentId,
    ) -> Sim2hResult<()> {
        // TODO: anyway, but especially with this Ping/Pong, mitigate DoS attacks.
        if message == WireMessage::Ping {
            trace!("Ping -> Pong");
            self.send(signer.clone(), uri.clone(), &WireMessage::Pong);
            return Ok(());
        }
        MESSAGE_LOGGER
            .lock()
            .log_in(signer.clone(), uri.clone(), message.clone());
        trace!("handle_message entered");
        let mut agent = self
            .get_connection(uri)
            .ok_or_else(|| format!("no connection for {}", uri))?;

        match agent {
            // if the agent sending the message is in limbo, then the only message
            // allowed is a join message.
            ConnectionState::Limbo(ref mut pending_messages) => {
                if let WireMessage::ClientToLib3h(ClientToLib3h::JoinSpace(data)) = message {
                    if &data.agent_id != signer {
                        return Err(SIGNER_MISMATCH_ERR_STR.into());
                    }
                    self.join(uri, &data)
                } else {
                    // TODO: maybe have some upper limit on the number of messages
                    // we allow to queue before dropping the connections
                    pending_messages.push(message);
                    let _ = self.connection_states.write().insert(uri.clone(), agent);
                    self.send(
                        signer.clone(),
                        uri.clone(),
                        &WireMessage::Err(WireError::MessageWhileInLimbo),
                    );
                    Ok(())
                }
            }

            // if the agent sending the messages has been vetted and is in the space
            // then build a message to be proxied to the correct destination, and forward it
            ConnectionState::Joined(space_address, agent_id) => {
                if &agent_id != signer {
                    return Err(SIGNER_MISMATCH_ERR_STR.into());
                }
                self.handle_joined(uri, &space_address, &agent_id, message)
            }
        }
    }

    fn verify_payload(payload: Opaque) -> Sim2hResult<(AgentId, WireMessage)> {
        let signed_message = SignedWireMessage::try_from(payload)?;
        let result = signed_message.verify().unwrap();
        if !result {
            return Err(VERIFY_FAILED_ERR_STR.into());
        }
        let wire_message = WireMessage::try_from(signed_message.payload)?;
        Ok((signed_message.provenance.source().into(), wire_message))
    }

    // process transport and  incoming messages from it
    pub fn process(&mut self) -> Sim2hResult<()> {
        trace!("process");
        self.num_ticks += 1;
        if self.num_ticks % 60000 == 0 {
            debug!(".");
            self.num_ticks = 0;
        }
        trace!("process transport");
        let (_did_work, messages) = self.stream_manager.process()?;

        trace!("process transport done");
        for transport_message in messages {
            match transport_message {
                StreamEvent::ReceivedData(uri, payload) => {
                    let payload: Opaque = payload.into();
                    match Sim2h::verify_payload(payload.clone()) {
                        Ok((source, wire_message)) => {
                            if let Err(error) =
                                self.handle_message(&uri.into(), wire_message, &source)
                            {
                                error!("Error handling message: {:?}", error);
                            }
                        }
                        Err(error) => error!(
                            "Could not verify payload!\nError: {:?}\nPayload was: {:?}",
                            error, payload
                        ),
                    }
                }
                StreamEvent::IncomingConnectionEstablished(uri) => {
                    if let Err(error) = self.handle_incoming_connect(uri.into()) {
                        error!("Error handling incomming connection: {:?}", error);
                    }
                }
                StreamEvent::ConnectionClosed(uri) => {
                    debug!("Disconnecting {} after connection reset", uri);
                    self.disconnect(&uri.into());
                }
                StreamEvent::ConnectResult(uri, net_id) => {
                    debug!("Connected to {} (net_id = {})", uri, net_id);
                }
                StreamEvent::ErrorOccured(uri, error) => {
                    error!(
                        "Transport error occurred on connection to {}: {:?}",
                        uri, error,
                    );
                }
            }
        }
        trace!("process done");
        Ok(())
    }

    // given an incoming messages, prepare a proxy message and whether it's an publish or request
    fn handle_joined(
        &mut self,
        uri: &Lib3hUri,
        space_address: &SpaceHash,
        agent_id: &AgentId,
        message: WireMessage,
    ) -> Sim2hResult<()> {
        trace!("handle_joined entered");
        debug!(
            "<<IN<< {} from {}",
            message.message_type(),
            agent_id.to_string()
        );
        match message {
            // First make sure we are not receiving a message in the wrong direction.
            // Panic for now so we can easily spot a mistake.
            // Should maybe break up WireMessage into two different structs so we get the
            // error already when parsing an incoming payload.
            WireMessage::Lib3hToClient(_) | WireMessage::ClientToLib3hResponse(_) =>
                panic!("This is soo wrong. Clients should never send a message that only servers can send."),
            // -- Space -- //
            WireMessage::ClientToLib3h(ClientToLib3h::JoinSpace(_)) => {
                Err("join message should have been processed elsewhere and can't be proxied".into())
            }
            WireMessage::ClientToLib3h(ClientToLib3h::LeaveSpace(data)) => {
                self.leave(uri, &data)
            }

            // -- Direct Messaging -- //
            // Send a message directly to another agent on the network
            WireMessage::ClientToLib3h(ClientToLib3h::SendDirectMessage(dm_data)) => {
                if (dm_data.from_agent_id != *agent_id) || (dm_data.space_address != *space_address)
                {
                    return Err(SPACE_MISMATCH_ERR_STR.into());
                }
                let to_url = self
                    .lookup_joined(space_address, &dm_data.to_agent_id)
                    .ok_or_else(|| format!("unvalidated proxy agent {}", &dm_data.to_agent_id))?;
                self.send(
                    dm_data.to_agent_id.clone(),
                    to_url,
                    &WireMessage::Lib3hToClient(Lib3hToClient::HandleSendDirectMessage(dm_data))
                );
                Ok(())
            }
            // Direct message response
            WireMessage::Lib3hToClientResponse(Lib3hToClientResponse::HandleSendDirectMessageResult(
                dm_data,
            )) => {
                if (dm_data.from_agent_id != *agent_id) || (dm_data.space_address != *space_address)
                {
                    return Err(SPACE_MISMATCH_ERR_STR.into());
                }
                let to_url = self
                    .lookup_joined(space_address, &dm_data.to_agent_id)
                    .ok_or_else(|| format!("unvalidated proxy agent {}", &dm_data.to_agent_id))?;
                self.send(
                    dm_data.to_agent_id.clone(),
                    to_url,
                    &WireMessage::Lib3hToClient(Lib3hToClient::SendDirectMessageResult(dm_data))
                );
                Ok(())
            }
            WireMessage::ClientToLib3h(ClientToLib3h::PublishEntry(data)) => {
                if (data.provider_agent_id != *agent_id) || (data.space_address != *space_address) {
                    return Err(SPACE_MISMATCH_ERR_STR.into());
                }
                self.handle_new_entry_data(data.entry, space_address.clone(), agent_id.clone());
                Ok(())
            }
            WireMessage::Lib3hToClientResponse(Lib3hToClientResponse::HandleGetAuthoringEntryListResult(list_data)) => {
                debug!("GOT AUTHORING LIST from {}", agent_id);
                if (list_data.provider_agent_id != *agent_id) || (list_data.space_address != *space_address) {
                    return Err(SPACE_MISMATCH_ERR_STR.into());
                }
                let unseen_aspects = AspectList::from(list_data.address_map)
                    .diff(self.spaces
                        .get(space_address)
                        .expect("This function should not get called if we don't have this space")
                        .read()
                        .all_aspects()
                    );
                debug!("UNSEEN ASPECTS:\n{}", unseen_aspects.pretty_string());
                for entry_address in unseen_aspects.entry_addresses() {
                    if let Some(aspect_address_list) = unseen_aspects.per_entry(entry_address) {
                        let wire_message = WireMessage::Lib3hToClient(
                            Lib3hToClient::HandleFetchEntry(FetchEntryData {
                                request_id: "".into(),
                                space_address: space_address.clone(),
                                provider_agent_id: agent_id.clone(),
                                entry_address: entry_address.clone(),
                                aspect_address_list: Some(aspect_address_list.clone())
                            })
                        );
                        self.send(agent_id.clone(), uri.clone(), &wire_message);
                    }
                }
                Ok(())
            }
            WireMessage::Lib3hToClientResponse(Lib3hToClientResponse::HandleGetGossipingEntryListResult(list_data)) => {
                debug!("GOT GOSSIPING LIST from {}", agent_id);
                if (list_data.provider_agent_id != *agent_id) || (list_data.space_address != *space_address) {
                    return Err(SPACE_MISMATCH_ERR_STR.into());
                }
                let (agents_in_space, aspects_missing_at_node) = {
                    let space = self.spaces
                        .get(space_address)
                        .expect("This function should not get called if we don't have this space")
                        .read();
                    let aspects_missing_at_node = space
                        .all_aspects()
                        .diff(&AspectList::from(list_data.address_map));

                    warn!("MISSING ASPECTS at {}:\n{}", agent_id, aspects_missing_at_node.pretty_string());

                    let agents_in_space = space
                        .all_agents()
                        .keys()
                        .cloned()
                        .collect::<Vec<AgentPubKey>>();
                    (agents_in_space, aspects_missing_at_node)
                };

                if agents_in_space.len() == 1 {
                    error!("MISSING ASPECTS and no way to get them. Agent is alone in space..");
                } else {
                    let other_agents = agents_in_space
                        .into_iter()
                        .filter(|a| a!=agent_id)
                        .collect::<Vec<_>>();

                    let mut rng = rand::thread_rng();
                    let random_agent_index = rng.gen_range(0, other_agents.len());
                    let random_agent = other_agents
                        .get(random_agent_index)
                        .expect("Random generator must work as documented");

                    debug!("FETCHING missing contents from RANDOM AGENT: {}", random_agent);

                    let maybe_url = self.lookup_joined(space_address, random_agent);
                    if maybe_url.is_none() {
                        error!("Could not find URL for randomly selected agent. This should not happen!");
                        return Ok(())
                    }
                    let random_url = maybe_url.unwrap();

                    for entry_address in aspects_missing_at_node.entry_addresses() {
                        if let Some(aspect_address_list) = aspects_missing_at_node.per_entry(entry_address) {
                            let wire_message = WireMessage::Lib3hToClient(
                                Lib3hToClient::HandleFetchEntry(FetchEntryData {
                                    request_id: agent_id.clone().into(),
                                    space_address: space_address.clone(),
                                    provider_agent_id: random_agent.clone(),
                                    entry_address: entry_address.clone(),
                                    aspect_address_list: Some(aspect_address_list.clone())
                                })
                            );
                            debug!("SENDING FeTCH with ReQUest ID: {:?}", wire_message);
                            self.send(random_agent.clone(), random_url.clone(), &wire_message);
                        }
                    }
                }
                Ok(())
            }
            WireMessage::Lib3hToClientResponse(
                Lib3hToClientResponse::HandleFetchEntryResult(fetch_result)) => {
                if (fetch_result.provider_agent_id != *agent_id) || (fetch_result.space_address != *space_address) {
                    return Err(SPACE_MISMATCH_ERR_STR.into());
                }
                debug!("HANDLE FETCH ENTRY RESULT: {:?}", fetch_result);
                if fetch_result.request_id == "" {
                    debug!("Got FetchEntry result form {} without request id - must be from authoring list", agent_id);
                    self.handle_new_entry_data(fetch_result.entry, space_address.clone(), agent_id.clone());
                } else {
                    debug!("Got FetchEntry result with request id {} - this is for gossiping to agent with incomplete data", fetch_result.request_id);
                    let to_agent_id = AgentPubKey::from(fetch_result.request_id);
                    let maybe_url = self.lookup_joined(space_address, &to_agent_id);;
                    if maybe_url.is_none() {
                        error!("Got FetchEntryResult with request id that is not a known agent id. My hack didn't work?");
                        return Ok(())
                    }
                    let url = maybe_url.unwrap();
                    for aspect in fetch_result.entry.aspect_list {
                        let store_message = WireMessage::Lib3hToClient(Lib3hToClient::HandleStoreEntryAspect(
                            StoreEntryAspectData {
                                request_id: "".into(),
                                space_address: space_address.clone(),
                                provider_agent_id: agent_id.clone(),
                                entry_address: fetch_result.entry.entry_address.clone(),
                                entry_aspect: aspect,
                            },
                        ));
                        self.send(to_agent_id.clone(), url.clone(), &store_message);
                    }
                }

                Ok(())
            }
            _ => {
                warn!("Ignoring unimplemented message: {:?}", message );
                Err(format!("Message not implemented: {:?}", message).into())
            }
        }
    }

    fn handle_new_entry_data(
        &mut self,
        entry_data: EntryData,
        space_address: SpaceHash,
        provider: AgentPubKey,
    ) {
        let aspect_addresses = entry_data
            .aspect_list
            .iter()
            .cloned()
            .map(|aspect_data| aspect_data.aspect_address)
            .collect::<Vec<_>>();
        let mut map = HashMap::new();
        map.insert(entry_data.entry_address.clone(), aspect_addresses);
        let aspect_list = AspectList::from(map);
        debug!("GOT NEW ASPECTS:\n{}", aspect_list.pretty_string());

        for aspect in entry_data.aspect_list {
            // 1. Add hashes to our global list of all aspects in this space:
            {
                let mut space = self
                    .spaces
                    .get(&space_address)
                    .expect("This function should not get called if we don't have this space")
                    .write();
                space.add_aspect(
                    entry_data.entry_address.clone(),
                    aspect.aspect_address.clone(),
                );
                debug!(
                    "Space {} now knows about these aspects:\n{}",
                    space_address,
                    space.all_aspects().pretty_string()
                );
            }

            // 2. Create store message
            let store_message = WireMessage::Lib3hToClient(Lib3hToClient::HandleStoreEntryAspect(
                StoreEntryAspectData {
                    request_id: "".into(),
                    space_address: space_address.clone(),
                    provider_agent_id: provider.clone(),
                    entry_address: entry_data.entry_address.clone(),
                    entry_aspect: aspect,
                },
            ));
            // 3. Send store message to everybody in this space
            if let Err(e) = self.broadcast(space_address.clone(), &store_message, Some(&provider)) {
                error!("Error during broadcast: {:?}", e);
            }
        }
    }

    fn broadcast(
        &mut self,
        space: SpaceHash,
        msg: &WireMessage,
        except: Option<&AgentId>,
    ) -> Sim2hResult<()> {
        debug!("Broadcast in space: {:?}", space);
        let all_agents = self
            .spaces
            .get(&space)
            .ok_or("No such space")?
            .read()
            .all_agents()
            .clone()
            .into_iter()
            .filter(|(a, _)| {
                if let Some(exception) = except {
                    *a != *exception
                } else {
                    true
                }
            })
            .collect::<Vec<(AgentId, Lib3hUri)>>();
        for (agent, uri) in all_agents {
            debug!("Broadcast: Sending to {:?}", uri);
            self.send(agent, uri, msg);
        }
        Ok(())
    }

    fn send(&mut self, agent: AgentId, uri: Lib3hUri, msg: &WireMessage) {
        match msg {
            WireMessage::Ping | WireMessage::Pong => debug!("PingPong: {} at {}", agent, uri),
            _ => {
                debug!(">>OUT>> {} to {}", msg.message_type(), uri);
                MESSAGE_LOGGER
                    .lock()
                    .log_out(agent, uri.clone(), msg.clone());
            }
        }
        let payload: Opaque = msg.clone().into();
        let url: url::Url = uri.into();
        let send_result = self
            .stream_manager
            .send(&url, payload.as_bytes().as_slice());

        if let Err(e) = send_result {
            error!("GhostError during broadcast send: {:?}", e)
        }
        match msg {
            WireMessage::Ping | WireMessage::Pong => {}
            _ => debug!("sent."),
        }
    }
}
