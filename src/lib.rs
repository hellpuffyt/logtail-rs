//! Core library for `logtail`: a streaming query and aggregation engine for
//! newline-delimited JSON logs.
#![deny(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![deny(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::module_name_repetitions)]

pub mod agg;
pub mod cardinality;
pub mod follow;
pub mod output;
pub mod query;
pub mod record;
pub mod reservoir;
pub mod window;
