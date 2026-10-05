//! Exclusive delegated draws. A failed or cancelled draw reaps the writer before
//! its reservation can be reused; exports stay charged until unmap acknowledgement.
use std::{os::fd::AsFd, time::Duration};

use crate::{
    Result,
    helper::{CommandSender, Helper, PendingCommand},
    protocol::{Ack, AckOutcome, Header},
};
use jackstay::acquisition::arena::{CpuReservation, WriterExport};

pub(crate) struct Mapping {
    pub export: WriterExport,
    sender: CommandSender,
    active: bool,
}
impl Mapping {
    pub fn install(helper: &Helper, export: WriterExport) -> Result<Self> {
        let sender = helper.command_sender();
        let mapping = Self {
            export,
            sender,
            active: true,
        };
        // SAFETY: mapping retains the export until a replacement unmap ack or
        // verified helper exit. Draw owns the exclusive reserved-slot protocol.
        let object = unsafe { mapping.export.duplicate_object()? };
        let command = mapping.sender.send_with_fd(
            serde_json::json!({
                "type": "arena", "layout": mapping.export.descriptor(),
            }),
            Duration::from_secs(5),
            Some(object.as_fd()),
        )?;
        if !command.wait_ack().is_some_and(|ack| {
            ack.outcome == AckOutcome::Executed
                && ack.generation == Some(mapping.export.descriptor().generation)
        }) {
            return Err(helper.take_error().map_or_else(
                || "renderer arena mapping failed or timed out".into(),
                Into::into,
            ));
        }
        Ok(mapping)
    }
    /// Unmap before resizing: keeping this export charged can otherwise prevent
    /// a capacity-paused replacement from ever obtaining its memory budget.
    pub fn release(mut self) -> Result<()> {
        let generation = self.export.descriptor().generation;
        let ack = self
            .sender
            .send_json_command(
                serde_json::json!({
                    "type":"arena_release", "generation":generation,
                }),
                Duration::from_secs(5),
            )?
            .wait_ack();
        if !ack.is_some_and(|ack| {
            ack.outcome == AckOutcome::Executed && ack.generation == Some(generation)
        }) {
            return Err("renderer arena release failed or timed out".into());
        }
        self.active = false;
        Ok(())
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        if self.active {
            self.sender.terminate();
        }
    }
}

pub struct Draw {
    pub(crate) command: Option<PendingCommand>,
    pub(crate) reservation: Option<CpuReservation>,
    pub(crate) header: Header,
    sender: CommandSender,
}
impl Draw {
    pub(crate) fn start(
        helper: &Helper,
        reservation: CpuReservation,
        header: Header,
        timeout: Duration,
    ) -> Result<Self> {
        let sender = helper.command_sender();
        let slot = reservation.slot();
        // Arm before writing: a partial send is also a potentially live delegate.
        let mut draw = Self {
            command: None,
            reservation: Some(reservation),
            header,
            sender,
        };
        draw.command = Some(draw.sender.send_json_command(
            serde_json::json!({
                "type":"draw", "slot":slot.slot, "generation":slot.generation,
                "width":draw.header.width, "height":draw.header.height, "stride":draw.header.stride,
            }),
            timeout,
        )?);
        Ok(draw)
    }
    pub fn remaining(&self) -> Duration {
        self.command.as_ref().unwrap().remaining()
    }
    pub fn poll(&self) -> Option<Ack> {
        self.command.as_ref().unwrap().poll()
    }
    pub fn expired(&self) -> bool {
        self.command.as_ref().unwrap().expired()
    }
    pub(crate) fn validate(&self, ack: &Ack) -> Result<()> {
        let slot = self.reservation.as_ref().unwrap().slot();
        if ack.id != self.command.as_ref().unwrap().id() {
            return Err("renderer draw id mismatch".into());
        }
        if ack.generation != Some(slot.generation) || ack.slot != Some(slot.slot) {
            return Err("renderer draw generation or slot mismatch".into());
        }
        if ack.outcome != AckOutcome::Executed {
            return Err(format!(
                "renderer capture failed: {}",
                ack.detail.as_deref().unwrap_or("no diagnostic")
            )
            .into());
        }
        if ack.capture.as_ref().is_some_and(|r| r.published)
            && ack.frame.as_ref() != Some(&self.header)
        {
            return Err("renderer draw frame header mismatch".into());
        }
        if ack.frame.is_some() && !ack.capture.as_ref().is_some_and(|r| r.published) {
            return Err("renderer draw frame without publication report".into());
        }
        Ok(())
    }
}
impl Drop for Draw {
    fn drop(&mut self) {
        if self.reservation.is_some() {
            self.sender.terminate();
        }
    }
}
