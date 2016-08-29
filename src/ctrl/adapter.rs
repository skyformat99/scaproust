// Copyright 2016 Benoît Labaere (benoit.labaere@gmail.com)
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or http://www.apache.org/licenses/LICENSE-2.0>
// or the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to those terms.

use std::rc::Rc;
use std::collections::HashMap;
use std::io;
use std::time::Duration;

use mio::{Evented, Token, EventSet, PollOpt, EventLoop, Handler, Timeout};

use core::context;
use core::network::Network;
use core::{SocketId, EndpointId, Message};
use transport::Transport;
use transport::endpoint::*;
use transport::pipe;
use transport::acceptor;
use transport::tcp::Tcp;
use ctrl::bus::EventLoopBus;
use ctrl::Signal;
use sequence::Sequence;
use io_error::*;

pub trait Registrar {
    fn register(&mut self, io: &Evented, tok: Token, interest: EventSet, opt: PollOpt) -> io::Result<()>;
    fn reregister(&mut self, io: &Evented, tok: Token, interest: EventSet, opt: PollOpt) -> io::Result<()>;
    fn deregister(&mut self, io: &Evented) -> io::Result<()>;
}

pub trait Timer {
    fn schedule(&mut self, task: context::Schedulable, delay: Duration) -> io::Result<Timeout>;
    fn cancel(&mut self, timeout: &Timeout);
}

pub struct SocketEventLoopContext<'a, 'b> {
    socket_id: SocketId,
    signal_tx: &'a mut EventLoopBus<Signal>,
    endpoints: &'a mut EndpointCollection,
    schedule: &'a mut Schedule,
    timer: &'b mut Timer
}

pub struct EndpointEventLoopContext<'a, 'b> {
    socket_id: SocketId,
    endpoint_id: EndpointId,
    signal_tx: &'a mut EventLoopBus<Signal>,
    registrar: &'b mut Registrar
}

pub struct PipeController {
    socket_id: SocketId,
    endpoint_id: EndpointId,
    pipe: Box<pipe::Pipe>
}

pub struct AcceptorController {
    socket_id: SocketId,
    endpoint_id: EndpointId,
    acceptor: Box<acceptor::Acceptor>
}

pub struct EndpointCollection {
    ids: Sequence,
    pipes: HashMap<EndpointId, PipeController>,
    acceptors: HashMap<EndpointId, AcceptorController>
}

pub struct Schedule {
    ids: Sequence,
    items: HashMap<context::Scheduled, Timeout>
}

impl<T:Handler> Registrar for EventLoop<T> {
    fn register(&mut self, io: &Evented, tok: Token, interest: EventSet, opt: PollOpt) -> io::Result<()> {
        self.register(io, tok, interest, opt)
    }
    fn reregister(&mut self, io: &Evented, tok: Token, interest: EventSet, opt: PollOpt) -> io::Result<()> {
        self.reregister(io, tok, interest, opt)
    }
    fn deregister(&mut self, io: &Evented) -> io::Result<()> {
        self.deregister(io)
    }
}

impl<T:Handler<Timeout=context::Schedulable>> Timer for EventLoop<T> {
    fn schedule(&mut self, task: context::Schedulable, delay: Duration) -> io::Result<Timeout> {
        self.timeout(task, delay).map_err(from_timer_error)
    }
    fn cancel(&mut self, timeout: &Timeout) {
        self.clear_timeout(timeout);
    }
}

/*****************************************************************************/
/*                                                                           */
/* Endpoint collection                                                       */
/*                                                                           */
/*****************************************************************************/

impl PipeController {
    pub fn ready<'a, 'b>(&mut self, registrar: &'a mut Registrar, signal_bus: &'b mut EventLoopBus<Signal>, events: EventSet) {
        let mut ctx = self.create_context(registrar, signal_bus);

        self.pipe.ready(&mut ctx, events);
    }

    pub fn process<'a, 'b>(&mut self, registrar: &'a mut Registrar, signal_bus: &'b mut EventLoopBus<Signal>, cmd: pipe::Command) {
        let mut ctx = self.create_context(registrar, signal_bus);

        match cmd {
            pipe::Command::Open      => self.pipe.open(&mut ctx),
            pipe::Command::Close     => self.pipe.close(&mut ctx),
            pipe::Command::Send(msg) => self.pipe.send(&mut ctx, msg),
            pipe::Command::Recv      => self.pipe.open(&mut ctx)
        }
    }

    fn create_context<'a, 'b>(&self, registrar: &'b mut Registrar, signal_bus: &'a mut EventLoopBus<Signal>) -> EndpointEventLoopContext<'a, 'b> {
        EndpointEventLoopContext {
            socket_id: self.socket_id,
            endpoint_id: self.endpoint_id,
            signal_tx: signal_bus,
            registrar: registrar
        }
    }
}

impl AcceptorController {
    pub fn ready<'a, 'b>(&mut self, registrar: &'a mut Registrar, signal_bus: &'b mut EventLoopBus<Signal>, events: EventSet) {
        let mut ctx = self.create_context(registrar, signal_bus);

        self.acceptor.ready(&mut ctx, events);
    }

    pub fn process<'a, 'b>(&mut self, registrar: &'a mut Registrar, signal_bus: &'b mut EventLoopBus<Signal>, cmd: acceptor::Command) {
        let mut ctx = self.create_context(registrar, signal_bus);

        match cmd {
            acceptor::Command::Open  => self.acceptor.open(&mut ctx),
            acceptor::Command::Close => self.acceptor.close(&mut ctx),
        }
    }

    fn create_context<'a, 'b>(&self, registrar: &'b mut Registrar, signal_bus: &'a mut EventLoopBus<Signal>) -> EndpointEventLoopContext<'a, 'b> {
        EndpointEventLoopContext {
            socket_id: self.socket_id,
            endpoint_id: self.endpoint_id,
            signal_tx: signal_bus,
            registrar: registrar
        }
    }
}

impl EndpointCollection {
    pub fn new(seq: Sequence) -> EndpointCollection {
        EndpointCollection {
            ids: seq,
            pipes: HashMap::new(),
            acceptors: HashMap::new()
        }
    }

    pub fn get_pipe_mut<'a>(&'a mut self, eid: EndpointId) -> Option<&'a mut PipeController> {
        self.pipes.get_mut(&eid)
    }

    pub fn insert_pipe(&mut self, sid: SocketId, pipe: Box<pipe::Pipe>) -> EndpointId {
        let eid = EndpointId::from(self.ids.next());
        
        self.insert_pipe_controller(sid, eid, pipe);

        eid
    }

    fn insert_pipe_controller(&mut self, sid: SocketId, eid: EndpointId, pipe: Box<pipe::Pipe>) {
        let controller = PipeController {
            socket_id: sid,
            endpoint_id: eid,
            pipe: pipe
        };

        self.pipes.insert(eid, controller);
    }

    pub fn remove_pipe(&mut self, eid: EndpointId) {
        self.pipes.remove(&eid);
    }

    pub fn get_acceptor_mut<'a>(&'a mut self, eid: EndpointId) -> Option<&'a mut AcceptorController> {
        self.acceptors.get_mut(&eid)
    }

    fn insert_acceptor(&mut self, sid: SocketId, acceptor: Box<acceptor::Acceptor>) -> EndpointId {
        let eid = EndpointId::from(self.ids.next());

        self.insert_acceptor_controller(sid, eid, acceptor);

        eid
    }

    fn insert_acceptor_controller(&mut self, sid: SocketId, eid: EndpointId, acceptor: Box<acceptor::Acceptor>) {
        let controller = AcceptorController {
            socket_id: sid,
            endpoint_id: eid,
            acceptor: acceptor
        };

        self.acceptors.insert(eid, controller);
    }
}

impl Schedule {
    pub fn new(seq: Sequence) -> Schedule {
        Schedule { 
            ids: seq,
            items: HashMap::new() 
        }
    }
    fn insert(&mut self, handle: Timeout) -> context::Scheduled {
        let scheduled = context::Scheduled::from(self.ids.next()); 
        self.items.insert(scheduled, handle);
        scheduled
    }
    fn remove(&mut self, scheduled: context::Scheduled) -> Option<Timeout> {
        self.items.remove(&scheduled)
    }
}

/*****************************************************************************/
/*                                                                           */
/* Socket context                                                            */
/*                                                                           */
/*****************************************************************************/

impl<'a, 'b> SocketEventLoopContext<'a, 'b> {
    pub fn new(
        sid: SocketId,
        tx: &'a mut EventLoopBus<Signal>,
        eps: &'a mut EndpointCollection,
        sched: &'a mut Schedule,
        timer: &'b mut Timer) -> SocketEventLoopContext<'a, 'b> {
        SocketEventLoopContext {
            socket_id: sid,
            signal_tx: tx,
            endpoints: eps,
            schedule: sched,
            timer: timer
        }
    }

    fn send_signal(&mut self, signal: Signal) {
        self.signal_tx.send(signal);
    }

    fn send_pipe_cmd(&mut self, endpoint_id: EndpointId, cmd: pipe::Command) {
        let signal = Signal::PipeCmd(self.socket_id, endpoint_id, cmd);

        self.send_signal(signal);
    }

    fn send_acceptor_cmd(&mut self, endpoint_id: EndpointId, cmd: acceptor::Command) {
        let signal = Signal::AcceptorCmd(self.socket_id, endpoint_id, cmd);

        self.send_signal(signal);
    }

    fn send_socket_evt(&mut self, evt: context::Event) {
        let signal = Signal::SocketEvt(self.socket_id, evt);

        self.send_signal(signal);
    }

    fn get_transport(&self, scheme: &str) -> io::Result<Box<Transport>> {
        match scheme {
            "tcp" => Ok(Box::new(Tcp)),
            _ => Err(invalid_input_io_error(scheme.to_owned()))
        }
    }

    fn connect(&mut self, url: &str, pids: (u16, u16)) -> io::Result<Box<pipe::Pipe>> {
        let index = match url.find("://") {
            Some(x) => x,
            None => return Err(invalid_input_io_error(url.to_owned()))
        };

        let (scheme, remainder) = url.split_at(index);
        let addr = &remainder[3..];
        let transport = try!(self.get_transport(scheme));

        transport.connect(addr, pids)
    }

    fn bind(&mut self, url: &str, pids: (u16, u16)) -> io::Result<Box<acceptor::Acceptor>> {
        let index = match url.find("://") {
            Some(x) => x,
            None => return Err(invalid_input_io_error(url.to_owned()))
        };

        let (scheme, remainder) = url.split_at(index);
        let addr = &remainder[3..];
        let transport = try!(self.get_transport(scheme));

        transport.bind(addr, pids)
    }
}

impl<'a, 'b> Network for SocketEventLoopContext<'a, 'b> {

    fn connect(&mut self, sid: SocketId, url: &str, pids: (u16, u16)) -> io::Result<EndpointId> {
        let pipe = try!(self.connect(url, pids));
        let eid = self.endpoints.insert_pipe(sid, pipe);

        Ok(eid)
    }
    fn reconnect(&mut self, sid: SocketId, eid: EndpointId, url: &str, pids: (u16, u16)) -> io::Result<()> {
        let pipe = try!(self.connect(url, pids));
        let void = self.endpoints.insert_pipe_controller(sid, eid, pipe);
        
        Ok(void)
    }
    fn bind(&mut self, sid: SocketId, url: &str, pids: (u16, u16)) -> io::Result<EndpointId> {
        let acceptor = try!(self.bind(url, pids));
        let eid = self.endpoints.insert_acceptor(sid, acceptor);

        Ok(eid)
    }
    fn rebind(&mut self, sid: SocketId, eid: EndpointId, url: &str, pids: (u16, u16)) -> io::Result<()> {
        let acceptor = try!(self.bind(url, pids));
        let void = self.endpoints.insert_acceptor_controller(sid, eid, acceptor);

        Ok(void)
    }
    fn open(&mut self, endpoint_id: EndpointId, remote: bool) {
        if remote {
            self.send_pipe_cmd(endpoint_id, pipe::Command::Open);
        } else {
            self.send_acceptor_cmd(endpoint_id, acceptor::Command::Open)
        }
    }
    fn close(&mut self, endpoint_id: EndpointId, remote: bool) {
        if remote {
            self.send_pipe_cmd(endpoint_id, pipe::Command::Close);
        } else {
            self.send_acceptor_cmd(endpoint_id, acceptor::Command::Close)
        }
    }
    fn send(&mut self, endpoint_id: EndpointId, msg: Rc<Message>) {
        self.send_pipe_cmd(endpoint_id, pipe::Command::Send(msg));
    }
    fn recv(&mut self, endpoint_id: EndpointId) {
        self.send_pipe_cmd(endpoint_id, pipe::Command::Recv);
    }

}

impl<'a, 'b> context::Scheduler for SocketEventLoopContext<'a, 'b> {
    fn schedule(&mut self, schedulable: context::Schedulable, delay: Duration) -> io::Result<context::Scheduled> {
        let handle = try!(self.timer.schedule(schedulable, delay));
        let scheduled = self.schedule.insert(handle);
        
        Ok(scheduled)
    }
    fn cancel(&mut self, scheduled: context::Scheduled) {
        if let Some(handle) = self.schedule.remove(scheduled) {
            self.timer.cancel(&handle);
        }
    }
}

impl<'a, 'b> context::Context for SocketEventLoopContext<'a, 'b> {
    fn raise(&mut self, evt: context::Event) {
        self.send_socket_evt(evt);
    }
}

/*****************************************************************************/
/*                                                                           */
/* Endpoint context                                                          */
/*                                                                           */
/*****************************************************************************/

impl<'a, 'b> EndpointRegistrar for EndpointEventLoopContext<'a, 'b> {
    fn register(&mut self, io: &Evented, interest: EventSet, opt: PollOpt) -> io::Result<()> {
        self.registrar.register(io, self.endpoint_id.into(), interest, opt)
    }
    fn reregister(&mut self, io: &Evented, interest: EventSet, opt: PollOpt) -> io::Result<()> {
        self.registrar.reregister(io, self.endpoint_id.into(), interest, opt)
    }
    fn deregister(&mut self, io: &Evented) -> io::Result<()> {
        self.registrar.deregister(io)
    }
}

impl<'a, 'b> pipe::Context for EndpointEventLoopContext<'a, 'b> {

    fn raise(&mut self, evt: pipe::Event) {
        let signal = Signal::PipeEvt(self.socket_id, self.endpoint_id, evt);

        self.signal_tx.send(signal);
    }
}

impl<'a, 'b> acceptor::Context for EndpointEventLoopContext<'a, 'b> {
    fn raise(&mut self, evt: acceptor::Event) {
        let signal = Signal::AcceptorEvt(self.socket_id, self.endpoint_id, evt);

        self.signal_tx.send(signal);
    }
}

impl Into<Token> for EndpointId {
    fn into(self) -> Token {
        Token(self.into())
    }
}

impl<'x> Into<Token> for &'x EndpointId {
    fn into(self) -> Token {
        Token(self.into())
    }
}

impl From<Token> for EndpointId {
    fn from(tok: Token) -> EndpointId {
        EndpointId::from(tok.0)
    }
}
