// Copyright 2015 Copyright (c) 2015 Benoît Labaere (benoit.labaere@gmail.com)
//
// Licensed under the MIT license LICENSE or <http://opensource.org/licenses/MIT>
// This file may not be copied, modified, or distributed except according to those terms.

use std::rc::Rc;
use std::collections::hash_map::*;
use std::sync::mpsc::Sender;
use std::io;
use std::time;

use mio;

use global::*;
use event_loop_msg::*;

use protocol::Protocol;
use pipe::Pipe;
use acceptor::Acceptor;
use transport::{create_transport, Connection, Listener};

use EventLoop;
use Message;

pub struct Socket {
    id: SocketId,
    protocol: Box<Protocol>,
    evt_sender: Rc<Sender<SocketNotify>>,
    sig_sender: mio::Sender<EventLoopSignal>,
    acceptors: HashMap<mio::Token, Acceptor>,
    id_seq: IdSequence,
    added_tokens: Option<Vec<mio::Token>>, // replace that by a user signal
    removed_tokens: Option<Vec<mio::Token>>, // replace that by a user signal
    options: SocketImplOptions
}

impl Socket {

    pub fn new(
        id: SocketId, 
        proto: Box<Protocol>, 
        evt_tx: Rc<Sender<SocketNotify>>, 
        sig_tx: mio::Sender<EventLoopSignal>,
        id_seq: IdSequence) -> Socket {
        Socket {
            id: id,
            protocol: proto, 
            evt_sender: evt_tx,
            sig_sender: sig_tx,
            acceptors: HashMap::new(),
            id_seq: id_seq,
            added_tokens: None,
            removed_tokens: None,
            options: SocketImplOptions::new()
        }
    }

    fn next_token(&self) -> mio::Token {
        let id = self.id_seq.next();

        mio::Token(id)
    }

    fn send_notify(&self, evt: SocketNotify) {
        let send_res = self.evt_sender.send(evt);

        if send_res.is_err() {
            error!("[{:?}] failed to notify event to session: '{:?}'", self.id, send_res.err());
        } 
    }

    fn send_event(&self, evt: SocketEvtSignal) {
        let evt_sig = EvtSignal::Socket(self.id, evt);
        let loop_sig = EventLoopSignal::Evt(evt_sig);

        self.sig_sender.send(loop_sig);
    }

    pub fn handle_cmd(&mut self, event_loop: &mut EventLoop, cmd: SocketCmdSignal) {
        debug!("[{:?}] handle_cmd", self.id);

        match cmd {
            SocketCmdSignal::Connect(addr)  => self.connect(event_loop, addr),
            //SocketSignal::Connected(tok) => self.on_connected(event_loop),
            /*SocketSignal::Bind(addr)     => self.bind(event_loop, addr),
            SocketSignal::SendMsg(msg)   => self.send(event_loop, msg),
            SocketSignal::RecvMsg        => self.recv(event_loop),
            SocketSignal::SetOption(opt) => self.set_option(event_loop, opt)*/
            x @ _ => {
                error!("[{:?}] handle_cmd: unexpected cmd !", self.id);
            }
        }
    }

    pub fn handle_evt(&mut self, event_loop: &mut EventLoop, evt: SocketEvtSignal) {
        self.protocol.handle_evt(event_loop, evt)
    }

    fn connect(&mut self, event_loop: &mut EventLoop, addr: String) {
        debug!("[{:?}] connect: '{}'", self.id, addr);

        /*{
            debug!("[{:?}] connect: 'get new token'", self.id);
            let token = self.next_token();
            //debug!("[{:?}] connect: 'raise connected signal' '{:?}'", self.id, token);
            //self.send_event(SocketSignal::Connected(token));
            debug!("[{:?}] connect: 'raise connected event'", self.id);
            self.send_notify(SocketNotify::Connected);
            debug!("[{:?}] connect: 'create connection'", self.id);
            let conn = self.create_connection(&addr).unwrap();
            debug!("[{:?}] connect: 'register connection'", self.id);
            let registered = event_loop.register(
                conn.as_evented(),
                token,
                mio::EventSet::all(),
                mio::PollOpt::level());

            debug!("[{:?}] connect: 'check registration result'", self.id);
            registered.unwrap();
        }*/

        self.
            create_connection(&addr).
            map(|conn| self.on_connection_created(Some(addr), conn)).
            map_err(|err| self.on_connection_not_created(err));


        /*let connect_result = self.
            create_connection(&addr).
            and_then(|conn| self.on_connected(Some(addr), event_loop, token, conn));
        let evt = match connect_result {
            Ok(_) => SocketNotify::Connected,
            Err(e) => SocketNotify::NotConnected(e)
        };

        event_loop.
        self.send_notify(evt);*/
    }

    pub fn reconnect(&mut self, addr: String, event_loop: &mut EventLoop, token: mio::Token) {
        debug!("[{:?}] pipe [{:?}] reconnect: '{}'", self.id, token, addr);

        let conn_addr = addr.clone();
        let reconn_addr = addr.clone();

        self.create_connection(&addr).
            and_then(|c| self.on_connected(Some(conn_addr), event_loop, token, c)).
            unwrap_or_else(|_| self.schedule_reconnect(event_loop, token, reconn_addr));
    }

    fn create_connection(&self, addr: &str) -> io::Result<Box<Connection>> {
        debug!("[{:?}] create_connection: '{}'", self.id, addr);

        let addr_parts: Vec<&str> = addr.split("://").collect();
        let scheme = addr_parts[0];
        let specific_addr = addr_parts[1];
        let transport = try!(create_transport(scheme));

        transport.connect(specific_addr)
    }

    fn on_connection_created(&mut self, addr: Option<String>, conn: Box<Connection>) {
        debug!("[{:?}] on_connection_created: '{:?}'", self.id, addr);
        let token = self.next_token();
        let protocol_ids = (self.protocol.id(), self.protocol.peer_id());
        let pipe = Pipe::new(token, addr, protocol_ids, conn);

        self.protocol.add_pipe(token, pipe);

        self.send_notify(SocketNotify::Connected); // move it outside to reuse this function ?
        self.send_event(SocketEvtSignal::Connected(token));
    }

    fn on_connection_not_created(&mut self, err: io::Error) {
        self.send_notify(SocketNotify::NotConnected(err));
    }

    fn on_connected(&mut self, addr: Option<String>, event_loop: &mut EventLoop, token: mio::Token, conn: Box<Connection>) -> io::Result<()> {
        /*let protocol_ids = (self.protocol.id(), self.protocol.peer_id());
        let pipe = Pipe::new(token, addr, protocol_ids, conn);

        pipe.open(event_loop).and_then(|_| Ok(self.protocol.add_pipe(token, pipe)))*/
        Ok(())
    }

    pub fn bind(&mut self, addr: String, event_loop: &mut EventLoop, token: mio::Token) {
        debug!("[{:?}] acceptor [{:?}] bind: '{}'", self.id, token, addr);

        let evt = match self.create_listener(&addr).and_then(|c| self.on_listener_created(addr, event_loop, token, c)) {
            Ok(_) => SocketNotify::Bound,
            Err(e) => SocketNotify::NotBound(e)
        };

        self.send_notify(evt);
    }

    pub fn rebind(&mut self, addr: String, event_loop: &mut EventLoop, token: mio::Token) {
        debug!("[{:?}] acceptor [{:?}] rebind: '{}'", self.id, token, addr);

        self.create_listener(&addr).
            and_then(|c| self.on_listener_created(addr, event_loop, token, c)).
            unwrap_or_else(|e| self.on_acceptor_error(event_loop, token, e));
    }

    fn create_listener(&self, addr: &str) -> io::Result<Box<Listener>> {

        let addr_parts: Vec<&str> = addr.split("://").collect();
        let scheme = addr_parts[0];
        let specific_addr = addr_parts[1];
        let transport = try!(create_transport(scheme));
        
        transport.bind(specific_addr)
    }

    fn on_listener_created(&mut self, addr: String, event_loop: &mut EventLoop, id: mio::Token, listener: Box<Listener>) -> io::Result<()> {
        let mut acceptor = Acceptor::new(id, addr, listener);

        acceptor.open(event_loop).and_then(|_| Ok(self.add_acceptor(id, acceptor)))
    }

    fn add_acceptor(&mut self, token: mio::Token, acceptor: Acceptor) {
        self.acceptors.insert(token, acceptor);
    }

    fn remove_acceptor(&mut self, token: mio::Token) -> Option<Acceptor> {
        self.acceptors.remove(&token)
    }

    pub fn ready(&mut self, event_loop: &mut EventLoop, token: mio::Token, events: mio::EventSet) -> Option<Vec<mio::Token>> {

        if self.acceptors.contains_key(&token) {
            self.acceptor_ready(event_loop, token, events)
        } else {
            self.pipe_ready(event_loop, token, events)
        }

        self.added_tokens.take()
    }

    fn acceptor_ready(&mut self, event_loop: &mut EventLoop, token: mio::Token, events: mio::EventSet) {
        debug!("[{:?}] acceptor [{:?}] ready: '{:?}'", self.id, token, events);

        self.acceptors.get_mut(&token).unwrap().
            ready(event_loop, events).
            and_then(|conns| self.on_connections_accepted(event_loop, conns)).
            unwrap_or_else(|e| self.on_acceptor_error(event_loop, token, e));
    }

    fn pipe_ready(&mut self, event_loop: &mut EventLoop, token: mio::Token, events: mio::EventSet) {
        debug!("[{:?}] pipe [{:?}] ready: '{:?}'", self.id, token, events);

        if events.is_hup() {
            self.remove_pipe_and_schedule_reconnect(event_loop, token);
        } else if events.is_error() {
            self.remove_pipe_and_schedule_reconnect(event_loop, token);
        } else {
            self.protocol.ready(event_loop, token, events);
        }
    }

    fn remove_pipe_and_schedule_reconnect(&mut self, event_loop: &mut EventLoop, token: mio::Token) {
        /*if let Some(pipe) = self.protocol.remove_pipe(token) {
            let _ = pipe.close(event_loop);
            if let Some(addr) = pipe.addr() {
                self.schedule_reconnect(event_loop, token, addr);
            }
        }*/
    }

    fn schedule_reconnect(&mut self, event_loop: &mut EventLoop, token: mio::Token, addr: String) {
        let _ = event_loop.
            timeout_ms(EventLoopTimeout::Reconnect(token, addr), 200).
            map_err(|err| error!("[{:?}] pipe [{:?}] reconnect timeout failed: '{:?}'", self.id, token, err));
    }

    fn on_connections_accepted(&mut self, event_loop: &mut EventLoop, mut conns: Vec<Box<Connection>>) -> io::Result<()> {
        let tokens: Vec<mio::Token> = conns.drain(..).
            map(|conn| self.on_connection_accepted(event_loop, conn)).
            collect();

        self.added_tokens = Some(tokens);

        Ok(())
    }

    fn on_connection_accepted(&mut self, event_loop: &mut EventLoop, conn: Box<Connection>) -> mio::Token {
        let token = mio::Token(self.id_seq.next());

        debug!("[{:?}] on connection accepted: '{:?}'", self.id, token);

        self.
            on_connected(None, event_loop, token, conn).
            map_err(|err| error!("[{:?}] pipe [{:?}] pipe failed when accepted: '{:?}'", self.id, token, err));

        token
    }

    fn on_acceptor_error(&mut self, event_loop: &mut EventLoop, token: mio::Token, err: io::Error) {
        debug!("[{:?}] acceptor [{:?}] error: '{:?}'", self.id, token, err);

        if let Some(mut acceptor) = self.remove_acceptor(token) {
            acceptor.
                close(event_loop).
                unwrap_or_else(|err| debug!("[{:?}] acceptor [{:?}] error while closing: '{:?}'", self.id, token, err));
            let _ = event_loop.
                timeout_ms(EventLoopTimeout::Rebind(token, acceptor.addr()), 200).
                map_err(|err| error!("[{:?}] acceptor [{:?}] reconnect timeout failed: '{:?}'", self.id, token, err));

        }
    }

    pub fn send(&mut self, event_loop: &mut EventLoop, msg: Message) {
        debug!("[{:?}] send", self.id);

        if let Some(timeout) = self.options.send_timeout_ms {
            let _ = event_loop.timeout_ms(EventLoopTimeout::CancelSend(self.id), timeout).
                map(|timeout| self.protocol.send(event_loop, msg, Box::new(move |el: &mut EventLoop| {el.clear_timeout(timeout)}))).
                map_err(|err| error!("[{:?}] failed to set timeout on send: '{:?}'", self.id, err));
        } else {
            self.protocol.send(event_loop, msg, Box::new(move |_: &mut EventLoop| {true}));
        }
    }

    pub fn on_send_timeout(&mut self, event_loop: &mut EventLoop) {
        debug!("[{:?}] on_send_timeout", self.id);
        self.protocol.on_send_timeout(event_loop);
    }

    pub fn recv(&mut self, event_loop: &mut EventLoop) {
        debug!("[{:?}] recv", self.id);

        if let Some(timeout) = self.options.recv_timeout_ms {
            let _ = event_loop.timeout_ms(EventLoopTimeout::CancelRecv(self.id), timeout).
                map(|timeout| self.protocol.recv(event_loop, Box::new(move |el: &mut EventLoop| {el.clear_timeout(timeout)}))).
                map_err(|err| error!("[{:?}] failed to set timeout on recv: '{:?}'", self.id, err));
        } else {
            self.protocol.recv(event_loop, Box::new(move |_: &mut EventLoop| {true}));
        }
    }

    pub fn on_recv_timeout(&mut self, event_loop: &mut EventLoop) {
        debug!("[{:?}] on_recv_timeout", self.id);
        self.protocol.on_recv_timeout(event_loop);
    }

    pub fn set_option(&mut self, event_loop: &mut EventLoop, option: SocketOption) {
        let set_res = match option {
            SocketOption::SendTimeout(timeout) => self.options.set_send_timeout(timeout),
            SocketOption::RecvTimeout(timeout) => self.options.set_recv_timeout(timeout),
            o @ _ => self.protocol.set_option(event_loop, o)
        };
        let evt = match set_res {
            Ok(_)  => SocketNotify::OptionSet,
            Err(e) => SocketNotify::OptionNotSet(e)
        };

        self.send_notify(evt);
    }

    pub fn on_survey_timeout(&mut self, event_loop: &mut EventLoop) {
        self.protocol.on_survey_timeout(event_loop);
    }

    pub fn on_request_timeout(&mut self, event_loop: &mut EventLoop) {
        self.protocol.on_request_timeout(event_loop);
    }
}

struct SocketImplOptions {
    pub send_timeout_ms: Option<u64>,
    pub recv_timeout_ms: Option<u64>
}

impl SocketImplOptions {
    fn new() -> SocketImplOptions {
        SocketImplOptions {
            send_timeout_ms: None,
            recv_timeout_ms: None
        }
    }

    fn set_send_timeout(&mut self, timeout: time::Duration) -> io::Result<()> {
        self.send_timeout_ms = duration_to_timeout(timeout);

        Ok(())
    }

    fn set_recv_timeout(&mut self, timeout: time::Duration) -> io::Result<()> {
        self.recv_timeout_ms = duration_to_timeout(timeout);

        Ok(())
    }
}

fn duration_to_timeout(duration: time::Duration) -> Option<u64> {
    let millis = duration_to_millis(duration);

    if millis == 0u64 {
        None
    } else {
        Some(millis)
    }
}

fn duration_to_millis(duration: time::Duration) -> u64 {
    let millis_from_secs = duration.as_secs() * 1_000;
    let millis_from_nanos = duration.subsec_nanos() as f64 / 1_000_000f64;

    millis_from_secs + millis_from_nanos as u64
}
