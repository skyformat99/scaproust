// Copyright 2016 Benoît Labaere (benoit.labaere@gmail.com)
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or http://www.apache.org/licenses/LICENSE-2.0>
// or the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to those terms.

use std::collections::HashMap;
use std::io;

use mio;

use super::{ Protocol, Timeout };
use super::clear_timeout;
use super::priolist::*;
use pipe::*;
use global::*;
use event_loop_msg::{ SocketNotify };
use EventLoop;
use Message;
use super::with_notify::WithNotify;

pub trait WithFairQueue : WithNotify {
    fn get_pipes<'a>(&'a self) -> &'a HashMap<mio::Token, Pipe>;
    fn get_pipes_mut<'a>(&'a mut self) -> &'a mut HashMap<mio::Token, Pipe>;
    fn get_fair_queue<'a>(&'a self) -> &'a PrioList;
    fn get_fair_queue_mut<'a>(&'a mut self) -> &'a mut PrioList;
    
    fn add_pipe(&mut self, tok: mio::Token, pipe: Pipe) -> io::Result<()> {
        match self.get_pipes_mut().insert(tok, pipe) {
            None    => Ok(()),
            Some(_) => Err(invalid_data_io_error("A pipe has already been added with that token"))
        }
    }

    fn remove_pipe(&mut self, tok: mio::Token) -> Option<Pipe> {
        self.get_fair_queue_mut().remove(&tok);
        self.get_pipes_mut().remove(&tok)
    }

    fn open_pipe(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        self.get_pipe(&tok).map(|p| p.open(event_loop));
    }

    fn on_pipe_opened(&mut self, event_loop: &mut EventLoop, tok: mio::Token) {
        self.get_fair_queue_mut().insert(tok, 8);
        self.get_pipe(&tok).map(|p| p.on_open_ack(event_loop));
    }

    fn get_active_pipe<'a>(&'a mut self) -> Option<&'a mut Pipe> {
        match self.get_fair_queue().get() {
            Some(tok) => self.get_pipe(&tok),
            None      => None
        }
    }

    fn is_active_pipe(&self, tok: mio::Token) -> bool {
        self.get_fair_queue().get() == Some(tok)
    }

    fn advance_pipe(&mut self, event_loop: &mut EventLoop) {
        self.get_active_pipe().map(|p| p.resync_readiness(event_loop));
        self.get_fair_queue_mut().deactivate_and_advance();
    }

    fn get_pipe<'a>(&'a mut self, tok: &mio::Token) -> Option<&'a mut Pipe> {
        self.get_pipes_mut().get_mut(tok)
    }

    fn ready(&mut self, event_loop: &mut EventLoop, tok: mio::Token, events: mio::EventSet) {
        if events.is_readable() {
            self.get_fair_queue_mut().activate(tok);
        }

        self.get_pipe(&tok).map(|p| p.ready(event_loop, events));
    }

    fn recv(&mut self, event_loop: &mut EventLoop) -> bool {
        self.get_active_pipe().map(|p| p.recv(event_loop)).is_some()
    }

    fn on_recv_by_pipe(&mut self, event_loop: &mut EventLoop, msg: Message, timeout: Timeout) {
        self.send_notify(SocketNotify::MsgRecv(msg));
        self.advance_pipe(event_loop);

        clear_timeout(event_loop, timeout);
    }

    fn on_recv_timeout(&mut self, event_loop: &mut EventLoop) {
        let err = io::Error::new(io::ErrorKind::TimedOut, "recv timeout reached");

        self.send_notify(SocketNotify::MsgNotRecv(err));
        self.get_active_pipe().map(|p| p.cancel_recv(event_loop));
        self.advance_pipe(event_loop);
    }
}
