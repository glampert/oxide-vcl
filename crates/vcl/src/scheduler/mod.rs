//! Linear scheduling: strategy search, the table wrangler, issue-row timing
//! and the clean-up passes that shrink scheduled tables.

pub mod boundary;
pub mod linear_cleanup;
pub mod raw_cleanup;
pub mod schedule;
pub mod timing;
pub mod wrangle;
