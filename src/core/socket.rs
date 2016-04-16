// Copyright 2016 Benoît Labaere (benoit.labaere@gmail.com)
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or http://www.apache.org/licenses/LICENSE-2.0>
// or the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
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
use transport::pipe::Pipe;
use core::acceptor::Acceptor;
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
            error!("[{:?}] failed to send notify to the facade: '{:?}'", self.id, send_res.err());
        } 
    }

    fn send_event(&self, evt: SocketEvtSignal) {
        let evt_sig = EvtSignal::Socket(self.id, evt);
        let loop_sig = EventLoopSignal::Evt(evt_sig);
        let send_res = self.sig_sender.send(loop_sig);

        if send_res.is_err() {
            error!("[{:?}] failed to send event to the session: '{:?}'", self.id, send_res.err());
        } 
    }

    pub fn handle_cmd(&mut self, event_loop: &mut EventLoop, cmd: SocketCmdSignal) {
        debug!("[{:?}] handle_cmd {}", self.id, cmd.name());

        match cmd {
            SocketCmdSignal::Connect(addr)  => self.connect(addr),
            SocketCmdSignal::Bind(addr)     => self.bind(addr),
            SocketCmdSignal::Shutdown(tok)  => self.shutdown(event_loop, tok),
            SocketCmdSignal::SendMsg(msg)   => self.send(event_loop, msg),
            SocketCmdSignal::RecvMsg        => self.recv(event_loop),
            SocketCmdSignal::SetOption(opt) => self.set_option(event_loop, opt)
        }
    }

    pub fn handle_evt(&mut self, event_loop: &mut EventLoop, evt: SocketEvtSignal) {
        debug!("[{:?}] handle_evt {}", self.id, evt.name());
        match evt {
            SocketEvtSignal::PipeAdded(tok)     => self.open_pipe(event_loop, tok),
            SocketEvtSignal::AcceptorAdded(tok) => self.open_acceptor(event_loop, tok),
            _ => {}
        }
    }

    pub fn on_pipe_evt(&mut self, event_loop: &mut EventLoop, tok: mio::Token, evt: PipeEvtSignal) {
        debug!("[{:?}] on_pipe_evt [{:?}]: {}", self.id, tok.as_usize(), evt.name());

        match evt {
            PipeEvtSignal::Error => self.remove_pipe_and_schedule_reconnect(event_loop, tok),
            other => self.protocol.on_pipe_evt(event_loop, tok, other)
        }
    }

    fn open_pipe(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        self.protocol.open_pipe(event_loop, tok)
    }

    fn open_acceptor(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        if let Some(acceptor) = self.acceptors.get_mut(&tok) {
            acceptor.open(event_loop);
            // TODO handle error
        }
    }

    fn connect(&mut self, addr: String) {
        debug!("[{:?}] connect: '{}'", self.id, addr);

        self.create_connection(&addr).
            and_then(|conn| self.on_connection_created(Some(addr), conn, None)).
            map(|t| self.send_notify(SocketNotify::Connected(t))).
            unwrap_or_else(|err| self.send_notify(SocketNotify::NotConnected(err)));
    }

    pub fn reconnect(&mut self, addr: String, event_loop: &mut EventLoop, tok: mio::Token) {
        debug!("[{:?}] pipe [{:?}] reconnect: '{}'", self.id, tok.as_usize(), addr);

        let conn_addr = addr.clone();
        let reconn_addr = addr.clone();
        let _ = self.create_connection(&addr).
            and_then(|conn| self.on_connection_created(Some(conn_addr), conn, Some(tok))).
            map_err(|_| self.schedule_reconnect(event_loop, tok, reconn_addr));
    }

    fn create_connection(&self, addr: &str) -> io::Result<Box<Connection>> {
        debug!("[{:?}] create_connection: '{}'", self.id, addr);

        let addr_parts: Vec<&str> = addr.split("://").collect();
        let scheme = addr_parts[0];
        let specific_addr = addr_parts[1];
        let mut transport = try!(create_transport(scheme));

        transport.set_nodelay(self.options.tcp_nodelay);
        transport.connect(specific_addr)
    }

    fn on_connection_created(&mut self, addr: Option<String>, conn: Box<Connection>, token: Option<mio::Token>) -> io::Result<mio::Token> {
        debug!("[{:?}] on_connection_created: '{:?}'", self.id, addr);
        let token = token.unwrap_or_else(|| self.next_token());
        let socket_type = self.protocol.get_type();
        let priorities = self.options.priorities();
        let sig_sender = self.sig_sender.clone();
        let pipe = Pipe::new(token, addr, socket_type, priorities, conn, sig_sender);

        self.protocol.add_pipe(token, pipe).
            map(|_|self.send_event(SocketEvtSignal::PipeAdded(token))).
            map(|()| token)
    }

    pub fn bind(&mut self, addr: String) {
        debug!("[{:?}] bind: '{}'", self.id, addr);

        self.create_listener(&addr).
            map(|lst| self.on_listener_created(addr, lst, None)).
            map(|t| self.send_notify(SocketNotify::Bound(t))).
            unwrap_or_else(|err| self.send_notify(SocketNotify::NotBound(err)));
    }

    pub fn rebind(&mut self, addr: String, event_loop: &mut EventLoop, tok: mio::Token) {
        debug!("[{:?}] acceptor [{:?}] rebind: '{}'", self.id, tok.as_usize(), addr);

        let _ = self.create_listener(&addr).
            map(|lst| self.on_listener_created(addr.clone(), lst, Some(tok))).
            map_err(|_| self.schedule_rebind(event_loop, tok, addr.clone()));
    }

    fn create_listener(&self, addr: &str) -> io::Result<Box<Listener>> {
        let addr_parts: Vec<&str> = addr.split("://").collect();
        let scheme = addr_parts[0];
        let specific_addr = addr_parts[1];
        let mut transport = try!(create_transport(scheme));
        
        transport.set_nodelay(self.options.tcp_nodelay);
        transport.bind(specific_addr)
    }

    fn on_listener_created(&mut self, addr: String, listener: Box<Listener>, tok: Option<mio::Token>) -> mio::Token {
        let tok = tok.unwrap_or_else(|| self.next_token());
        let acceptor = Acceptor::new(tok, addr, listener);

        self.add_acceptor(tok, acceptor);
        self.send_event(SocketEvtSignal::AcceptorAdded(tok));

        tok
    }

    fn add_acceptor(&mut self, token: mio::Token, acceptor: Acceptor) {
        self.acceptors.insert(token, acceptor);
    }

    fn remove_acceptor(&mut self, token: mio::Token) -> Option<Acceptor> {
        self.acceptors.remove(&token)
    }

    fn shutdown(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        if self.acceptors.contains_key(&tok) {
            self.shutdown_acceptor(event_loop, tok)
        } else {
            self.shutdown_pipe(event_loop, tok)
        }
    }

    fn shutdown_acceptor(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        self.remove_acceptor(tok).map(|mut a| a.close(event_loop));
    }

    fn shutdown_pipe(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        self.protocol.remove_pipe(tok).map(|mut p| p.close(event_loop));
    }

    pub fn ready(&mut self, event_loop: &mut EventLoop, token: mio::Token, events: mio::EventSet) {
        if self.acceptors.contains_key(&token) {
            self.acceptor_ready(event_loop, token, events)
        } else {
            self.pipe_ready(event_loop, token, events)
        }
    }

    fn acceptor_ready(&mut self, event_loop: &mut EventLoop, tok: mio::Token, events: mio::EventSet) {
        debug!("[{:?}] acceptor [{:?}] ready: '{:?}'", self.id, tok.as_usize(), events);

        self.acceptors.get_mut(&tok).unwrap().
            ready(event_loop, events).
            and_then(|conns| self.on_connections_accepted(conns)).
            unwrap_or_else(|e| self.on_acceptor_error(event_loop, tok, e));
    }

    fn pipe_ready(&mut self, event_loop: &mut EventLoop, tok: mio::Token, events: mio::EventSet) {
        debug!("[{:?}] pipe [{:?}] ready: '{:?}'", self.id, tok.as_usize(), events);

        self.protocol.ready(event_loop, tok, events);

        if self.options.is_device_item && self.protocol.can_recv() {
            self.send_event(SocketEvtSignal::Readable);
        }

        if events.is_hup() | events.is_error() {
            self.remove_pipe_and_schedule_reconnect(event_loop, tok);
        }
    }

    fn remove_pipe_and_schedule_reconnect(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        if let Some(mut pipe) = self.protocol.remove_pipe(tok) {
            pipe.close(event_loop);
            if let Some(addr) = pipe.addr() {
                self.schedule_reconnect(event_loop, tok, addr);
            }
        }
    }

    fn schedule_reconnect(&mut self, event_loop: &mut EventLoop, tok: mio::Token, addr: String) {
        let timespan = time::Duration::from_millis(500);
        let _ = event_loop.
            timeout(EventLoopTimeout::Reconnect(tok, addr), timespan).
            map_err(|err| error!("[{:?}] pipe [{:?}] reconnect timeout failed: '{:?}'", self.id, tok.as_usize(), err));
    }

    fn schedule_rebind(&mut self, event_loop: &mut EventLoop, tok: mio::Token, addr: String) {
        let timespan = time::Duration::from_millis(500);
        let _ = event_loop.
            timeout(EventLoopTimeout::Rebind(tok, addr), timespan).
            map_err(|err| error!("[{:?}] acceptor [{:?}] reconnect timeout failed: '{:?}'", self.id, tok.as_usize(), err));
    }

    fn on_connections_accepted(&mut self, conns: Vec<Box<Connection>>) -> io::Result<()> {
        for conn in conns {
            try!(self.on_connection_created(None, conn, None));
        }
        Ok(())
    }

    fn on_acceptor_error(&mut self, event_loop: &mut EventLoop, tok: mio::Token, err: io::Error) {
        debug!("[{:?}] acceptor [{:?}] error: '{:?}'", self.id, tok.as_usize(), err);

        if let Some(mut acceptor) = self.remove_acceptor(tok) {
            acceptor.
                close(event_loop).
                unwrap_or_else(|err| debug!("[{:?}] acceptor [{:?}] error while closing: '{:?}'", self.id, tok, err));
            let timespan = time::Duration::from_millis(500);
            let _ = event_loop.
                timeout(EventLoopTimeout::Rebind(tok, acceptor.addr()), timespan).
                map_err(|err| error!("[{:?}] acceptor [{:?}] reconnect timeout failed: '{:?}'", self.id, tok, err));

        }
    }

    pub fn send(&mut self, event_loop: &mut EventLoop, msg: Message) {
        debug!("[{:?}] send", self.id);

        if let Some(timeout) = self.options.send_timeout_ms {
            event_loop.
                timeout(EventLoopTimeout::CancelSend(self.id), time::Duration::from_millis(timeout)).
                map(|handle| self.protocol.send(event_loop, msg, Some(handle))).
                unwrap_or_else(|err| error!("[{:?}] failed to set timeout on send: '{:?}'", self.id, err));
        } else {
            self.protocol.send(event_loop, msg, None);
        }
    }

    pub fn on_send_timeout(&mut self, event_loop: &mut EventLoop) {
        debug!("[{:?}] on_send_timeout", self.id);
        self.protocol.on_send_timeout(event_loop);
    }

    pub fn recv(&mut self, event_loop: &mut EventLoop) {
        debug!("[{:?}] recv", self.id);

        if let Some(timeout) = self.options.recv_timeout_ms {
            event_loop.
                timeout(EventLoopTimeout::CancelRecv(self.id), time::Duration::from_millis(timeout)).
                map(|handle| self.protocol.recv(event_loop, Some(handle))).
                unwrap_or_else(|err| error!("[{:?}] failed to set timeout on recv: '{:?}'", self.id, err));
        } else {
            self.protocol.recv(event_loop, None);
        }
    }

    pub fn on_recv_timeout(&mut self, event_loop: &mut EventLoop) {
        debug!("[{:?}] on_recv_timeout", self.id);
        self.protocol.on_recv_timeout(event_loop);
    }

    pub fn set_option(&mut self, event_loop: &mut EventLoop, option: SocketOption) {
        let set_res = match option {
            SocketOption::SendTimeout(timeout)   => self.options.set_send_timeout(timeout),
            SocketOption::RecvTimeout(timeout)   => self.options.set_recv_timeout(timeout),
            SocketOption::SendPriority(priority) => self.options.set_send_priority(priority),
            SocketOption::RecvPriority(priority) => self.options.set_recv_priority(priority),
            SocketOption::TcpNoDelay(flag)       => self.options.set_tcp_nodelay(flag),
            SocketOption::DeviceItem(value)      => self.set_device_item(value),
            option                               => self.protocol.set_option(event_loop, option)
        };
        let evt = match set_res {
            Ok(_)  => SocketNotify::OptionSet,
            Err(e) => SocketNotify::OptionNotSet(e)
        };

        self.send_notify(evt);
    }

    fn set_device_item(&mut self, value: bool) -> io::Result<()> {
        self.options.set_device_item(value);
        self.protocol.set_device_item(value)
    }

    pub fn on_survey_timeout(&mut self, event_loop: &mut EventLoop) {
        self.protocol.on_survey_timeout(event_loop);
    }

    pub fn resend(&mut self, event_loop: &mut EventLoop) {
        self.protocol.resend(event_loop);
    }

    pub fn destroy(&mut self, event_loop: &mut EventLoop) {
        let _: Vec<_> = self.acceptors.drain().map(|(_, mut a)| a.close(event_loop)).collect();
        self.protocol.destroy(event_loop);
    }
}

struct SocketImplOptions {
    pub send_timeout_ms: Option<u64>,
    pub recv_timeout_ms: Option<u64>,
    pub send_priority: u8,
    pub recv_priority: u8,
    pub tcp_nodelay: bool,
    pub is_device_item: bool
}

impl SocketImplOptions {
    fn new() -> SocketImplOptions {
        SocketImplOptions {
            send_timeout_ms: None,
            recv_timeout_ms: None,
            send_priority: 8,
            recv_priority: 8,
            tcp_nodelay: false,
            is_device_item: false
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

    fn set_send_priority(&mut self, priority: u8) -> io::Result<()> {
        if is_valid_priority(priority) {
            self.send_priority = priority;
            Ok(())
        } else {
            Err(invalid_data_io_error("Invalid priority"))
        }
    }

    fn priorities(&self) -> (u8, u8) {
        (self.send_priority, self.recv_priority)
    }

    fn set_recv_priority(&mut self, priority: u8) -> io::Result<()> {
        if is_valid_priority(priority) {
            self.recv_priority = priority;
            Ok(())
        } else {
            Err(invalid_data_io_error("Invalid priority"))
        }
    }

    fn set_tcp_nodelay(&mut self, value: bool) -> io::Result<()> {
        self.tcp_nodelay = value;
        Ok(())
    }

    fn set_device_item(&mut self, value: bool) {
        self.is_device_item = value;
    }
}

impl Default for SocketImplOptions {
    fn default() -> Self {
        SocketImplOptions::new()
    }
}

fn is_valid_priority(priority: u8) -> bool {
    priority >= 1 && priority <=16
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