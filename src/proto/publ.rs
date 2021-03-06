// Copyright (c) 2015-2017 Contributors as noted in the AUTHORS file.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or http://www.apache.org/licenses/LICENSE-2.0>
// or the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to those terms.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use core::{EndpointId, Message};
use core::socket::{Protocol, Reply};
use core::endpoint::Pipe;
use core::context::{Context, Event};
use super::pipes::PipeCollection;
use super::{Timeout, PUB, SUB};
use super::policy::broadcast;
use io_error::*;

pub struct Pub {
    reply_tx: Sender<Reply>,
    pipes: PipeCollection,
    bc: HashSet<EndpointId>
}

/*****************************************************************************/
/*                                                                           */
/* Pub                                                                       */
/*                                                                           */
/*****************************************************************************/

impl From<Sender<Reply>> for Pub {
    fn from(tx: Sender<Reply>) -> Pub {
        Pub {
            reply_tx: tx,
            pipes: PipeCollection::new(),
            bc: HashSet::new()
        }
    }
}

/*****************************************************************************/
/*                                                                           */
/* Protocol                                                                  */
/*                                                                           */
/*****************************************************************************/

impl Protocol for Pub {
    fn id(&self)      -> u16 { PUB }
    fn peer_id(&self) -> u16 { SUB }

    fn add_pipe(&mut self, _: &mut Context, eid: EndpointId, pipe: Pipe) {
        self.pipes.insert(eid, pipe);
    }
    fn remove_pipe(&mut self, ctx: &mut Context, eid: EndpointId) -> Option<Pipe> {
        self.bc.remove(&eid);
        if self.bc.is_empty() {
            ctx.raise(Event::CanSend(false));
        }
        self.pipes.remove(&eid)
    }
    fn send(&mut self, ctx: &mut Context, msg: Message, timeout: Timeout) {
        let msg = Rc::new(msg);

        broadcast::send_to_all(&mut self.bc, &mut self.pipes, ctx, msg);
        ctx.raise(Event::CanSend(false));

        let _ = self.reply_tx.send(Reply::Send);
        if let Some(sched) = timeout {
            ctx.cancel(sched);
        }
    }
    fn on_send_ack(&mut self, _: &mut Context, _: EndpointId) {
    }
    fn on_send_timeout(&mut self, _: &mut Context) {
    }
    fn on_send_ready(&mut self, ctx: &mut Context, eid: EndpointId) {
        if self.bc.is_empty() {
            ctx.raise(Event::CanSend(true));
        }
        self.bc.insert(eid);
    }
    fn recv(&mut self, ctx: &mut Context, timeout: Timeout) {
        let error = other_io_error("Recv is not supported by pub protocol");
        let _ = self.reply_tx.send(Reply::Err(error));
        if let Some(sched) = timeout {
            ctx.cancel(sched);
        }
    }
    fn on_recv_ack(&mut self, _: &mut Context, _: EndpointId, _: Message) {
    }
    fn on_recv_timeout(&mut self, _: &mut Context) {
    }
    fn on_recv_ready(&mut self, _: &mut Context, _: EndpointId) {
    }
    fn is_send_ready(&self) -> bool {
        !self.bc.is_empty()
    }
    fn is_recv_ready(&self) -> bool {
        false
    }
    fn close(&mut self, ctx: &mut Context) {
        self.pipes.close_all(ctx)
    }
}
