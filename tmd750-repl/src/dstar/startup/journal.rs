//! Durable startup intent and exact page images, separate from runtime traffic.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kenwood_tmd750::radio::programming::PageReplacement;
use kenwood_tmd750::radio::terminal::TerminalPlan;
use kenwood_tmd750::{Identity, MenuFieldSnapshot};
use serde::Serialize;

use crate::capture::{Recorder, create_private_file};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Entry,
    Restore,
}

#[derive(Serialize)]
struct PageImage<'a> {
    address: u32,
    expected: &'a [u8],
    replacement: &'a [u8],
}

impl<'a> From<&'a PageReplacement> for PageImage<'a> {
    fn from(page: &'a PageReplacement) -> Self {
        Self {
            address: page.page().address().as_u32(),
            expected: page.expected(),
            replacement: page.replacement(),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event<'a> {
    Backup {
        model: String,
        firmware: String,
        radio_type: String,
        pages: Vec<(u32, &'a [u8])>,
    },
    Planned {
        active_pm: u8,
        gateway_before: u8,
        route_before: String,
        route_after: String,
        pages: Vec<PageImage<'a>>,
    },
    BeforeWrite {
        phase: Phase,
        page: PageImage<'a>,
    },
    Checkpoint {
        phase: Phase,
        acknowledged_exit: bool,
        possibly_written: Vec<u32>,
        verified: Vec<u32>,
    },
}

pub(super) struct Journal {
    recorder: Recorder<File>,
    /// A durable before-write intent exists; radio delivery remains independent.
    pub(super) write_started: bool,
    /// Entry returned E/ACK; later publication failure never erases that outcome.
    pub(super) entry_exit_acknowledged: bool,
    entry_plan: Option<TerminalPlan>,
}

impl Journal {
    pub(super) fn create(directory: &Path, cancelled: Arc<AtomicBool>) -> io::Result<Self> {
        Ok(Self::new(Recorder::named(
            create_private_file(&directory.join("journal.jsonl"))?,
            cancelled,
            "journal.jsonl",
        )))
    }

    pub(super) const fn new(recorder: Recorder<File>) -> Self {
        Self {
            recorder,
            write_started: false,
            entry_exit_acknowledged: false,
            entry_plan: None,
        }
    }

    /// Keep the exact entry images even if later protocol or journal work fails.
    pub(super) const fn entry_plan(&self) -> Option<&TerminalPlan> {
        self.entry_plan.as_ref()
    }

    pub(super) fn backup(
        &mut self,
        identity: &Identity,
        snapshot: &MenuFieldSnapshot,
    ) -> io::Result<()> {
        self.recorder.record(Event::Backup {
            model: identity.model.to_string(),
            firmware: identity.firmware.to_string(),
            radio_type: identity.radio_type.to_string(),
            pages: snapshot
                .pages()
                .iter()
                .map(|(page, bytes)| (page.address().as_u32(), bytes.as_slice()))
                .collect(),
        });
        self.recorder.synchronize()
    }

    pub(super) fn planned(&mut self, plan: &TerminalPlan) -> io::Result<()> {
        if self.entry_plan.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the entry plan is already retained and cannot be replaced",
            ));
        }
        self.entry_plan = Some(plan.clone());
        self.recorder.record(Event::Planned {
            active_pm: plan.slot().index(),
            gateway_before: plan.before().into(),
            route_before: plan.route().to_string(),
            route_after: plan.target_route().to_string(),
            pages: plan.replacements().iter().map(Into::into).collect(),
        });
        self.recorder.synchronize()
    }

    pub(super) fn before_write(&mut self, phase: Phase, page: &PageReplacement) -> io::Result<()> {
        self.recorder.record(Event::BeforeWrite {
            phase,
            page: page.into(),
        });
        self.recorder.synchronize()?;
        self.write_started = true;
        Ok(())
    }

    pub(super) fn checkpoint(
        &mut self,
        phase: Phase,
        acknowledged_exit: bool,
        journal: &kenwood_tmd750::radio::programming::McpJournal,
    ) -> io::Result<()> {
        self.entry_exit_acknowledged |= matches!(phase, Phase::Entry) && acknowledged_exit;
        self.recorder.record(Event::Checkpoint {
            phase,
            acknowledged_exit,
            possibly_written: journal
                .possibly_written
                .iter()
                .map(|page| page.address().as_u32())
                .collect(),
            verified: journal
                .verified
                .iter()
                .map(|page| page.address().as_u32())
                .collect(),
        });
        self.recorder.synchronize()
    }

    pub(super) fn synchronize(&mut self) -> io::Result<()> {
        self.recorder.synchronize()
    }
}
