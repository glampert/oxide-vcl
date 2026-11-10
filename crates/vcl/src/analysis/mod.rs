//! Program analyses over the source IR: component-level data flow, register
//! webs, liveness pruning, copy and induction recognition, control-flow
//! predicates, status-flag resources and memory dependencies.

pub mod control;
pub mod copies;
pub mod flow;
pub mod induction;
pub mod memory;
pub mod prune;
pub mod status;
pub mod web;
