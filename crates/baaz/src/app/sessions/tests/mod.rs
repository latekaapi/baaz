//! The session lifecycle's tests, split by the module they exercise. The
//! shared harnesses and factories are [`support`].

use super::*;
use super::{list::*, open::*, steps::*};
use support::*;

pub(in crate::app) mod support;
mod list;
mod open;
mod provider_open;
mod switch;
mod events;
mod reopen;
mod handoff;
mod steps;
